//! The `shepherd` binary: the cow composition root. Boots the
//! reference backends, registers the videre venue platform, and hands
//! it all to the generic launcher; CoW enters only as the bundled
//! `cow-venue` adapter component, and the engine itself stays venue-
//! and cow-free.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

use std::sync::Arc;

use nexum_runtime::addons::{AddOns, PrometheusAddOn};
use nexum_runtime::engine_config::EngineConfig;
use nexum_runtime::host::component::{
    ComponentsBuilder, LocalStoreBuilder, LogPipelineBuilder, ProviderPoolBuilder,
};
use nexum_runtime::host::extension::Extension;
use nexum_runtime::preset::{CoreRuntime, Runtime};

/// The cow preset: the reference core backends with the videre venue
/// platform and the Prometheus add-on.
#[derive(Debug, Clone, Copy, Default)]
struct ShepherdRuntime;

impl nexum_runtime::sealed::SealedRuntime for ShepherdRuntime {}

impl Runtime for ShepherdRuntime {
    type Types = CoreRuntime;
    type ChainBuilder = ProviderPoolBuilder;
    type StoreBuilder = LocalStoreBuilder;
    type ExtBuilder = ();
    type LogsBuilder = LogPipelineBuilder;

    fn components(self) -> ComponentsBuilder<ProviderPoolBuilder, LocalStoreBuilder, ()> {
        ComponentsBuilder::new(ProviderPoolBuilder, LocalStoreBuilder, ())
    }

    fn add_ons(&self) -> AddOns {
        vec![Box::new(PrometheusAddOn)]
    }

    fn extensions(&self, config: &EngineConfig) -> Vec<Arc<dyn Extension<CoreRuntime>>> {
        vec![Arc::new(videre_host::platform(config))]
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    nexum_launch::run("shepherd", ShepherdRuntime).await
}
