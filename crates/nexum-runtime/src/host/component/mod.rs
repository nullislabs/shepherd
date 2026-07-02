//! Backend component traits: the seam between the WIT host impls and
//! the concrete capability backends. Implemented here for the existing
//! pools; a later runtime-generic layer consumes them via generic
//! bounds (the async traits are not dyn-compatible by design).

mod chain;
mod clock;
mod cow;
mod http;
mod random;
mod state;

pub use chain::ChainProvider;
pub use clock::{Clock, SystemClock};
pub use cow::CowApi;
pub use http::{HttpClient, UnsupportedHttp};
pub use random::{OsRandom, Random};
pub use state::{StateHandle, StateStore};

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
    fn random<T: Random>() {}
    fn http<T: HttpClient>() {}

    #[test]
    fn concrete_backends_satisfy_the_traits() {
        chain::<ProviderPool>();
        cow::<OrderBookPool>();
        store::<LocalStore>();
        handle::<ModuleStore>();
        clock::<SystemClock>();
        random::<OsRandom>();
        http::<UnsupportedHttp>();
    }

    #[tokio::test]
    async fn trait_dispatch_matches_inherent_dispatch() {
        let pool = ProviderPool::empty();
        let err = ChainProvider::request(&pool, 1, "eth_blockNumber".into(), "[]".into())
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            crate::host::provider_pool::ProviderError::UnknownChain(1)
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

    #[test]
    fn os_random_fills_bytes() {
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        OsRandom.fill(&mut a).expect("csprng");
        OsRandom.fill(&mut b).expect("csprng");
        assert_ne!(a, b, "two 32-byte CSPRNG draws must differ");
    }
}
