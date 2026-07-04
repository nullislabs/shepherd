//! Embed the runtime without the CLI: build an engine config plus the
//! shared backends in code and hand them to the generic `bootstrap::run`.
//!
//! This assembles a core-only lattice (no domain extension). A domain
//! capability such as cow-api is added by depending on its extension crate
//! and passing its `Extension` value here, exactly as the CLI does.
//!
//! Build the example module first (`just build-module`), then run
//! `cargo run -p nexum-runtime --example embed` from the repo root.

use nexum_runtime::bootstrap;
use nexum_runtime::engine_config::{EngineConfig, ModuleEntry};
use nexum_runtime::host::component::{Components, RuntimeTypes, SystemClock};
use nexum_runtime::host::local_store_redb::LocalStore;
use nexum_runtime::host::provider_pool::ProviderPool;

/// Core-only lattice: the reference core backends with an empty extension
/// slot (`Ext = ()`).
#[derive(Debug, Clone, Copy, Default)]
struct CoreTypes;

impl RuntimeTypes for CoreTypes {
    type Chain = ProviderPool;
    type Store = LocalStore;
    type Clock = SystemClock;
    type Ext = ();
}

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

    std::fs::create_dir_all(&cfg.engine.state_dir)?;
    let store = LocalStore::open(cfg.engine.state_dir.join("local-store.redb"))?;
    let chain = ProviderPool::from_config(&cfg).await?;
    let components = Components::<CoreTypes> {
        chain,
        store,
        ext: (),
    };

    bootstrap::run::<CoreTypes>(&cfg, None, None, &components, &[]).await
}
