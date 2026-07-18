//! Embed the runtime without the CLI: point the builder at a loaded config
//! and a [`Runtime`] preset, then launch and run until shutdown.
//!
//! [`CoreRuntime`] is the domain-free preset: it bundles the reference core
//! backends (chain provider pool, local redb store, empty extension slot) and
//! the Prometheus add-on. A domain capability is added by
//! writing a preset that names its extension builder in the `Ext` slot and
//! returns its linker extensions, or by dropping to the explicit
//! `with_components` builder path. The returned [`RuntimeHandle`] carries the
//! in-process log read side; clone it to keep reading after `wait` consumes
//! the handle.
//!
//! Build the example module first (`just build-module`), then run
//! `cargo run -p nexum-runtime --example embed` from the repo root.
//!
//! [`Runtime`]: nexum_runtime::preset::Runtime
//! [`RuntimeHandle`]: nexum_runtime::builder::RuntimeHandle

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
            manifest: Some("nexum/modules/example/module.toml".into()),
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
