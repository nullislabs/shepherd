#![cfg_attr(not(test), warn(unused_crate_dependencies))]

// alloy split its API across multiple crates; we depend on the
// transports directly so cargo resolves the right feature set, but
// the runtime code only names them through the `alloy_provider`
// re-exports. Silence `unused_crate_dependencies` with `as _`.
use alloy_rpc_client as _;
use alloy_transport as _;
use alloy_transport_ws as _;

mod bindings;
mod cli;
mod engine_config;
mod host;
mod manifest;
mod runtime;
mod supervisor;

use clap::Parser;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use wasmtime::Engine;
use wasmtime::component::Linker;

use crate::bindings::Shepherd;
use crate::cli::Cli;
use crate::host::state::HostState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let engine_cfg = engine_config::load_or_default(cli.engine_config.as_deref())?;

    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(&engine_cfg.engine.log_level))
        .unwrap_or_else(|_| EnvFilter::new("info"));
    // Structured logging: JSON by default (machine-readable
    // for production; one `jq` query reconstructs any dispatch
    // timeline); `--pretty-logs` opts back into the 0.1 human-readable
    // formatter for local dev. The same `EnvFilter` applies to both
    // so `RUST_LOG=debug` works identically.
    if cli.pretty_logs {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_target(true)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_target(true)
            .json()
            .flatten_event(true)
            .with_current_span(false)
            .init();
    }

    info!("nexum-engine starting");

    // Surface config footguns now that the tracing subscriber is
    // up. Today's only check: an HTTP `rpc_url` would loop forever
    // in the event-loop's WS reconnect backoff because
    // `eth_subscribe` is WS-only. One ERROR log per offending chain
    // with the exact `wss://` swap suggested. See
    // `engine_config::validate_transports`.
    engine_cfg.validate_transports();

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

    // Bring up shared host backends.
    std::fs::create_dir_all(&engine_cfg.engine.state_dir).map_err(|e| {
        anyhow::anyhow!(
            "create state directory {}: {e}",
            engine_cfg.engine.state_dir.display()
        )
    })?;
    let store_path = engine_cfg.engine.state_dir.join("local-store.redb");
    let local_store = host::local_store_redb::LocalStore::open(&store_path)
        .map_err(|e| anyhow::anyhow!("open local-store at {}: {e}", store_path.display()))?;
    let cow_pool = host::cow_orderbook::OrderBookPool::from_config(&engine_cfg);
    let provider_pool = host::provider_pool::ProviderPool::from_config(&engine_cfg).await?;

    // wasmtime engine + linker - one of each, shared across modules.
    let mut config = wasmtime::Config::new();
    config.wasm_component_model(true);
    config.consume_fuel(true);
    let engine = Engine::new(&config)?;

    let mut linker = Linker::<HostState>::new(&engine);
    Shepherd::add_to_linker::<HostState, wasmtime::component::HasSelf<HostState>>(
        &mut linker,
        |state| state,
    )?;
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;

    // Boot supervisor - `engine.toml.[[modules]]` first, CLI positional second.
    let mut supervisor = if let Some(wasm) = cli.wasm.as_deref() {
        if !engine_cfg.modules.is_empty() {
            warn!("ignoring engine.toml [[modules]] because a positional <wasm-path> was given");
        }
        supervisor::Supervisor::boot_single(
            &engine,
            &linker,
            wasm,
            cli.manifest.as_deref(),
            &cow_pool,
            &provider_pool,
            &local_store,
            &engine_cfg.limits,
        )
        .await?
    } else if !engine_cfg.modules.is_empty() {
        supervisor::Supervisor::boot(
            &engine,
            &linker,
            &engine_cfg,
            &cow_pool,
            &provider_pool,
            &local_store,
        )
        .await?
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

    // Open per-chain block subscriptions + per-module log
    // subscriptions, merge, dispatch until shutdown.
    let block_chains = supervisor.block_chains();
    let log_subs = supervisor.log_subscriptions();

    if block_chains.is_empty() && log_subs.is_empty() {
        info!("no [[subscription]] entries - engine has nothing to run; exiting");
        return Ok(());
    }

    let mut reconnect_tasks = tokio::task::JoinSet::new();
    let block_streams = runtime::event_loop::open_block_streams(
        &provider_pool,
        &block_chains,
        &mut reconnect_tasks,
    )
    .await;
    let log_streams =
        runtime::event_loop::open_log_streams(&provider_pool, log_subs, &mut reconnect_tasks).await;

    let shutdown = async {
        match runtime::event_loop::wait_for_shutdown_signal().await {
            Ok(name) => info!(signal = %name, "shutdown signal received"),
            Err(err) => warn!(error = %err, "signal handler failed - using ctrl-c"),
        }
    };

    runtime::event_loop::run(
        &mut supervisor,
        block_streams,
        log_streams,
        reconnect_tasks,
        shutdown,
    )
    .await;
    info!("done");
    Ok(())
}
