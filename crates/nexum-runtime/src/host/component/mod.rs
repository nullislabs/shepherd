//! Backend component traits: the seam between the WIT host impls and the
//! concrete capability backends, tied together by the [`RuntimeTypes`]
//! lattice.

mod builder;
mod chain;
mod runtime_types;
mod state;

pub use builder::{
    BuildError, BuilderContext, ComponentBuilder, ComponentsBuilder, LocalStoreBuilder,
    LogPipelineBuilder, ProviderPoolBuilder,
};
pub use chain::{ChainMethod, ChainProvider};
pub use runtime_types::{Handle, RuntimeTypes};
pub use state::{StateHandle, StateStore};

/// Owned bundle of shared backends threaded into every module store; cheap to
/// clone.
pub struct Components<T: RuntimeTypes> {
    pub chain: T::Chain,
    pub store: T::Store,
    /// Extension backends (the lattice `Ext` payload).
    pub ext: T::Ext,
    /// Shared log pipeline.
    pub logs: crate::host::logs::LogPipeline,
}

impl<T: RuntimeTypes> Clone for Components<T> {
    fn clone(&self) -> Self {
        Self {
            chain: self.chain.clone(),
            store: self.store.clone(),
            ext: self.ext.clone(),
            logs: self.logs.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::local_store_redb::{LocalStore, ModuleStore};
    use crate::host::provider_pool::ProviderPool;

    /// Core-only lattice (no extension payload).
    #[derive(Clone, Copy, Default)]
    struct CoreTypes;

    impl crate::sealed::SealedRuntimeTypes for CoreTypes {}

    impl RuntimeTypes for CoreTypes {
        type Chain = ProviderPool;
        type Store = LocalStore;
        type Ext = ();
    }

    fn chain<T: ChainProvider>() {}
    fn store<T: StateStore>() {}
    fn handle<T: StateHandle>() {}
    fn lattice<T: RuntimeTypes>() {}

    #[test]
    fn concrete_backends_satisfy_the_traits() {
        chain::<ProviderPool>();
        store::<LocalStore>();
        handle::<ModuleStore>();
        lattice::<CoreTypes>();
    }

    #[tokio::test]
    async fn chain_provider_trait_delegates_to_the_pool() {
        use alloy_chains::Chain;
        let pool = ProviderPool::empty();
        let err = ChainProvider::request(
            &pool,
            Chain::from_id(1),
            ChainMethod::EthBlockNumber,
            "[]".into(),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            crate::host::provider_pool::ProviderError::UnknownChain(c) if c == Chain::from_id(1)
        ));
    }
}
