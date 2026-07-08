//! Generic launch path: install add-ons, build the linker, boot the
//! supervisor, and drive the event loop until shutdown.
//!
//! Parameterised over the [`RuntimeTypes`] lattice. The composition root
//! builds the concrete [`Components`] and the extension list (including any
//! domain extension such as cow-api) and hands them here; this module
//! stays free of every domain backend.

use std::path::Path;

use tracing::{info, warn};
use wasmtime::Engine;

use crate::addons::{AddOnsContext, RuntimeAddOns};
use crate::engine_config::EngineConfig;
use crate::host::component::{Components, RuntimeTypes};
use crate::host::extension::Extension;
use crate::runtime;
use crate::supervisor;

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
    // Install cross-cutting add-ons before the engine boots so any metric
    // recorder is live for the whole run. Handles are held until shutdown;
    // dropping them tears their add-on down.
    let addons_ctx = AddOnsContext {
        metrics: &engine_cfg.engine.metrics,
    };
    let _add_on_handles = add_ons
        .iter()
        .map(|add_on| add_on.install(&addons_ctx))
        .collect::<anyhow::Result<Vec<_>>>()?;

    // wasmtime engine + linker - one of each, shared across modules.
    let mut config = wasmtime::Config::new();
    config.wasm_component_model(true);
    config.consume_fuel(true);
    let engine = Engine::new(&config)?;

    let linker = supervisor::build_linker::<T>(&engine, extensions)?;

    // Boot supervisor - `engine.toml.[[modules]]` first, CLI positional second.
    let mut supervisor = if let Some(wasm) = wasm {
        if !engine_cfg.modules.is_empty() {
            warn!("ignoring engine.toml [[modules]] because a positional <wasm-path> was given");
        }
        supervisor::Supervisor::boot_single(
            &engine,
            &linker,
            wasm,
            manifest,
            components,
            &engine_cfg.limits,
            extensions,
        )
        .await?
    } else if !engine_cfg.modules.is_empty() {
        supervisor::Supervisor::boot(&engine, &linker, engine_cfg, components, extensions).await?
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

    // Open per-chain block subscriptions + per-module chain-log
    // subscriptions, merge, dispatch until shutdown.
    let block_chains = supervisor.block_chains();
    let chain_log_subs = supervisor.chain_log_subscriptions();

    if block_chains.is_empty() && chain_log_subs.is_empty() {
        info!("no [[subscription]] entries - engine has nothing to run; exiting");
        return Ok(());
    }

    let executor = runtime::task::TokioExecutor;
    let mut reconnect_tasks = runtime::task::TaskSet::new();
    let block_streams = runtime::event_loop::open_block_streams(
        &components.chain,
        &block_chains,
        &executor,
        &mut reconnect_tasks,
    );
    let chain_log_streams = runtime::event_loop::open_chain_log_streams(
        &components.chain,
        chain_log_subs,
        &executor,
        &mut reconnect_tasks,
    );

    let shutdown = async {
        match runtime::event_loop::wait_for_shutdown_signal().await {
            Ok(name) => info!(signal = %name, "shutdown signal received"),
            Err(err) => warn!(error = %err, "signal handler failed - using ctrl-c"),
        }
    };

    runtime::event_loop::run(
        &mut supervisor,
        block_streams,
        chain_log_streams,
        reconnect_tasks,
        shutdown,
    )
    .await;
    info!("done");
    Ok(())
}
