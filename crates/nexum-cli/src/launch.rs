//! Composition root: bind the reference lattice (core backends plus the
//! cow-api extension in the `Ext` slot), build the shared backends and the
//! extension list, then hand off to the generic runtime launch.

use std::path::Path;

use nexum_runtime::engine_config::EngineConfig;
use nexum_runtime::host::component::{
    BuilderContext, ComponentsBuilder, LocalStoreBuilder, ProviderPoolBuilder, RuntimeTypes,
};
use nexum_runtime::host::local_store_redb::LocalStore;
use nexum_runtime::host::provider_pool::ProviderPool;
use shepherd_cow_host::{ReferenceExt, ReferenceExtBuilder, extension};

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

    // Bring up shared host backends through the component builders. The
    // context carries the loaded config and the data directory backends
    // root their on-disk state at; each builder opens one backend and the
    // assembler bundles them (plus the `[limits.logs]`-sized pipeline).
    let ctx = BuilderContext {
        config: engine_cfg,
        data_dir: &engine_cfg.engine.state_dir,
    };
    let components =
        ComponentsBuilder::new(ProviderPoolBuilder, LocalStoreBuilder, ReferenceExtBuilder)
            .build::<ReferenceTypes>(&ctx)
            .await?;

    // Wire cow-api as an extension: linker hook plus capability namespace.
    // The core runtime knows nothing of cow; it plugs in here at the
    // composition root.
    let extensions = [extension::<ReferenceTypes>()];

    nexum_runtime::bootstrap::run::<ReferenceTypes>(
        engine_cfg,
        wasm,
        manifest,
        &components,
        &extensions,
    )
    .await
}
