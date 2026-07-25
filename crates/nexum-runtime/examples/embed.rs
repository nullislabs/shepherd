//! Embed the runtime without the CLI: point the builder at a loaded config
//! and a [`Runtime`] preset, then launch and run until shutdown.
//!
//! Build the example module first (`just build-module`), then run
//! `cargo run -p nexum-runtime --example embed` from the repo root.
//!
//! [`Runtime`]: nexum_runtime::preset::Runtime

use nexum_runtime::builder::RuntimeBuilder;
use nexum_runtime::engine_config::{EngineConfig, ModuleEntry};
use nexum_runtime::preset::CoreRuntime;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // The embedder owns the tracing subscriber; the library never
    // installs one.
    tracing_subscriber::fmt().init();

    let cfg = EngineConfig {
        modules: vec![ModuleEntry {
            path: "target/wasm32-wasip2/release/example.wasm".into(),
            manifest: Some("modules/example/module.toml".into()),
        }],
        ..EngineConfig::default()
    };

    // Bind the default preset and launch: the component builders open the
    // backends, the add-ons install, and the event loop runs until shutdown.
    let handle = RuntimeBuilder::new(&cfg)
        .runtime::<CoreRuntime>()
        .launch()
        .await?;

    // The operator surface: the handle's log pipeline serves the run/log
    // read side while (and after) the runtime runs.
    let logs = handle.logs().clone();
    handle.wait().await?;

    for meta in logs.list_runs("example") {
        let page = logs.read(&meta.run, 0);
        println!(
            "run {:?} retained {} record(s)",
            meta.run,
            page.records.len()
        );
    }
    Ok(())
}
