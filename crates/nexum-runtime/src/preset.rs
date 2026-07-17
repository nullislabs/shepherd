//! Runtime presets: a preset names a lattice, its component builders, its
//! extensions, and its add-on set as one bundle, so an embedder launches with
//! `RuntimeBuilder::new(cfg).runtime::<Preset>().launch()` instead of naming
//! each seam. A preset carrying pre-built backends or non-static extensions
//! binds by value through
//! [`RuntimeBuilder::with_runtime`](crate::builder::RuntimeBuilder::with_runtime).
//! [`CoreRuntime`] is the domain-free default: the reference core backends
//! (a chain provider pool and a local redb store, no extension payload) with
//! the Prometheus add-on. A domain assembly ships its own preset naming its
//! extension builder in the `Ext` slot and returning its linker extensions.

use std::sync::Arc;

use crate::addons::{AddOns, PrometheusAddOn};
use crate::engine_config::EngineConfig;
use crate::host::component::{
    ComponentBuilder, ComponentsBuilder, LocalStoreBuilder, LogPipelineBuilder,
    ProviderPoolBuilder, RuntimeTypes,
};
use crate::host::extension::Extension;
use crate::host::local_store_redb::LocalStore;
use crate::host::logs::LogPipeline;
use crate::host::provider_pool::ProviderPool;

/// A bundled runtime assembly: the [`RuntimeTypes`] lattice plus the
/// component builders, extensions, and add-ons the launcher needs, gathered
/// behind one name.
/// [`RuntimeBuilder::runtime`](crate::builder::RuntimeBuilder::runtime) binds
/// a `Default` marker;
/// [`RuntimeBuilder::with_runtime`](crate::builder::RuntimeBuilder::with_runtime)
/// binds a value, so a preset can hand back already-built backends through a
/// pass-through builder such as `Prebuilt`.
pub trait Runtime {
    /// The lattice the preset assembles.
    type Types: RuntimeTypes;
    /// Builds the chain backend ([`RuntimeTypes::Chain`]).
    type ChainBuilder: ComponentBuilder<Output = <Self::Types as RuntimeTypes>::Chain>;
    /// Builds the store backend ([`RuntimeTypes::Store`]).
    type StoreBuilder: ComponentBuilder<Output = <Self::Types as RuntimeTypes>::Store>;
    /// Builds the extension payload ([`RuntimeTypes::Ext`]).
    type ExtBuilder: ComponentBuilder<Output = <Self::Types as RuntimeTypes>::Ext>;
    /// Builds the shared [`LogPipeline`].
    type LogsBuilder: ComponentBuilder<Output = LogPipeline>;

    /// The component builders that open the backends at launch. Consumes the
    /// preset, so a value-bound preset hands over owned, pre-built backends.
    fn components(
        self,
    ) -> ComponentsBuilder<
        Self::ChainBuilder,
        Self::StoreBuilder,
        Self::ExtBuilder,
        Self::LogsBuilder,
    >;

    /// The cross-cutting add-ons installed before the engine boots.
    fn add_ons(&self) -> AddOns;

    /// The extensions the preset launches with, derived from the loaded
    /// config so an extension can carry config-resolved policy. None by
    /// default;
    /// [`PresetBuilder::with_extensions`](crate::builder::PresetBuilder::with_extensions)
    /// appends on top.
    fn extensions(&self, config: &EngineConfig) -> Vec<Arc<dyn Extension<Self::Types>>> {
        let _ = config;
        Vec::new()
    }
}

/// The domain-free default preset: the reference core backends (a chain
/// provider pool and a local redb store, no extension payload) with the
/// Prometheus add-on. Doubles as its own [`RuntimeTypes`] lattice.
#[derive(Debug, Clone, Copy, Default)]
pub struct CoreRuntime;

impl RuntimeTypes for CoreRuntime {
    type Chain = ProviderPool;
    type Store = LocalStore;
    type Ext = ();
}

impl Runtime for CoreRuntime {
    type Types = Self;
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
}
