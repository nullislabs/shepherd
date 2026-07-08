//! Composition root: bind the reference lattice (core backends plus the
//! cow-api extension in the `Ext` slot), build the shared backends and the
//! extension list, then hand off to the generic runtime launch.

use std::path::Path;

use nexum_runtime::addons::{PrometheusAddOn, RuntimeAddOn};
use nexum_runtime::builder::RuntimeBuilder;
use nexum_runtime::engine_config::EngineConfig;
use nexum_runtime::host::component::{
    ComponentsBuilder, LocalStoreBuilder, ProviderPoolBuilder, RuntimeTypes,
};
use nexum_runtime::host::local_store_redb::LocalStore;
use nexum_runtime::host::provider_pool::ProviderPool;
use nexum_runtime::runtime::task::TokioExecutor;
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

    // Attach the reference add-on set. The binary ships the Prometheus
    // exporter; an embedder omits or replaces it by choosing a different
    // list here.
    let add_ons: [&dyn RuntimeAddOn; 1] = [&PrometheusAddOn];

    // Assemble and launch over the type-state builder: bind the reference
    // lattice, wire cow-api as an extension (linker hook plus capability
    // namespace; the core runtime knows nothing of cow, it plugs in here),
    // name the component builders, then drive to a running handle and block
    // until the event loop returns on shutdown.
    RuntimeBuilder::new(engine_cfg)
        .with_types::<ReferenceTypes>()
        .with_extensions([extension::<ReferenceTypes>()])
        .with_module_source(wasm.map(Path::to_path_buf), manifest.map(Path::to_path_buf))
        // The launch root is the executor-selection seam: the binary spawns on
        // tokio, while an embedder or a non-tokio target substitutes its own
        // executor here.
        .with_executor(&TokioExecutor)
        .with_components(ComponentsBuilder::new(
            ProviderPoolBuilder,
            LocalStoreBuilder,
            ReferenceExtBuilder,
        ))
        .with_add_ons(&add_ons)
        .launch()
        .await?
        .wait()
        .await
}
