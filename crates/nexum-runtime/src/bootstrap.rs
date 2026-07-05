//! Generic launch entry point: assemble the [`AssembledRuntime`] from
//! pre-built backends and run it until shutdown.
//!
//! Parameterised over the [`RuntimeTypes`] lattice. The composition root
//! builds the concrete [`Components`] and the extension list (including any
//! domain extension such as cow-api) and hands them here; this thin wrapper
//! forwards to the [`builder`](crate::builder) launcher and blocks until the
//! event loop returns. A launcher that wants the [`RuntimeHandle`] back drives
//! [`LaunchRuntime`](crate::builder::LaunchRuntime) directly.

use std::path::Path;

use crate::addons::RuntimeAddOns;
use crate::builder::{AssembledRuntime, LaunchContext, LaunchRuntime};
use crate::engine_config::EngineConfig;
use crate::host::component::{Components, RuntimeTypes};
use crate::host::extension::Extension;
use crate::runtime::task::TokioExecutor;

/// Launch the runtime from a loaded config and run until shutdown.
///
/// `components` carries the shared backends threaded into every module
/// store; `extensions` carries the linker hooks and capability namespaces
/// assembled at the composition root. Both must agree: a module importing
/// an extension interface boots only if that extension is present in both.
///
/// `add_ons` carries the cross-cutting facilities (the Prometheus exporter
/// today) installed before the engine boots; the composition root picks the
/// set so an embedder omits or replaces any of them.
pub async fn run<T: RuntimeTypes>(
    engine_cfg: &EngineConfig,
    wasm: Option<&Path>,
    manifest: Option<&Path>,
    components: &Components<T>,
    extensions: &[Extension<T>],
    add_ons: &[&dyn RuntimeAddOns],
) -> anyhow::Result<()> {
    let runtime = AssembledRuntime {
        components: components.clone(),
        extensions: extensions.to_vec(),
        add_ons,
        wasm,
        manifest,
    };
    let executor = TokioExecutor;
    let ctx = LaunchContext {
        executor: &executor,
        data_dir: &engine_cfg.engine.state_dir,
        config: engine_cfg,
    };
    runtime.launch(ctx).await?.wait().await
}
