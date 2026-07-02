//! Backend component traits: the seam between the WIT host impls and
//! the concrete capability backends. Implemented here for the existing
//! pools; the runtime-generic `HostState` consumes them via generic
//! bounds (the async traits are not dyn-compatible by design).

mod chain;
mod clock;
mod cow;
mod http;
mod state;

pub use chain::ChainProvider;
pub use clock::{Clock, SystemClock};
pub use cow::CowApi;
// `self::` disambiguates the local `http` module from the `http` crate.
pub use self::http::{HttpClient, HttpError, UnsupportedHttp};
pub use state::{StateHandle, StateStore};

/// Owned bundle of the shared backends the supervisor threads into
/// every module store. All members are cheap Arc-backed clones.
#[derive(Clone)]
pub struct Components<C, W, S, H> {
    pub chain: C,
    pub cow: W,
    pub store: S,
    pub http: H,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::cow_orderbook::OrderBookPool;
    use crate::host::local_store_redb::{LocalStore, ModuleStore};
    use crate::host::provider_pool::ProviderPool;

    fn chain<T: ChainProvider>() {}
    fn cow<T: CowApi>() {}
    fn store<T: StateStore>() {}
    fn handle<T: StateHandle>() {}
    fn clock<T: Clock>() {}
    fn http<T: HttpClient>() {}

    #[test]
    fn concrete_backends_satisfy_the_traits() {
        chain::<ProviderPool>();
        cow::<OrderBookPool>();
        store::<LocalStore>();
        handle::<ModuleStore>();
        clock::<SystemClock>();
        http::<UnsupportedHttp>();
    }

    #[tokio::test]
    async fn chain_provider_trait_delegates_to_the_pool() {
        use alloy_chains::Chain;
        let pool = ProviderPool::empty();
        let err = ChainProvider::request(
            &pool,
            Chain::from_id(1),
            "eth_blockNumber".into(),
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
