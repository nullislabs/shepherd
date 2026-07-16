//! Generic launch entry point: assemble the [`AssembledRuntime`] from
//! pre-built backends and run it until shutdown.
//!
//! Parameterised over the [`RuntimeTypes`] lattice. The composition root
//! builds the concrete [`Components`] and the extension list (including any
//! domain extension such as cow-api) and hands them here; this thin wrapper
//! forwards to the [`builder`](crate::builder) launcher and blocks until the
//! event loop returns. A launcher that wants the
//! [`RuntimeHandle`](crate::builder::RuntimeHandle) back drives
//! [`LaunchRuntime`] directly.

use std::path::Path;
use std::sync::Arc;

use crate::addons::RuntimeAddOn;
use crate::builder::{AssembledRuntime, LaunchContext, LaunchRuntime};
use crate::engine_config::EngineConfig;
use crate::host::component::{Components, RuntimeTypes};
use nexum_tasks::TaskManager;

use crate::host::extension::Extension;

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
    extensions: &[Arc<dyn Extension<T>>],
    add_ons: &[&dyn RuntimeAddOn],
) -> anyhow::Result<()> {
    let runtime = AssembledRuntime {
        components: components.clone(),
        extensions: extensions.to_vec(),
        add_ons,
        wasm,
        manifest,
        clocks: None,
    };
    let ctx = LaunchContext {
        tasks: TaskManager::new(),
        config: engine_cfg,
    };
    runtime.launch(ctx).await?.wait().await
}
