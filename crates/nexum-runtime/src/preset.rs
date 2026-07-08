//! Runtime presets: a preset names a lattice, its component builders, and its
//! add-on set as one bundle, so an embedder launches with
//! `RuntimeBuilder::new(cfg).runtime::<Preset>().launch()` instead of naming
//! each seam. [`CoreRuntime`] is the domain-free default: the reference core
//! backends (a chain provider pool and a local redb store, no extension
//! payload) with the Prometheus add-on. A domain assembly ships its own
//! preset naming its extension builder in the `Ext` slot.

use crate::addons::{AddOns, PrometheusAddOn};
use crate::host::component::{
    ComponentBuilder, ComponentsBuilder, LocalStoreBuilder, ProviderPoolBuilder, RuntimeTypes,
};
use crate::host::local_store_redb::LocalStore;
use crate::host::provider_pool::ProviderPool;

/// A bundled runtime assembly: the [`RuntimeTypes`] lattice plus the component
/// builders and add-ons the launcher needs, gathered behind one name.
/// Implemented by zero-sized markers;
/// [`RuntimeBuilder::runtime`](crate::builder::RuntimeBuilder::runtime) binds
/// one and launches it.
pub trait Runtime {
    /// The lattice the preset assembles.
    type Types: RuntimeTypes;
    /// Builds the chain backend ([`RuntimeTypes::Chain`]).
    type ChainBuilder: ComponentBuilder<Output = <Self::Types as RuntimeTypes>::Chain>;
    /// Builds the store backend ([`RuntimeTypes::Store`]).
    type StoreBuilder: ComponentBuilder<Output = <Self::Types as RuntimeTypes>::Store>;
    /// Builds the extension payload ([`RuntimeTypes::Ext`]).
    type ExtBuilder: ComponentBuilder<Output = <Self::Types as RuntimeTypes>::Ext>;

    /// The component builders that open the backends at launch.
    fn components() -> ComponentsBuilder<Self::ChainBuilder, Self::StoreBuilder, Self::ExtBuilder>;

    /// The cross-cutting add-ons installed before the engine boots.
    fn add_ons() -> AddOns;
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

    fn components() -> ComponentsBuilder<ProviderPoolBuilder, LocalStoreBuilder, ()> {
        ComponentsBuilder::new(ProviderPoolBuilder, LocalStoreBuilder, ())
    }

    fn add_ons() -> AddOns {
        vec![Box::new(PrometheusAddOn)]
    }
}
