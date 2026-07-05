//! Composition root: bind the reference lattice (core backends plus the
//! cow-api extension in the `Ext` slot), build the shared backends and the
//! extension list, then hand off to the generic runtime launch.

use std::path::Path;

use nexum_runtime::engine_config::EngineConfig;
use nexum_runtime::host::component::{Components, RuntimeTypes};
use nexum_runtime::host::local_store_redb::LocalStore;
use nexum_runtime::host::provider_pool::ProviderPool;
use shepherd_cow_host::{OrderBookPool, ReferenceExt, extension};

/// The backends the reference engine ships: the core seams plus the
/// cow-api extension payload in the [`Ext`](RuntimeTypes::Ext) slot.
#[derive(Debug, Clone, Copy, Default)]
struct ReferenceTypes;

impl RuntimeTypes for ReferenceTypes {
    type Chain = ProviderPool;
    type Store = LocalStore;
    type Ext = ReferenceExt;
}

/// Build the reference backends and extension list, then run until shutdown.
pub async fn run_from_config(
    engine_cfg: &EngineConfig,
    wasm: Option<&Path>,
    manifest: Option<&Path>,
) -> anyhow::Result<()> {
    // Surface config footguns now that the tracing subscriber is up.
    // Today's only check: an HTTP `rpc_url` would loop forever in the
    // event-loop's WS reconnect backoff because `eth_subscribe` is
    // WS-only. See `engine_config::validate_transports`.
    engine_cfg.validate_transports();

    // Bring up shared host backends.
    std::fs::create_dir_all(&engine_cfg.engine.state_dir).map_err(|e| {
        anyhow::anyhow!(
            "create state directory {}: {e}",
            engine_cfg.engine.state_dir.display()
        )
    })?;
    let store_path = engine_cfg.engine.state_dir.join("local-store.redb");
    let local_store = LocalStore::open(&store_path)
        .map_err(|e| anyhow::anyhow!("open local-store at {}: {e}", store_path.display()))?;
    let cow_pool = OrderBookPool::from_config(engine_cfg)?;
    let provider_pool = ProviderPool::from_config(engine_cfg).await?;

    // Wire cow-api as an extension: linker hook plus capability namespace.
    // The core runtime knows nothing of cow; it plugs in here at the
    // composition root.
    let extensions = [extension::<ReferenceTypes>()];

    // Bundle the shared backends the supervisor threads into every store.
    // The cow backend lives in the extension slot. The log pipeline is
    // sized by `[limits.logs]`; the same handle serves the embedder's
    // run/log read side.
    let logs = nexum_runtime::host::logs::LogPipeline::in_memory(engine_cfg.limits.logs());
    let components = Components::<ReferenceTypes> {
        chain: provider_pool,
        store: local_store,
        ext: ReferenceExt { cow: cow_pool },
        logs,
    };

    nexum_runtime::bootstrap::run::<ReferenceTypes>(
        engine_cfg,
        wasm,
        manifest,
        &components,
        &extensions,
    )
    .await
}
