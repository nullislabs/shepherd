//! Engine-side mock backends for launching an in-process runtime entirely on
//! fakes.
//!
//! [`MockChainProvider`] and [`MockStateStore`] implement the component seam
//! traits with no network and no disk; [`Prebuilt`] wraps a pre-built instance
//! as a [`ComponentBuilder`](crate::host::component::ComponentBuilder); and
//! [`MockTypes`] is the domain-free lattice that ties them together. The
//! assembly composes through the public builder path:
//!
//! ```no_run
//! # use nexum_runtime::builder::RuntimeBuilder;
//! # use nexum_runtime::engine_config::EngineConfig;
//! # use nexum_runtime::host::component::ComponentsBuilder;
//! # use nexum_runtime::test_utils::{MockChainProvider, MockStateStore, MockTypes, Prebuilt};
//! # async fn demo(config: &EngineConfig) -> anyhow::Result<()> {
//! let chain = MockChainProvider::new();
//! let store = MockStateStore::new();
//! let _handle = RuntimeBuilder::new(config)
//!     .with_types::<MockTypes>()
//!     .with_components(ComponentsBuilder::new(
//!         Prebuilt(chain.clone()),
//!         Prebuilt(store.clone()),
//!         (),
//!     ))
//!     .with_add_ons(&[])
//!     .launch()
//!     .await?;
//! # Ok(())
//! # }
//! ```
//!
//! The caller keeps its own clones of `chain` / `store` to program responses
//! and assert on what a module wrote.

mod builders;
mod chain;
pub mod clock;
mod store;
mod types;

pub use builders::Prebuilt;
pub use chain::{MockChainProvider, RecordedRequest};
pub use store::{MockStateHandle, MockStateStore};
pub use types::MockTypes;

use crate::engine_config::ModuleLimits;
use crate::host::component::Components;
use crate::host::logs::LogPipeline;

/// A [`Components`] bundle over fresh mock backends, ready for
/// [`Supervisor::boot`](crate::supervisor::Supervisor::boot).
pub fn mock_components() -> Components<MockTypes> {
    mock_components_from(MockChainProvider::new(), MockStateStore::new())
}

/// A [`Components`] bundle over the given mock backends, with an empty
/// extension slot and an in-memory log pipeline. Pass clones the caller
/// retains for programming and assertion.
pub fn mock_components_from(
    chain: MockChainProvider,
    store: MockStateStore,
) -> Components<MockTypes> {
    Components {
        chain,
        store,
        ext: (),
        logs: LogPipeline::in_memory(ModuleLimits::default().logs()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_chains::Chain;
    use futures::StreamExt as _;

    use crate::builder::RuntimeBuilder;
    use crate::engine_config::EngineConfig;
    use crate::host::component::{
        ChainMethod, ChainProvider, ComponentsBuilder, StateHandle, StateStore,
    };
    use crate::host::provider_pool::ProviderError;

    /// M0 acceptance: a custom component set launched entirely through the
    /// public builder, on fakes, with no CLI, no disk, and no network. The
    /// launch reaches supervisor boot and bails only because the default
    /// config declares no modules, which proves the mock backends composed
    /// and the build path ran end to end.
    #[tokio::test]
    async fn m0_custom_component_set_launches_through_the_public_builder() {
        let chain = MockChainProvider::new();
        chain.on_method(ChainMethod::EthBlockNumber, "\"0x10\"");
        let store = MockStateStore::new();

        let config = EngineConfig::default();
        let err = match RuntimeBuilder::new(&config)
            .with_types::<MockTypes>()
            .with_components(ComponentsBuilder::new(
                Prebuilt(chain.clone()),
                Prebuilt(store),
                (),
            ))
            .with_add_ons(&[])
            .launch()
            .await
        {
            Ok(_) => panic!("default config declares no modules; launch must bail"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("no modules to run"), "{err}");

        // The fake actually serves and records, independent of the launch.
        let body = ChainProvider::request(
            &chain,
            Chain::from_id(1),
            ChainMethod::EthBlockNumber,
            "[]".into(),
        )
        .await
        .expect("programmed response");
        assert_eq!(body, "\"0x10\"");
        assert_eq!(chain.recorded_requests().len(), 1);
    }

    /// The exact `(method, params)` programming wins over a method wildcard,
    /// and an unprogrammed method surfaces a provider error.
    #[tokio::test]
    async fn request_routing_prefers_exact_then_wildcard() {
        let chain = MockChainProvider::new();
        chain
            .on_method(ChainMethod::EthCall, "\"wild\"")
            .on_request(ChainMethod::EthCall, "[\"0x1\"]", "\"exact\"");

        let exact = ChainProvider::request(
            &chain,
            Chain::from_id(1),
            ChainMethod::EthCall,
            "[\"0x1\"]".into(),
        )
        .await
        .expect("exact");
        assert_eq!(exact, "\"exact\"");

        let wild =
            ChainProvider::request(&chain, Chain::from_id(1), ChainMethod::EthCall, "[]".into())
                .await
                .expect("wildcard");
        assert_eq!(wild, "\"wild\"");

        let err = ChainProvider::request(
            &chain,
            Chain::from_id(1),
            ChainMethod::EthChainId,
            "[]".into(),
        )
        .await
        .expect_err("unprogrammed method has no response");
        assert!(
            matches!(err, ProviderError::UnknownChain(c) if c == Chain::from_id(1)),
            "unprogrammed method surfaces UnknownChain, got: {err:?}",
        );
    }

    /// A pushed header reaches the open block subscription.
    #[tokio::test]
    async fn subscribe_blocks_yields_pushed_headers() {
        let chain = MockChainProvider::new();
        let mut stream = ChainProvider::subscribe_blocks(&chain, Chain::from_id(1))
            .await
            .expect("block stream");
        chain.push_block(alloy_rpc_types_eth::Header::default());
        let item = stream.next().await.expect("one item");
        assert!(item.is_ok(), "pushed header arrives as Ok");
    }

    /// The store round-trips values, isolates namespaces, lists by prefix, and
    /// rejects the empty namespace.
    #[test]
    fn store_roundtrips_and_isolates_namespaces() {
        let store = MockStateStore::new();
        let a = store.module("mod-a").expect("namespace a");
        let b = store.module("mod-b").expect("namespace b");

        a.set("k", b"va").expect("set a");
        b.set("k", b"vb").expect("set b");
        assert_eq!(a.get("k").expect("get a").as_deref(), Some(&b"va"[..]));
        assert_eq!(b.get("k").expect("get b").as_deref(), Some(&b"vb"[..]));

        a.set("k2", b"x").expect("set a k2");
        assert_eq!(
            a.list_keys("k").expect("list a"),
            vec!["k".to_owned(), "k2".to_owned()],
        );

        a.delete("k").expect("delete a k");
        assert!(a.get("k").expect("get a k").is_none());

        assert!(store.module("").is_err(), "empty namespace rejected");
    }
}
