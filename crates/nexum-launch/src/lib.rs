//! Generic engine launcher: parse the shared CLI, load the engine config,
//! initialise tracing, and drive a [`Runtime`] preset until shutdown.
//!
//! A binary is one line: `nexum_launch::run("nexum", CoreRuntime)`. The
//! preset supplies the lattice, backends, extension list, and add-ons;
//! this crate knows nothing beyond the runtime seam.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

mod cli;

pub use cli::Cli;

use nexum_runtime::builder::RuntimeBuilder;
use nexum_runtime::engine_config::{self, EngineConfig};
use nexum_runtime::preset::Runtime;
use tracing::info;
use tracing_subscriber::EnvFilter;

/// Parse the process arguments as `name`, then [`launch`] the preset.
pub async fn run<R: Runtime>(name: &'static str, preset: R) -> anyhow::Result<()> {
    launch(name, preset, Cli::parse_as(name)).await
}

/// Load the config, initialise tracing, and run the preset until shutdown.
pub async fn launch<R: Runtime>(name: &str, preset: R, cli: Cli) -> anyhow::Result<()> {
    let mut engine_cfg = engine_config::load_or_default(cli.engine_config.as_deref())?;
    if let Some(n) = cli.log_backfill_concurrency {
        engine_cfg.engine.log_backfill_concurrency = n;
    }

    init_tracing(cli.pretty_logs, &engine_cfg);

    info!("{name} starting");

    RuntimeBuilder::new(&engine_cfg)
        .with_runtime(preset)
        .with_module_source(cli.wasm, cli.manifest)
        .launch()
        .await?
        .wait()
        .await
}

/// Install the global tracing subscriber: JSON by default, the
/// human-readable formatter behind `--pretty-logs`. The same
/// [`EnvFilter`] (`RUST_LOG`, else the config level) applies to both.
fn init_tracing(pretty: bool, engine_cfg: &EngineConfig) {
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(&engine_cfg.engine.log_level))
        .unwrap_or_else(|_| EnvFilter::new("info"));
    if pretty {
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
}
