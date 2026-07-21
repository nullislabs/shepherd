//! The `shepherd` binary: the cow composition root. Binds the reference
//! lattice with the cow-api extension payload in the `Ext` slot, registers
//! the videre venue platform, and hands it all to the generic launcher;
//! the engine itself stays venue- and cow-free.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

use std::sync::Arc;

use nexum_runtime::addons::{AddOns, PrometheusAddOn};
use nexum_runtime::engine_config::EngineConfig;
use nexum_runtime::host::component::{
    ComponentsBuilder, LocalStoreBuilder, LogPipelineBuilder, ProviderPoolBuilder, RuntimeTypes,
};
use nexum_runtime::host::extension::Extension;
use nexum_runtime::host::local_store_redb::LocalStore;
use nexum_runtime::host::provider_pool::ProviderPool;
use nexum_runtime::preset::Runtime;
use shepherd_cow_host::{ReferenceExt, ReferenceExtBuilder, extension};

/// The reference lattice: the core backends with the cow-api payload in
/// the `Ext` slot.
#[derive(Debug, Clone, Copy, Default)]
struct ReferenceTypes;

impl RuntimeTypes for ReferenceTypes {
    type Chain = ProviderPool;
    type Store = LocalStore;
    type Ext = ReferenceExt;
}

/// The cow preset: reference backends, the videre venue platform, the
/// cow-api extension, and the Prometheus add-on.
#[derive(Debug, Clone, Copy, Default)]
struct ShepherdRuntime;

impl Runtime for ShepherdRuntime {
    type Types = ReferenceTypes;
    type ChainBuilder = ProviderPoolBuilder;
    type StoreBuilder = LocalStoreBuilder;
    type ExtBuilder = ReferenceExtBuilder;
    type LogsBuilder = LogPipelineBuilder;

    fn components(
        self,
    ) -> ComponentsBuilder<ProviderPoolBuilder, LocalStoreBuilder, ReferenceExtBuilder> {
        ComponentsBuilder::new(ProviderPoolBuilder, LocalStoreBuilder, ReferenceExtBuilder)
    }

    fn add_ons(&self) -> AddOns {
        vec![Box::new(PrometheusAddOn)]
    }

    fn extensions(&self, config: &EngineConfig) -> Vec<Arc<dyn Extension<ReferenceTypes>>> {
        vec![
            Arc::new(videre_host::platform(config)),
            extension::<ReferenceTypes>(),
        ]
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    nexum_launch::run("shepherd", ShepherdRuntime).await
}
