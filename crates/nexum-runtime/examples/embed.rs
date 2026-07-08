//! Embed the runtime without the CLI: point the builder at a loaded config
//! and a [`Runtime`] preset, then launch and run until shutdown.
//!
//! [`CoreRuntime`] is the domain-free preset: it bundles the reference core
//! backends (chain provider pool, local redb store, empty extension slot) and
//! the Prometheus add-on. A domain capability such as cow-api is added by
//! writing a preset that names its extension builder in the `Ext` slot and
//! its linker hook via `with_extensions`, or by dropping to the explicit
//! `with_components` builder path. That explicit path is also how an embedder
//! retains the in-process log read handle, by cloning `components.logs` after
//! the build.
//!
//! Build the example module first (`just build-module`), then run
//! `cargo run -p nexum-runtime --example embed` from the repo root.

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
    RuntimeBuilder::new(&cfg)
        .runtime::<CoreRuntime>()
        .launch()
        .await?
        .wait()
        .await
}
