//! Embed the runtime without the CLI: build an engine config in code
//! and hand it to `bootstrap::run_from_config`.
//!
//! Build the example module first (`just build-module`), then run
//! `cargo run -p nexum-runtime --example embed` from the repo root.

use nexum_runtime::bootstrap;
use nexum_runtime::engine_config::{EngineConfig, ModuleEntry};

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

    bootstrap::run_from_config(&cfg, None, None).await
}
