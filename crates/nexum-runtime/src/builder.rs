//! Type-state runtime builder and the imperative launcher it drives.
//!
//! [`RuntimeBuilder`] accumulates the assembly (config, the [`RuntimeTypes`]
//! lattice, extensions, the component builders, add-ons) through a type-state
//! chain; [`ReadyBuilder::launch`] opens the backends and hands off to
//! [`LaunchRuntime::launch`]. The launcher runs one imperative sequence -
//! install add-ons, build the engine and linker, boot the supervisor, open
//! subscriptions through the [`TaskManager`]'s executor, spawn the event
//! loop - and returns a [`RuntimeHandle`] owning the manager and the
//! running tasks.
//!
//! The reference binary reaches this through its `run_from_config` one-liner;
//! an embedder holding pre-built backends constructs an [`AssembledRuntime`]
//! and calls [`LaunchRuntime::launch`] directly. For the common case,
//! [`RuntimeBuilder::runtime`] binds a [`Runtime`] preset that bundles the
//! lattice, component builders, and add-ons in one call.

use std::future::{Future, IntoFuture};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use nexum_tasks::{DrainOutcome, TaskExit, TaskHandle, TaskManager, TaskSet};
use tracing::{error, info, warn};
use wasmtime::Engine;

use crate::addons::{AddOnHandle, AddOnsContext, RuntimeAddOn};
use crate::engine_config::EngineConfig;
use crate::host::component::{
    BuilderContext, ComponentBuilder, Components, ComponentsBuilder, RuntimeTypes,
};
use crate::host::extension::Extension;
use crate::host::logs::LogPipeline;
use crate::preset::Runtime;
use crate::runtime::event_loop;
pub use crate::supervisor::WasiClockOverride;
use crate::supervisor::{self, Supervisor};

/// Ambient inputs the imperative launcher reads: the task manager every
/// runtime task spawns through, and the loaded config.
pub struct LaunchContext<'a> {
    /// Owns task spawning and graceful shutdown for the run.
    pub tasks: TaskManager,
    /// The loaded engine config.
    pub config: &'a EngineConfig,
}

/// Upper bound on how long the top level blocks for the event loop's final
/// durable flush after shutdown is signalled before forcing exit.
const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

/// A running runtime: the event-loop task, the task manager, and add-on
/// handles. [`shutdown`](Self::shutdown) or dropping fires shutdown;
/// [`wait`](Self::wait) blocks on the bounded drain.
pub struct RuntimeHandle {
    event_loop: TaskHandle<TaskExit>,
    tasks: TaskManager,
    logs: LogPipeline,
    // Held for the length of the run; dropped once the event loop has joined.
    _add_ons: Vec<AddOnHandle>,
}

impl RuntimeHandle {
    /// Signal the event loop to stop. The in-flight dispatch finishes first.
    pub fn shutdown(&mut self) {
        self.tasks.trigger().fire();
    }

    /// The shared log pipeline: the read side for module runs and log pages.
    /// Clone it to keep reading after [`wait`](Self::wait) consumes the handle.
    pub fn logs(&self) -> &LogPipeline {
        &self.logs
    }

    /// Block until the loop stops (on its own, on shutdown, or on a critical
    /// task ending), bounding the final durable flush; a drain past the
    /// timeout forces exit. A `None` join reason means the task panicked or
    /// was aborted.
    pub async fn wait(self) -> anyhow::Result<()> {
        let RuntimeHandle {
            event_loop,
            mut tasks,
            _add_ons,
            ..
        } = self;
        let mut signal = tasks.subscribe();
        let join = event_loop.join();
        tokio::pin!(join);
        tokio::select! {
            biased;
            joined = &mut join => return finish_wait(joined),
            name = tasks.on_critical_failure() => {
                warn!(task = %name, "critical task ended, draining");
            }
            () = signal.recv() => {}
        }
        // Signalled: block on the bounded drain. The event-loop task holds
        // the flush guard until it returns, not the abort-only reconnect
        // pumps.
        match tasks
            .graceful_shutdown_with_timeout(SHUTDOWN_DRAIN_TIMEOUT)
            .await
        {
            DrainOutcome::Drained => finish_wait(join.await),
            DrainOutcome::TimedOut { outstanding } => {
                error!(
                    outstanding,
                    timeout = ?SHUTDOWN_DRAIN_TIMEOUT,
                    "shutdown drain exceeded deadline, forcing exit"
                );
                std::process::exit(1);
            }
        }
    }
}

/// Map an event-loop join outcome to the [`wait`](RuntimeHandle::wait) result.
fn finish_wait(joined: Option<TaskExit>) -> anyhow::Result<()> {
    match joined {
        Some(_) => Ok(()),
        None => anyhow::bail!("event loop task terminated abnormally"),
    }
}

/// A fully-assembled runtime: concrete backends, extensions, add-ons, and the
/// optional module-source override. Implements [`LaunchRuntime`].
pub struct AssembledRuntime<'a, T: RuntimeTypes> {
    /// Shared backends threaded into every module store.
    pub components: Components<T>,
    /// Extensions: namespaces, capabilities, linker hooks, services, and
    /// provider kinds.
    pub extensions: Vec<Arc<dyn Extension<T>>>,
    /// Cross-cutting facilities installed before the engine boots.
    pub add_ons: &'a [&'a dyn RuntimeAddOn],
    /// Single-module source override; `None` runs `[[modules]]`.
    pub wasm: Option<&'a Path>,
    /// Manifest paired with `wasm`.
    pub manifest: Option<&'a Path>,
    /// Per-store WASI clock override; `None` leaves the ambient host clocks.
    pub clocks: Option<WasiClockOverride>,
}

/// An assembled runtime launchable from a [`LaunchContext`].
pub trait LaunchRuntime {
    /// Run the imperative launch sequence and return the running handle.
    fn launch(self, ctx: LaunchContext<'_>) -> impl Future<Output = anyhow::Result<RuntimeHandle>>;
}

impl<T: RuntimeTypes> LaunchRuntime for AssembledRuntime<'_, T> {
    async fn launch(self, ctx: LaunchContext<'_>) -> anyhow::Result<RuntimeHandle> {
        let AssembledRuntime {
            components,
            extensions,
            add_ons,
            wasm,
            manifest,
            clocks,
        } = self;
        let LaunchContext {
            tasks,
            config: engine_cfg,
        } = ctx;

        // Install cross-cutting add-ons before the engine boots so any metric
        // recorder is live for the whole run. The handles move into the
        // returned handle and drop once the event loop joins.
        let addons_ctx = AddOnsContext {
            metrics: &engine_cfg.engine.metrics,
        };
        let add_on_handles = add_ons
            .iter()
            .map(|add_on| add_on.install(&addons_ctx))
            .collect::<anyhow::Result<Vec<_>>>()?;

        // wasmtime engine + linker - one of each, shared across modules.
        let mut config = wasmtime::Config::new();
        config.wasm_component_model(true);
        config.consume_fuel(true);
        let engine = Engine::new(&config)?;
        let linker = supervisor::build_linker::<T>(&engine, &extensions)?;

        // Boot supervisor - a module-source override wins over
        // `engine.toml.[[modules]]`.
        let wasm_override = wasm.is_some();
        let supervisor = if let Some(wasm) = wasm {
            if !engine_cfg.modules.is_empty() {
                warn!(
                    "ignoring engine.toml [[modules]] because a module source override was given"
                );
            }
            Supervisor::boot_single(
                &engine,
                &linker,
                wasm,
                manifest,
                &components,
                &engine_cfg.limits,
                &extensions,
                clocks,
            )
            .await?
        } else if !engine_cfg.modules.is_empty() || !engine_cfg.adapters.is_empty() {
            Supervisor::boot(
                &engine,
                &linker,
                engine_cfg,
                &components,
                &extensions,
                clocks,
            )
            .await?
        } else {
            anyhow::bail!(
                "no modules to run - set a module source or declare [[modules]] or \
                 [[adapters]] entries in engine.toml"
            );
        };

        let alive = supervisor.alive_count();
        let block_chains = supervisor.block_chains();
        info!(
            modules = supervisor.module_count(),
            adapters = supervisor.adapter_count(),
            alive,
            chains = block_chains.len(),
            "supervisor ready"
        );
        if alive == 0 {
            if wasm_override {
                anyhow::bail!(
                    "all {} module(s) failed initialisation - check the logs above for \
                     per-module errors and fix the wasm binary passed as an override",
                    supervisor.module_count(),
                );
            } else {
                anyhow::bail!(
                    "all {} module(s) failed initialisation - check the logs above for \
                     per-module errors and fix or remove the failing module from engine.toml",
                    supervisor.module_count(),
                );
            }
        }

        // The OS signal listener: SIGINT/SIGTERM ends it, and its end (or
        // panic) fires the shutdown signal via the critical-task path. It
        // also watches the signal itself so a programmatic shutdown or a
        // handle drop winds it down rather than leaking it.
        let executor = tasks.executor();
        let mut listener_signal = tasks.subscribe();
        let mut fallback_signal = tasks.subscribe();
        executor.spawn_critical("os-signal-listener", async move {
            tokio::select! {
                res = event_loop::wait_for_shutdown_signal() => match res {
                    Ok(name) => info!(signal = %name, "shutdown signal received"),
                    Err(err) => {
                        warn!(error = %err, "signal handler failed - programmatic shutdown only");
                        fallback_signal.recv().await;
                    }
                },
                () = listener_signal.recv() => {}
            }
        });

        // The handle keeps the log read side reachable after launch consumes
        // the components.
        let logs = components.logs.clone();
        let chain_log_subs = supervisor.chain_log_subscriptions();
        // Status polling runs only when it can produce something a module
        // will see: at least one intent-status subscriber and at least one
        // installed adapter to poll.
        let poll_statuses = supervisor.has_intent_status_subscribers()
            && supervisor.venue_registry().venue_count() > 0;

        // No subscriptions: nothing to drive. Return a handle whose event loop
        // is already complete so `wait` resolves immediately.
        if block_chains.is_empty() && chain_log_subs.is_empty() && !poll_statuses {
            if supervisor.dead_modules_hold_subscriptions() {
                anyhow::bail!(
                    "every declared [[subscription]] belongs to an init-failed module - \
                     the engine would idle with nothing to run; fix or remove the \
                     failing module(s)"
                );
            }
            info!("no [[subscription]] entries - engine has nothing to run; exiting");
            let event_loop = executor.spawn(async { TaskExit::ReceiverGone });
            return Ok(RuntimeHandle {
                event_loop,
                tasks,
                logs,
                _add_ons: add_on_handles,
            });
        }

        // Open per-chain block subscriptions + per-module chain-log
        // subscriptions through the executor, then drive them in the event
        // loop until shutdown.
        let mut reconnect_tasks = TaskSet::new();
        let block_streams = event_loop::open_block_streams(
            &components.chain,
            &block_chains,
            &executor,
            &mut reconnect_tasks,
        );
        let chain_log_streams = event_loop::open_chain_log_streams(
            &components.chain,
            chain_log_subs,
            &executor,
            &mut reconnect_tasks,
        );
        let intent_status_stream = poll_statuses.then(|| {
            event_loop::open_intent_status_stream(
                supervisor.venue_registry(),
                engine_cfg.limits.status_poll_interval(),
                &executor,
                &mut reconnect_tasks,
            )
        });

        // The event-loop task holds the graceful guard until `run` returns
        // (after its final dispatch and cursor commit); shutdown ends the
        // loop between dispatches rather than cancelling it, so the drain
        // blocks on it.
        let event_loop = executor.spawn_graceful(move |graceful| async move {
            let mut supervisor = supervisor; // rebind as mut: the dispatch calls below take &mut self
            event_loop::run(
                &mut supervisor,
                block_streams,
                chain_log_streams,
                intent_status_stream,
                reconnect_tasks,
                graceful.into_future(),
            )
            .await;
            info!("done");
            TaskExit::ReceiverGone
        });

        Ok(RuntimeHandle {
            event_loop,
            tasks,
            logs,
            _add_ons: add_on_handles,
        })
    }
}

/// Entry stage of the type-state runtime builder: only the config is bound.
pub struct RuntimeBuilder<'a> {
    config: &'a EngineConfig,
}

impl<'a> RuntimeBuilder<'a> {
    /// Start a builder over a loaded config.
    pub fn new(config: &'a EngineConfig) -> Self {
        Self { config }
    }

    /// Bind the [`RuntimeTypes`] lattice.
    pub fn with_types<T: RuntimeTypes>(self) -> TypedBuilder<'a, T> {
        TypedBuilder {
            config: self.config,
            extensions: Vec::new(),
            wasm: None,
            manifest: None,
            clocks: None,
            _t: PhantomData,
        }
    }

    /// Bind a [`Runtime`] preset that bundles the lattice, the component
    /// builders, and the add-on set. Sugar over the type-state chain: an
    /// embedder writes `RuntimeBuilder::new(cfg).runtime::<Preset>().launch()`.
    pub fn runtime<R: Runtime>(self) -> PresetBuilder<'a, R> {
        PresetBuilder {
            config: self.config,
            extensions: Vec::new(),
            wasm: None,
            manifest: None,
            clocks: None,
            _r: PhantomData,
        }
    }
}

/// Terminal stage of the preset shortcut: the [`Runtime`] preset supplies the
/// lattice, the component builders, and the add-on set, leaving only the
/// optional extension hooks and module source before [`launch`](Self::launch).
pub struct PresetBuilder<'a, R: Runtime> {
    config: &'a EngineConfig,
    extensions: Vec<Arc<dyn Extension<R::Types>>>,
    wasm: Option<PathBuf>,
    manifest: Option<PathBuf>,
    clocks: Option<WasiClockOverride>,
    _r: PhantomData<fn() -> R>,
}

impl<'a, R: Runtime> PresetBuilder<'a, R> {
    /// Add extensions on top of the preset. The default preset carries none.
    pub fn with_extensions(
        mut self,
        extensions: impl IntoIterator<Item = Arc<dyn Extension<R::Types>>>,
    ) -> Self {
        self.extensions.extend(extensions);
        self
    }

    /// Set the single-module source override, taking precedence over engine.toml
    /// `[[modules]]`. Both `None` runs the configured modules.
    pub fn with_module_source(mut self, wasm: Option<PathBuf>, manifest: Option<PathBuf>) -> Self {
        self.wasm = wasm;
        self.manifest = manifest;
        self
    }

    /// Override the per-store WASI wall and monotonic clocks. Every module
    /// store, including the ones rebuilt on restart, reads these instead of
    /// the ambient host clocks. Omitting it is behaviour-neutral.
    pub fn with_wasi_clocks(mut self, clocks: WasiClockOverride) -> Self {
        self.clocks = Some(clocks);
        self
    }

    /// Open the preset's backends and launch. Builds the [`Components`] bundle
    /// from the preset's component builders, installs the preset's add-ons,
    /// then drives [`LaunchRuntime::launch`] with a fresh [`TaskManager`].
    pub async fn launch(self) -> anyhow::Result<RuntimeHandle> {
        let tasks = TaskManager::new();
        let executor = tasks.executor();
        let data_dir = self.config.engine.state_dir.clone();
        let build_ctx = BuilderContext {
            config: self.config,
            data_dir: &data_dir,
            executor: &executor,
        };
        let components = R::components().build::<R::Types>(&build_ctx).await?;

        // `add_ons` owns the boxed add-ons; `add_on_refs` borrows into it and is
        // consumed by the launch call, so both must stay in scope for that call.
        let add_ons = R::add_ons();
        let add_on_refs: Vec<&dyn RuntimeAddOn> = add_ons.iter().map(|a| &**a).collect();

        let runtime = AssembledRuntime {
            components,
            extensions: self.extensions,
            add_ons: &add_on_refs,
            wasm: self.wasm.as_deref(),
            manifest: self.manifest.as_deref(),
            clocks: self.clocks,
        };
        let ctx = LaunchContext {
            tasks,
            config: self.config,
        };
        runtime.launch(ctx).await
    }
}

/// The lattice is bound; extensions and an optional module-source override
/// may be added before the component builders.
pub struct TypedBuilder<'a, T: RuntimeTypes> {
    config: &'a EngineConfig,
    extensions: Vec<Arc<dyn Extension<T>>>,
    wasm: Option<PathBuf>,
    manifest: Option<PathBuf>,
    clocks: Option<WasiClockOverride>,
    _t: PhantomData<fn() -> T>,
}

impl<'a, T: RuntimeTypes> TypedBuilder<'a, T> {
    /// Add the extensions.
    pub fn with_extensions(
        mut self,
        extensions: impl IntoIterator<Item = Arc<dyn Extension<T>>>,
    ) -> Self {
        self.extensions.extend(extensions);
        self
    }

    /// Set the single-module source override, taking precedence over engine.toml
    /// `[[modules]]`. Both `None` runs the configured modules.
    pub fn with_module_source(mut self, wasm: Option<PathBuf>, manifest: Option<PathBuf>) -> Self {
        self.wasm = wasm;
        self.manifest = manifest;
        self
    }

    /// Override the per-store WASI wall and monotonic clocks. Every module
    /// store, including the ones rebuilt on restart, reads these instead of
    /// the ambient host clocks. Omitting it is behaviour-neutral.
    pub fn with_wasi_clocks(mut self, clocks: WasiClockOverride) -> Self {
        self.clocks = Some(clocks);
        self
    }

    /// Bind the component builders that open the backends at launch.
    pub fn with_components<C, S, E, L>(
        self,
        components: ComponentsBuilder<C, S, E, L>,
    ) -> ComponentsStage<'a, T, C, S, E, L> {
        ComponentsStage {
            config: self.config,
            extensions: self.extensions,
            wasm: self.wasm,
            manifest: self.manifest,
            clocks: self.clocks,
            components,
            _t: PhantomData,
        }
    }
}

/// The component builders are bound; the add-on set remains.
pub struct ComponentsStage<'a, T: RuntimeTypes, C, S, E, L> {
    config: &'a EngineConfig,
    extensions: Vec<Arc<dyn Extension<T>>>,
    wasm: Option<PathBuf>,
    manifest: Option<PathBuf>,
    clocks: Option<WasiClockOverride>,
    components: ComponentsBuilder<C, S, E, L>,
    _t: PhantomData<fn() -> T>,
}

impl<'a, T: RuntimeTypes, C, S, E, L> ComponentsStage<'a, T, C, S, E, L> {
    /// Bind the cross-cutting add-on set installed before the engine boots.
    pub fn with_add_ons(
        self,
        add_ons: &'a [&'a dyn RuntimeAddOn],
    ) -> ReadyBuilder<'a, T, C, S, E, L> {
        ReadyBuilder {
            config: self.config,
            extensions: self.extensions,
            wasm: self.wasm,
            manifest: self.manifest,
            clocks: self.clocks,
            components: self.components,
            add_ons,
        }
    }
}

/// The assembly is complete; [`launch`](Self::launch) opens the backends and
/// runs.
pub struct ReadyBuilder<'a, T: RuntimeTypes, C, S, E, L> {
    config: &'a EngineConfig,
    extensions: Vec<Arc<dyn Extension<T>>>,
    wasm: Option<PathBuf>,
    manifest: Option<PathBuf>,
    clocks: Option<WasiClockOverride>,
    components: ComponentsBuilder<C, S, E, L>,
    add_ons: &'a [&'a dyn RuntimeAddOn],
}

impl<T, C, S, E, L> ReadyBuilder<'_, T, C, S, E, L>
where
    T: RuntimeTypes,
    C: ComponentBuilder<Output = T::Chain>,
    S: ComponentBuilder<Output = T::Store>,
    E: ComponentBuilder<Output = T::Ext>,
    L: ComponentBuilder<Output = LogPipeline>,
{
    /// Open the backends and launch. Builds the [`Components`] bundle from the
    /// bound builders, then drives [`LaunchRuntime::launch`] with a fresh
    /// [`TaskManager`].
    pub async fn launch(self) -> anyhow::Result<RuntimeHandle> {
        let tasks = TaskManager::new();
        let executor = tasks.executor();
        let data_dir = self.config.engine.state_dir.clone();
        let build_ctx = BuilderContext {
            config: self.config,
            data_dir: &data_dir,
            executor: &executor,
        };
        let components = self.components.build::<T>(&build_ctx).await?;

        let runtime = AssembledRuntime {
            components,
            extensions: self.extensions,
            add_ons: self.add_ons,
            wasm: self.wasm.as_deref(),
            manifest: self.manifest.as_deref(),
            clocks: self.clocks,
        };
        let ctx = LaunchContext {
            tasks,
            config: self.config,
        };
        runtime.launch(ctx).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::engine_config::EngineConfig;
    use crate::host::component::{LocalStoreBuilder, ProviderPoolBuilder};
    use crate::preset::CoreRuntime;

    /// The preset shortcut is exercised at runtime, not just compiled: the
    /// component builders open the backends, the add-ons install, and the
    /// launch reaches the supervisor boot, which bails because the default
    /// config declares no modules. Locks the sugar path so a builder-chain
    /// refactor cannot silently break it.
    #[tokio::test]
    async fn preset_launch_runs_the_build_path_then_bails_without_modules() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = EngineConfig::default();
        config.engine.state_dir = dir.path().join("state");

        let err = match RuntimeBuilder::new(&config)
            .runtime::<CoreRuntime>()
            .launch()
            .await
        {
            Ok(_) => panic!("default config declares no modules; launch must bail"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("no modules to run"), "{err}");
    }

    /// when every configured module fails `init`, launch must
    /// abort with an operator-facing error instead of idling behind an
    /// empty event loop.
    #[tokio::test]
    async fn launch_bails_when_all_modules_fail_init() {
        let wasm = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates dir")
            .parent()
            .expect("repo root")
            .join("target/wasm32-wasip2/release/price_alert.wasm");
        if !wasm.exists() {
            eprintln!(
                "SKIP: {} not found - build with `cargo build -p price-alert --target wasm32-wasip2 --release`",
                wasm.display()
            );
            return;
        }

        let dir = tempfile::tempdir().expect("tempdir");
        // Unparseable threshold: the module loads, then `init` fails.
        let manifest = dir.path().join("module.toml");
        std::fs::write(
            &manifest,
            r#"
[module]
name = "price-alert"

[capabilities]
required = ["logging", "chain"]

[[subscription]]
kind     = "block"
chain_id = 11155111

[config]
oracle_address = "0x694AA1769357215DE4FAC081bf1f309aDC325306"
decimals       = "8"
threshold      = "not-a-number"
direction      = "below"
every_n_blocks = "1"
"#,
        )
        .expect("write manifest");

        let mut config = EngineConfig::default();
        config.engine.state_dir = dir.path().join("state");

        let err = match RuntimeBuilder::new(&config)
            .with_types::<CoreRuntime>()
            .with_module_source(Some(wasm), Some(manifest))
            .with_components(ComponentsBuilder::new(
                ProviderPoolBuilder,
                LocalStoreBuilder,
                (),
            ))
            .with_add_ons(&[])
            .launch()
            .await
        {
            Ok(_) => panic!("init-failing module must abort launch"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("failed initialisation"), "{err}");
    }

    /// The add-on set installs before the supervisor boots: a stub add-on's
    /// `install` runs exactly once even though the launch bails on the
    /// no-modules boot that follows.
    #[tokio::test]
    async fn assembled_runtime_installs_add_ons_before_boot() {
        struct CountingAddOn(Arc<AtomicUsize>);
        impl RuntimeAddOn for CountingAddOn {
            fn install(&self, _ctx: &AddOnsContext<'_>) -> anyhow::Result<AddOnHandle> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(AddOnHandle::named("counting"))
            }
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().join("state");
        let mut config = EngineConfig::default();
        config.engine.state_dir = data_dir.clone();

        let tasks = TaskManager::new();
        let executor = tasks.executor();
        let build_ctx = BuilderContext {
            config: &config,
            data_dir: &data_dir,
            executor: &executor,
        };
        let components = ComponentsBuilder::new(ProviderPoolBuilder, LocalStoreBuilder, ())
            .build::<CoreRuntime>(&build_ctx)
            .await
            .expect("build core components");

        let calls = Arc::new(AtomicUsize::new(0));
        let add_on = CountingAddOn(calls.clone());
        let add_on_refs: Vec<&dyn RuntimeAddOn> = vec![&add_on];
        let runtime = AssembledRuntime {
            components,
            extensions: Vec::new(),
            add_ons: &add_on_refs,
            wasm: None,
            manifest: None,
            clocks: None,
        };
        let ctx = LaunchContext {
            tasks,
            config: &config,
        };

        let err = match runtime.launch(ctx).await {
            Ok(_) => panic!("no modules configured; launch must bail"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("no modules to run"), "{err}");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "add-on installed once, before the boot that bails",
        );
    }

    /// Full builder-path launch against the pre-built example module: the
    /// handle exposes the shared log pipeline and the trigger-to-wait
    /// handshake stops the run. Skips when the module fixture is not built
    /// (`just build-module`).
    #[tokio::test]
    async fn e2e_builder_launch_exposes_logs_and_stops_on_shutdown() {
        let wasm = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates dir")
            .parent()
            .expect("repo root")
            .join("target/wasm32-wasip2/release/example.wasm");
        if !wasm.exists() {
            eprintln!(
                "SKIP: {} not found - run `just build-module` to enable E2E tests",
                wasm.display()
            );
            return;
        }
        let manifest = wasm
            .ancestors()
            .nth(3)
            .expect("repo root")
            .join("modules/example/module.toml");

        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = EngineConfig::default();
        config.engine.state_dir = dir.path().join("state");

        let mut handle = RuntimeBuilder::new(&config)
            .with_types::<CoreRuntime>()
            .with_module_source(Some(wasm), Some(manifest))
            .with_components(ComponentsBuilder::new(
                ProviderPoolBuilder,
                LocalStoreBuilder,
                (),
            ))
            .with_add_ons(&[])
            .launch()
            .await
            .expect("launch the example module");

        // The handle carries the run/log read side of the launched pipeline.
        let logs = handle.logs().clone();
        let _ = logs.list_runs("example");

        handle.shutdown();
        handle.wait().await.expect("clean shutdown");
    }

    fn handle_over(tasks: TaskManager, event_loop: TaskHandle<TaskExit>) -> RuntimeHandle {
        RuntimeHandle {
            event_loop,
            tasks,
            logs: test_logs(),
            _add_ons: Vec::new(),
        }
    }

    fn test_logs() -> LogPipeline {
        LogPipeline::in_memory(EngineConfig::default().limits.logs())
    }

    /// A cleanly completing event loop resolves `wait` to `Ok`.
    #[tokio::test]
    async fn runtime_handle_wait_is_ok_on_clean_completion() {
        let tasks = TaskManager::new();
        let event_loop = tasks.executor().spawn(async { TaskExit::ReceiverGone });
        handle_over(tasks, event_loop)
            .wait()
            .await
            .expect("clean completion resolves Ok");
    }

    /// Firing the shutdown trigger drives the event-loop task to completion
    /// and `wait` returns once the graceful guard releases.
    #[tokio::test]
    async fn runtime_handle_shutdown_trigger_drives_wait_to_return() {
        let tasks = TaskManager::new();
        let event_loop = tasks.executor().spawn_graceful(|graceful| async move {
            drop(graceful.await);
            TaskExit::ReceiverGone
        });
        let mut handle = handle_over(tasks, event_loop);
        handle.shutdown();
        handle.wait().await.expect("wait returns after the trigger");
    }

    /// An event-loop task that stops abnormally (here: aborted, the same
    /// join outcome a panic produces) surfaces the wrapped error from
    /// `wait` instead of masking it as a clean stop.
    #[tokio::test]
    async fn runtime_handle_wait_is_err_on_abnormal_stop() {
        let tasks = TaskManager::new();
        let event_loop = tasks.executor().spawn(async {
            std::future::pending::<()>().await;
            TaskExit::ReceiverGone
        });
        event_loop.abort();
        let err = handle_over(tasks, event_loop)
            .wait()
            .await
            .expect_err("aborted task surfaces an error");
        assert!(err.to_string().contains("terminated abnormally"), "{err}");
    }

    /// dropping the handle without `wait` fires the shutdown signal,
    /// so the detached event loop winds down and drains rather than leaking.
    #[tokio::test]
    async fn dropping_handle_without_wait_drains_the_event_loop() {
        let tasks = TaskManager::new();
        let drained = Arc::new(AtomicUsize::new(0));
        let seen = drained.clone();
        let event_loop = tasks.executor().spawn_graceful(move |graceful| async move {
            let guard = graceful.await;
            seen.fetch_add(1, Ordering::SeqCst);
            drop(guard);
            TaskExit::ReceiverGone
        });
        let handle = handle_over(tasks, event_loop);

        drop(handle);

        for _ in 0..200 {
            if drained.load(Ordering::SeqCst) == 1 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("event loop did not drain after the handle was dropped");
    }
}
