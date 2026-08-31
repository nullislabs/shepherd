//! `shepherd-engine`: the composition root. Assembles the reference
//! backends, registers the venues this binary compiles in, and hands the
//! preset to the generic launcher. The `shepherd` binary is one line over
//! this crate.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

// The launcher and the async runtime belong to the `shepherd` binary, not
// to this library, but the lint above runs per target and cannot see that.
// Naming them keeps it enforcing on the deps the library does own.
use nexum_launch as _;
use tokio as _;

pub mod venues;

use std::sync::Arc;

use nexum_runtime::addons::{AddOns, PrometheusAddOn};
use nexum_runtime::component::{
    ComponentsBuilder, LocalStoreBuilder, LogPipelineBuilder, ProviderPoolBuilder,
};
use nexum_runtime::config::EngineConfig;
use nexum_runtime::extension::Extension;
use nexum_runtime::{CoreRuntime, Runtime};

/// The shepherd preset: the reference core backends with the videre venue
/// platform and the Prometheus add-on.
#[derive(Debug, Clone, Copy, Default)]
pub struct ShepherdRuntime;

impl nexum_runtime::sealed::SealedRuntime for ShepherdRuntime {}

impl Runtime for ShepherdRuntime {
    type Types = CoreRuntime;
    type ChainBuilder = ProviderPoolBuilder;
    type StoreBuilder = LocalStoreBuilder;
    type LogsBuilder = LogPipelineBuilder;

    fn components(
        self,
    ) -> ComponentsBuilder<ProviderPoolBuilder, LocalStoreBuilder, LogPipelineBuilder> {
        ComponentsBuilder::new(ProviderPoolBuilder, LocalStoreBuilder)
    }

    fn add_ons(&self) -> AddOns {
        vec![Box::new(PrometheusAddOn)]
    }

    /// Registers the compiled-in venues on the platform's registry before
    /// the engine boots. The hook cannot report an error, so a venue the
    /// operator misconfigured stops the process here instead of reaching a
    /// launch that cannot route it. Exiting rather than panicking keeps the
    /// message one operator-facing line, as `RunError` would have rendered
    /// it.
    fn extensions(&self, config: &EngineConfig) -> Vec<Arc<dyn Extension<CoreRuntime>>> {
        let videre = videre_host::platform();
        if let Err(err) = venues::register(videre.registry(), config) {
            eprintln!("shepherd: venue registration refused: {err:#}");
            std::process::exit(1);
        }
        vec![Arc::new(videre)]
    }
}
