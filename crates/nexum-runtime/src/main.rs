use clap::Parser;
use tracing::info;
use tracing_subscriber::EnvFilter;

use nexum_runtime::cli::Cli;
use nexum_runtime::engine_config;

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

    nexum_runtime::bootstrap::run_from_config(
        &engine_cfg,
        cli.wasm.as_deref(),
        cli.manifest.as_deref(),
    )
    .await
}
