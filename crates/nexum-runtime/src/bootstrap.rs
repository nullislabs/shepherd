//! Generic launch path: install metrics, build the linker, boot the
//! supervisor, and drive the event loop until shutdown.
//!
//! Parameterised over the [`RuntimeTypes`] lattice. The composition root
//! builds the concrete [`Components`] and the extension list (including any
//! domain extension such as cow-api) and hands them here; this module
//! stays free of every domain backend.

use std::path::Path;

use tracing::{info, warn};
use wasmtime::Engine;

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
pub async fn run<T: RuntimeTypes>(
    engine_cfg: &EngineConfig,
    wasm: Option<&Path>,
    manifest: Option<&Path>,
    components: &Components<T>,
    extensions: &[Extension<T>],
) -> anyhow::Result<()> {
    // Install the Prometheus exporter. When
    // `[engine.metrics].enabled = true` the HTTP listener also binds
    // and serves `/metrics`. Otherwise the recorder is still
    // installed (so `metrics::counter!` etc. call sites stay live)
    // but no port is opened. This means the same binary can be run
    // in CI / tests without binding a port and in production with
    // observability enabled by flipping one config flag.
    if engine_cfg.engine.metrics.enabled {
        let addr: std::net::SocketAddr =
            engine_cfg.engine.metrics.bind_addr.parse().map_err(|e| {
                anyhow::anyhow!(
                    "invalid [engine.metrics].bind_addr `{}`: {e}",
                    engine_cfg.engine.metrics.bind_addr
                )
            })?;
        metrics_exporter_prometheus::PrometheusBuilder::new()
            .with_http_listener(addr)
            .install()
            .map_err(|e| anyhow::anyhow!("install Prometheus exporter on {addr}: {e}"))?;
        info!(addr = %addr, "metrics exporter listening at /metrics");
    } else {
        // Recorder still installed so call sites do not panic; just
        // discarded into a no-op sink instead of served.
        metrics_exporter_prometheus::PrometheusBuilder::new()
            .install_recorder()
            .map_err(|e| anyhow::anyhow!("install Prometheus recorder: {e}"))?;
    }

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
