//! Backend component traits: the seam between the WIT host impls and
//! the concrete capability backends. Implemented here for the existing
//! pools; the runtime-generic `HostState` consumes them via generic
//! bounds (the async traits are not dyn-compatible by design). The
//! [`RuntimeTypes`] lattice ties the seams into one parameter.

mod chain;
mod clock;
mod runtime_types;
mod state;

pub use chain::{ChainMethod, ChainProvider};
pub use clock::{Clock, SystemClock};
pub use runtime_types::{Handle, RuntimeTypes};
pub use state::{StateHandle, StateStore};

/// Owned bundle of the shared backends the supervisor threads into
/// every module store. All members are cheap Arc-backed clones.
pub struct Components<T: RuntimeTypes> {
    pub chain: T::Chain,
    pub store: T::Store,
    /// Extension backends (the lattice `Ext` payload), threaded into
    /// `HostState.ext` and reached by extensions through `ExtState`.
    pub ext: T::Ext,
}

impl<T: RuntimeTypes> Clone for Components<T> {
    fn clone(&self) -> Self {
        Self {
            chain: self.chain.clone(),
            store: self.store.clone(),
            ext: self.ext.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::local_store_redb::{LocalStore, ModuleStore};
    use crate::host::provider_pool::ProviderPool;

    /// Core-only lattice (no extension payload) so the trait bounds are
    /// exercised without depending on any domain extension crate.
    #[derive(Clone, Copy, Default)]
    struct CoreTypes;

    impl RuntimeTypes for CoreTypes {
        type Chain = ProviderPool;
        type Store = LocalStore;
        type Clock = SystemClock;
        type Ext = ();
    }

    fn chain<T: ChainProvider>() {}
    fn store<T: StateStore>() {}
    fn handle<T: StateHandle>() {}
    fn clock<T: Clock>() {}
    fn lattice<T: RuntimeTypes>() {}

    #[test]
    fn concrete_backends_satisfy_the_traits() {
        chain::<ProviderPool>();
        store::<LocalStore>();
        handle::<ModuleStore>();
        clock::<SystemClock>();
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

    #[test]
    fn system_clock_behaves_like_the_direct_calls() {
        let clk = SystemClock::new();
        assert!(clk.now_ms() > 0);
        let a = clk.monotonic_ns();
        let b = clk.monotonic_ns();
        assert!(b >= a);
    }
}
