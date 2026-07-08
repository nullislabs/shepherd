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
use crate::preset::Runtime;
use crate::runtime::event_loop;
use crate::runtime::task::{TaskExecutor, TaskExit, TaskHandle, TaskSet, TokioExecutor};
use crate::supervisor::{self, Supervisor};

/// Ambient inputs the imperative launcher reads: the executor that spawns the
/// long-lived subscription and event-loop tasks, the resolved data directory,
/// and the loaded config.
pub struct LaunchContext<'a> {
    /// Spawns the subscription and event-loop tasks.
    pub executor: &'a dyn TaskExecutor,
    /// Directory the backends root their on-disk state at.
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
/// optional positional module source. Implements [`LaunchRuntime`].
pub struct AssembledRuntime<'a, T: RuntimeTypes> {
    /// Shared backends threaded into every module store.
    pub components: Components<T>,
    /// Linker hooks and capability namespaces.
    pub extensions: Vec<Extension<T>>,
    /// Cross-cutting facilities installed before the engine boots.
    pub add_ons: &'a [&'a dyn RuntimeAddOns],
    /// Positional single-module override; `None` runs `[[modules]]`.
    pub wasm: Option<&'a Path>,
    /// Manifest paired with `wasm`.
    pub manifest: Option<&'a Path>,
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

        // Boot supervisor - `engine.toml.[[modules]]` first, CLI positional second.
        let supervisor = if let Some(wasm) = wasm {
            if !engine_cfg.modules.is_empty() {
                warn!(
                    "ignoring engine.toml [[modules]] because a positional <wasm-path> was given"
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
            )
            .await?
        } else if !engine_cfg.modules.is_empty() {
            Supervisor::boot(&engine, &linker, engine_cfg, &components, &extensions).await?
        } else {
            anyhow::bail!(
                "no modules to run - either pass a positional <wasm-path> or declare \
                 [[modules]] entries in engine.toml"
            );
        };

        info!(
            modules = supervisor.module_count(),
            chains = supervisor.block_chains().len(),
            "supervisor ready"
        );

        // Programmatic shutdown trigger, selected against the OS signal inside
        // the event-loop task. Dropping the sender (with the handle) also fires.
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        let block_chains = supervisor.block_chains();
        let chain_log_subs = supervisor.chain_log_subscriptions();

        // No subscriptions: nothing to drive. Return a handle whose event loop
        // is already complete so `wait` resolves immediately.
        if block_chains.is_empty() && chain_log_subs.is_empty() {
            info!("no [[subscription]] entries - engine has nothing to run; exiting");
            let event_loop = ctx
                .executor
                .spawn(Box::pin(async { TaskExit::ReceiverGone }));
            return Ok(RuntimeHandle {
                event_loop,
                shutdown: Some(shutdown_tx),
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

        let event_loop = ctx.executor.spawn(Box::pin(async move {
            let shutdown = async move {
                tokio::select! {
                    _ = shutdown_rx => info!("shutdown requested"),
                    res = event_loop::wait_for_shutdown_signal() => match res {
                        Ok(name) => info!(signal = %name, "shutdown signal received"),
                        Err(err) => warn!(error = %err, "signal handler failed - using ctrl-c"),
                    },
                }
            };
            let mut supervisor = supervisor;
            event_loop::run(
                &mut supervisor,
                block_streams,
                chain_log_streams,
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
    _r: PhantomData<fn() -> R>,
}

impl<R: Runtime> PresetBuilder<'_, R> {
    /// Add extension linker hooks and capability namespaces on top of the
    /// preset. The default preset carries none.
    pub fn with_extensions(
        mut self,
        extensions: impl IntoIterator<Item = Extension<R::Types>>,
    ) -> Self {
        self.extensions.extend(extensions);
        self
    }

    /// Set the positional single-module source, overriding engine.toml
    /// `[[modules]]`. Both `None` runs the configured modules.
    pub fn with_module_source(mut self, wasm: Option<PathBuf>, manifest: Option<PathBuf>) -> Self {
        self.wasm = wasm;
        self.manifest = manifest;
        self
    }

    /// Open the preset's backends and launch. Builds the [`Components`] bundle
    /// from the preset's component builders, installs the preset's add-ons,
    /// then drives [`LaunchRuntime::launch`] on the ambient tokio executor.
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
        };
        let executor = TokioExecutor;
        let ctx = LaunchContext {
            executor: &executor,
            data_dir: &data_dir,
            config: self.config,
        };
        runtime.launch(ctx).await
    }
}

/// The lattice is bound; extensions and an optional positional module source
/// may be added before the component builders.
pub struct TypedBuilder<'a, T: RuntimeTypes> {
    config: &'a EngineConfig,
    extensions: Vec<Extension<T>>,
    wasm: Option<PathBuf>,
    manifest: Option<PathBuf>,
    _t: PhantomData<fn() -> T>,
}

impl<'a, T: RuntimeTypes> TypedBuilder<'a, T> {
    /// Add the extension linker hooks and capability namespaces.
    pub fn with_extensions(mut self, extensions: impl IntoIterator<Item = Extension<T>>) -> Self {
        self.extensions.extend(extensions);
        self
    }

    /// Set the positional single-module source, overriding engine.toml
    /// `[[modules]]`. Both `None` runs the configured modules.
    pub fn with_module_source(mut self, wasm: Option<PathBuf>, manifest: Option<PathBuf>) -> Self {
        self.wasm = wasm;
        self.manifest = manifest;
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
    /// bound builders, then drives [`LaunchRuntime::launch`] on the ambient
    /// tokio executor.
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
        };
        let executor = TokioExecutor;
        let ctx = LaunchContext {
            executor: &executor,
            data_dir: &data_dir,
            config: self.config,
        };
        runtime.launch(ctx).await
    }
}
