//! Type-state runtime builder and the imperative launcher it drives.
//!
//! [`RuntimeBuilder`] accumulates the assembly (config, the [`RuntimeTypes`]
//! lattice, extensions, the component builders, add-ons) through a type-state
//! chain; [`ReadyBuilder::launch`] opens the backends and hands off to
//! [`LaunchRuntime::launch`]. The launcher runs one imperative sequence -
//! install add-ons, build the engine and linker, boot the supervisor, open
//! subscriptions through the [`TaskExecutor`], spawn the event loop - and
//! returns a [`RuntimeHandle`] owning the running tasks plus a shutdown
//! trigger.
//!
//! The reference binary reaches this through its `run_from_config` one-liner;
//! an embedder holding pre-built backends constructs an [`AssembledRuntime`]
//! and calls [`LaunchRuntime::launch`] directly. For the common case,
//! [`RuntimeBuilder::runtime`] binds a [`Runtime`] preset that bundles the
//! lattice, component builders, and add-ons in one call.

use std::future::Future;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use tracing::{info, warn};
use wasmtime::Engine;

use crate::addons::{AddOnHandle, AddOnsContext, RuntimeAddOns};
use crate::engine_config::EngineConfig;
use crate::host::component::{
    BuilderContext, ComponentBuilder, Components, ComponentsBuilder, RuntimeTypes,
};
use crate::host::extension::Extension;
use crate::host::logs::LogPipeline;
use crate::preset::Runtime;
use crate::runtime::event_loop;
use crate::runtime::task::{TaskExecutor, TaskExit, TaskHandle, TaskSet, TokioExecutor};
pub use crate::supervisor::WasiClockOverride;
use crate::supervisor::{self, Supervisor};

/// Ambient inputs the imperative launcher reads: the executor that spawns the
/// long-lived subscription and event-loop tasks, the resolved data directory,
/// and the loaded config.
pub struct LaunchContext<'a> {
    /// Spawns the subscription and event-loop tasks.
    pub executor: &'a dyn TaskExecutor,
    /// Directory the backends root their on-disk state at. Advisory: the
    /// launcher receives pre-built backends, so it does not open the data
    /// directory itself; a builder that opens the backends reads the data
    /// directory at build time, not here.
    pub data_dir: &'a Path,
    /// The loaded engine config.
    pub config: &'a EngineConfig,
}

/// A running runtime: the event-loop task handle, a shutdown trigger, and the
/// add-on handles kept alive for the run.
///
/// Firing the trigger (via [`shutdown`](Self::shutdown) or by dropping the
/// handle) stops the event loop between dispatches; it drains its subscription
/// tasks and returns. [`wait`](Self::wait) awaits that completion.
pub struct RuntimeHandle {
    event_loop: TaskHandle,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    logs: LogPipeline,
    // Held for the length of the run; dropped once the event loop has joined.
    _add_ons: Vec<AddOnHandle>,
}

impl RuntimeHandle {
    /// Signal the event loop to stop. The in-flight dispatch finishes first.
    pub fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }

    /// The shared log pipeline: the read side for module runs and log pages.
    /// Clone it to keep reading after [`wait`](Self::wait) consumes the handle.
    pub fn logs(&self) -> &LogPipeline {
        &self.logs
    }

    /// Await the event loop's completion, returning once it has stopped and
    /// drained its subscription tasks. A `None` join reason means the task
    /// panicked or was aborted rather than shutting down cleanly; surface it
    /// instead of masking the failure.
    pub async fn wait(self) -> anyhow::Result<()> {
        match self.event_loop.join().await {
            Some(_) => Ok(()),
            None => anyhow::bail!("event loop task terminated abnormally"),
        }
    }
}

/// A fully-assembled runtime: concrete backends, extensions, add-ons, and the
/// optional module-source override. Implements [`LaunchRuntime`].
pub struct AssembledRuntime<'a, T: RuntimeTypes> {
    /// Shared backends threaded into every module store.
    pub components: Components<T>,
    /// Linker hooks and capability namespaces.
    pub extensions: Vec<Extension<T>>,
    /// Cross-cutting facilities installed before the engine boots.
    pub add_ons: &'a [&'a dyn RuntimeAddOns],
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
        let engine_cfg = ctx.config;

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

        info!(
            modules = supervisor.module_count(),
            adapters = supervisor.adapter_count(),
            chains = supervisor.block_chains().len(),
            "supervisor ready"
        );

        // Programmatic shutdown trigger, selected against the OS signal inside
        // the event-loop task. Dropping the sender (with the handle) also fires.
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        // The handle keeps the log read side reachable after launch consumes
        // the components.
        let logs = components.logs.clone();

        let block_chains = supervisor.block_chains();
        let chain_log_subs = supervisor.chain_log_subscriptions();
        // Status polling runs only when it can produce something a module
        // will see: at least one intent-status subscriber and at least one
        // installed adapter to poll.
        let poll_statuses = supervisor.has_intent_status_subscribers()
            && supervisor.pool_router().venue_count() > 0;

        // No subscriptions: nothing to drive. Return a handle whose event loop
        // is already complete so `wait` resolves immediately.
        if block_chains.is_empty() && chain_log_subs.is_empty() && !poll_statuses {
            info!("no [[subscription]] entries - engine has nothing to run; exiting");
            let event_loop = ctx
                .executor
                .spawn(Box::pin(async { TaskExit::ReceiverGone }));
            return Ok(RuntimeHandle {
                event_loop,
                shutdown: Some(shutdown_tx),
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
            ctx.executor,
            &mut reconnect_tasks,
        );
        let chain_log_streams = event_loop::open_chain_log_streams(
            &components.chain,
            chain_log_subs,
            ctx.executor,
            &mut reconnect_tasks,
        );
        let intent_status_stream = poll_statuses.then(|| {
            event_loop::open_intent_status_stream(
                supervisor.pool_router(),
                engine_cfg.limits.status_poll_interval(),
                ctx.executor,
                &mut reconnect_tasks,
            )
        });

        let event_loop = ctx.executor.spawn(Box::pin(async move {
            let shutdown = async move {
                // A failed signal registration must not resolve the shutdown
                // future: park that leg so the programmatic trigger (or the
                // handle dropping) remains the only stop.
                let signal = async {
                    match event_loop::wait_for_shutdown_signal().await {
                        Ok(name) => info!(signal = %name, "shutdown signal received"),
                        Err(err) => {
                            warn!(error = %err, "signal handler failed - programmatic shutdown only");
                            std::future::pending::<()>().await
                        }
                    }
                };
                tokio::select! {
                    _ = shutdown_rx => info!("shutdown requested"),
                    () = signal => {},
                }
            };
            let mut supervisor = supervisor;
            event_loop::run(
                &mut supervisor,
                block_streams,
                chain_log_streams,
                intent_status_stream,
                reconnect_tasks,
                shutdown,
            )
            .await;
            info!("done");
            TaskExit::ReceiverGone
        }));

        Ok(RuntimeHandle {
            event_loop,
            shutdown: Some(shutdown_tx),
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
            executor: None,
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
            executor: None,
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
    extensions: Vec<Extension<R::Types>>,
    wasm: Option<PathBuf>,
    manifest: Option<PathBuf>,
    executor: Option<&'a dyn TaskExecutor>,
    clocks: Option<WasiClockOverride>,
    _r: PhantomData<fn() -> R>,
}

impl<'a, R: Runtime> PresetBuilder<'a, R> {
    /// Add extension linker hooks and capability namespaces on top of the
    /// preset. The default preset carries none.
    pub fn with_extensions(
        mut self,
        extensions: impl IntoIterator<Item = Extension<R::Types>>,
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

    /// Bind the executor the launcher spawns its tasks on. Defaults to the
    /// ambient tokio runtime.
    pub fn with_executor(mut self, executor: &'a dyn TaskExecutor) -> Self {
        self.executor = Some(executor);
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
    /// then drives [`LaunchRuntime::launch`] on the bound executor (the
    /// ambient tokio runtime by default).
    pub async fn launch(self) -> anyhow::Result<RuntimeHandle> {
        let data_dir = self.config.engine.state_dir.clone();
        let build_ctx = BuilderContext {
            config: self.config,
            data_dir: &data_dir,
        };
        let components = R::components().build::<R::Types>(&build_ctx).await?;

        // The preset owns its add-ons; the launcher borrows each one to
        // install it, so both the owned set and the ref view stay live across
        // the launch await.
        let add_ons = R::add_ons();
        let add_on_refs: Vec<&dyn RuntimeAddOns> = add_ons.iter().map(|a| &**a).collect();

        let runtime = AssembledRuntime {
            components,
            extensions: self.extensions,
            add_ons: &add_on_refs,
            wasm: self.wasm.as_deref(),
            manifest: self.manifest.as_deref(),
            clocks: self.clocks,
        };
        let ctx = LaunchContext {
            executor: self.executor.unwrap_or(&TokioExecutor),
            data_dir: &data_dir,
            config: self.config,
        };
        runtime.launch(ctx).await
    }
}

/// The lattice is bound; extensions and an optional module-source override
/// may be added before the component builders.
pub struct TypedBuilder<'a, T: RuntimeTypes> {
    config: &'a EngineConfig,
    extensions: Vec<Extension<T>>,
    wasm: Option<PathBuf>,
    manifest: Option<PathBuf>,
    executor: Option<&'a dyn TaskExecutor>,
    clocks: Option<WasiClockOverride>,
    _t: PhantomData<fn() -> T>,
}

impl<'a, T: RuntimeTypes> TypedBuilder<'a, T> {
    /// Add the extension linker hooks and capability namespaces.
    pub fn with_extensions(mut self, extensions: impl IntoIterator<Item = Extension<T>>) -> Self {
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

    /// Bind the executor the launcher spawns its tasks on. Defaults to the
    /// ambient tokio runtime.
    pub fn with_executor(mut self, executor: &'a dyn TaskExecutor) -> Self {
        self.executor = Some(executor);
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
    pub fn with_components<C, S, E>(
        self,
        components: ComponentsBuilder<C, S, E>,
    ) -> ComponentsStage<'a, T, C, S, E> {
        ComponentsStage {
            config: self.config,
            extensions: self.extensions,
            wasm: self.wasm,
            manifest: self.manifest,
            executor: self.executor,
            clocks: self.clocks,
            components,
            _t: PhantomData,
        }
    }
}

/// The component builders are bound; the add-on set remains.
pub struct ComponentsStage<'a, T: RuntimeTypes, C, S, E> {
    config: &'a EngineConfig,
    extensions: Vec<Extension<T>>,
    wasm: Option<PathBuf>,
    manifest: Option<PathBuf>,
    executor: Option<&'a dyn TaskExecutor>,
    clocks: Option<WasiClockOverride>,
    components: ComponentsBuilder<C, S, E>,
    _t: PhantomData<fn() -> T>,
}

impl<'a, T: RuntimeTypes, C, S, E> ComponentsStage<'a, T, C, S, E> {
    /// Bind the cross-cutting add-on set installed before the engine boots.
    pub fn with_add_ons(
        self,
        add_ons: &'a [&'a dyn RuntimeAddOns],
    ) -> ReadyBuilder<'a, T, C, S, E> {
        ReadyBuilder {
            config: self.config,
            extensions: self.extensions,
            wasm: self.wasm,
            manifest: self.manifest,
            executor: self.executor,
            clocks: self.clocks,
            components: self.components,
            add_ons,
        }
    }
}

/// The assembly is complete; [`launch`](Self::launch) opens the backends and
/// runs.
pub struct ReadyBuilder<'a, T: RuntimeTypes, C, S, E> {
    config: &'a EngineConfig,
    extensions: Vec<Extension<T>>,
    wasm: Option<PathBuf>,
    manifest: Option<PathBuf>,
    executor: Option<&'a dyn TaskExecutor>,
    clocks: Option<WasiClockOverride>,
    components: ComponentsBuilder<C, S, E>,
    add_ons: &'a [&'a dyn RuntimeAddOns],
}

impl<T, C, S, E> ReadyBuilder<'_, T, C, S, E>
where
    T: RuntimeTypes,
    C: ComponentBuilder<Output = T::Chain>,
    S: ComponentBuilder<Output = T::Store>,
    E: ComponentBuilder<Output = T::Ext>,
{
    /// Open the backends and launch. Builds the [`Components`] bundle from the
    /// bound builders, then drives [`LaunchRuntime::launch`] on the bound
    /// executor (the ambient tokio runtime by default).
    pub async fn launch(self) -> anyhow::Result<RuntimeHandle> {
        let data_dir = self.config.engine.state_dir.clone();
        let build_ctx = BuilderContext {
            config: self.config,
            data_dir: &data_dir,
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
            executor: self.executor.unwrap_or(&TokioExecutor),
            data_dir: &data_dir,
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

    /// The add-on set installs before the supervisor boots: a stub add-on's
    /// `install` runs exactly once even though the launch bails on the
    /// no-modules boot that follows.
    #[tokio::test]
    async fn assembled_runtime_installs_add_ons_before_boot() {
        struct CountingAddOn(Arc<AtomicUsize>);
        impl RuntimeAddOns for CountingAddOn {
            fn install(&self, _ctx: &AddOnsContext<'_>) -> anyhow::Result<AddOnHandle> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(AddOnHandle::named("counting"))
            }
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().join("state");
        let mut config = EngineConfig::default();
        config.engine.state_dir = data_dir.clone();

        let build_ctx = BuilderContext {
            config: &config,
            data_dir: &data_dir,
        };
        let components = ComponentsBuilder::new(ProviderPoolBuilder, LocalStoreBuilder, ())
            .build::<CoreRuntime>(&build_ctx)
            .await
            .expect("build core components");

        let calls = Arc::new(AtomicUsize::new(0));
        let add_on = CountingAddOn(calls.clone());
        let add_on_refs: Vec<&dyn RuntimeAddOns> = vec![&add_on];
        let runtime = AssembledRuntime {
            components,
            extensions: Vec::new(),
            add_ons: &add_on_refs,
            wasm: None,
            manifest: None,
            clocks: None,
        };
        let executor = TokioExecutor;
        let ctx = LaunchContext {
            executor: &executor,
            data_dir: &data_dir,
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
    /// bound executor spawns the launch tasks, the handle exposes the shared
    /// log pipeline, and the trigger-to-wait handshake stops the run. Skips
    /// when the module fixture is not built (`just build-module`).
    #[tokio::test]
    async fn e2e_builder_launch_uses_the_bound_executor_and_exposes_logs() {
        use crate::runtime::task::TaskFuture;

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

        struct CountingExecutor(AtomicUsize);
        impl TaskExecutor for CountingExecutor {
            fn spawn(&self, fut: TaskFuture) -> TaskHandle {
                self.0.fetch_add(1, Ordering::SeqCst);
                TokioExecutor.spawn(fut)
            }
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = EngineConfig::default();
        config.engine.state_dir = dir.path().join("state");

        let executor = CountingExecutor(AtomicUsize::new(0));
        let mut handle = RuntimeBuilder::new(&config)
            .with_types::<CoreRuntime>()
            .with_module_source(Some(wasm), Some(manifest))
            .with_executor(&executor)
            .with_components(ComponentsBuilder::new(
                ProviderPoolBuilder,
                LocalStoreBuilder,
                (),
            ))
            .with_add_ons(&[])
            .launch()
            .await
            .expect("launch the example module");

        assert!(
            executor.0.load(Ordering::SeqCst) >= 1,
            "the bound executor spawned the launch tasks",
        );
        // The handle carries the run/log read side of the launched pipeline.
        let logs = handle.logs().clone();
        let _ = logs.list_runs("example");

        handle.shutdown();
        handle.wait().await.expect("clean shutdown");
    }

    fn ok_handle(event_loop: TaskHandle) -> RuntimeHandle {
        let (shutdown, _rx) = tokio::sync::oneshot::channel::<()>();
        RuntimeHandle {
            event_loop,
            shutdown: Some(shutdown),
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
        let event_loop = TokioExecutor.spawn(Box::pin(async { TaskExit::ReceiverGone }));
        ok_handle(event_loop)
            .wait()
            .await
            .expect("clean completion resolves Ok");
    }

    /// Firing the shutdown trigger drives the event-loop task to completion
    /// and `wait` returns. Locks the trigger to wait handshake.
    #[tokio::test]
    async fn runtime_handle_shutdown_trigger_drives_wait_to_return() {
        let (shutdown, rx) = tokio::sync::oneshot::channel::<()>();
        let event_loop = TokioExecutor.spawn(Box::pin(async move {
            let _ = rx.await;
            TaskExit::ReceiverGone
        }));
        let mut handle = RuntimeHandle {
            event_loop,
            shutdown: Some(shutdown),
            logs: test_logs(),
            _add_ons: Vec::new(),
        };
        handle.shutdown();
        handle.wait().await.expect("wait returns after the trigger");
    }

    /// An event-loop task that stops abnormally (here: aborted, the same
    /// join outcome a panic produces) surfaces the wrapped error from
    /// `wait` instead of masking it as a clean stop.
    #[tokio::test]
    async fn runtime_handle_wait_is_err_on_abnormal_stop() {
        let event_loop = TokioExecutor.spawn(Box::pin(async {
            std::future::pending::<()>().await;
            TaskExit::ReceiverGone
        }));
        event_loop.abort();
        let err = ok_handle(event_loop)
            .wait()
            .await
            .expect_err("aborted task surfaces an error");
        assert!(err.to_string().contains("terminated abnormally"), "{err}");
    }
}
