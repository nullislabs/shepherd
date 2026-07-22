//! `host.provider(chain)`: an alloy `Provider` over the chain host.

use std::future::{Future, IntoFuture};
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use alloy_provider::RootProvider;
use alloy_rpc_client::RpcClient;

use super::{Chain, HostTransport};
use crate::host::ChainHost;

/// Mints an alloy [`Provider`](alloy_provider::Provider) over
/// [`ChainHost::request`], so a strategy calls typed provider methods
/// instead of hand-building JSON-RPC. Blanket-implemented for every
/// cloneable [`ChainHost`]; drive the returned futures with
/// [`block_on`].
///
/// ```
/// use alloy_provider::Provider;
/// use nexum_sdk::chain::{Chain, ProviderHost, block_on};
/// use nexum_sdk::host::{ChainError, ChainHost};
///
/// #[derive(Clone)]
/// struct StubHost;
/// impl ChainHost for StubHost {
///     fn request(&self, _: u64, _: &str, _: &str) -> Result<String, ChainError> {
///         Ok("\"0x2a\"".into())
///     }
/// }
///
/// let provider = StubHost.provider(Chain::mainnet());
/// let block = block_on(provider.get_block_number()).unwrap();
/// assert_eq!(block, 42);
/// ```
pub trait ProviderHost: ChainHost + Clone + Send + Sync + Sized + 'static {
    /// Provider for `chain`, routed through the host's RPC stack.
    fn provider(&self, chain: Chain) -> RootProvider {
        RootProvider::new(RpcClient::new(
            HostTransport::new(self.clone(), chain),
            false,
        ))
    }
}

impl<H: ChainHost + Clone + Send + Sync + 'static> ProviderHost for H {}

/// Drive a host-backed provider future to completion. The host
/// transport is a synchronous WIT import, so the future resolves on the
/// first poll; a `Pending` means an async alloy layer crept in and the
/// chain SDK must move to a host-driven surface, not a poll loop.
pub fn block_on<F: IntoFuture>(future: F) -> F::Output {
    let mut future = pin!(future.into_future());
    let mut cx = Context::from_waker(Waker::noop());
    match future.as_mut().poll(&mut cx) {
        Poll::Ready(output) => output,
        Poll::Pending => panic!(
            "chain provider future did not resolve synchronously: the host \
             transport is a synchronous WIT import, so an alloy layer that \
             awaits a reactor or timer was added; the chain SDK must move \
             to a host-driven async surface, not a poll loop"
        ),
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Bytes, address};
    use alloy_provider::Provider;
    use alloy_rpc_types_eth::TransactionRequest;

    use super::{ProviderHost, block_on};
    use crate::chain::Chain;
    use crate::host::{ChainError, ChainHost};

    #[derive(Clone)]
    struct StubHost;

    impl ChainHost for StubHost {
        fn request(
            &self,
            chain_id: u64,
            method: &str,
            _params: &str,
        ) -> Result<String, ChainError> {
            assert_eq!(chain_id, 100);
            match method {
                "eth_blockNumber" => Ok("\"0x2a\"".into()),
                "eth_call" => Ok("\"0x1234\"".into()),
                other => panic!("unexpected method {other}"),
            }
        }
    }

    #[test]
    fn provider_reads_typed_values_through_the_host() {
        let provider = StubHost.provider(Chain::from_id(100));
        let block = block_on(provider.get_block_number()).expect("block number");
        assert_eq!(block, 42);
    }

    #[test]
    fn provider_call_decodes_bytes() {
        let provider = StubHost.provider(Chain::from_id(100));
        let tx = TransactionRequest::default()
            .to(address!("0x9008D19f58AAbD9eD0D60971565AA8510560ab41"));
        let out = block_on(provider.call(tx)).expect("eth_call");
        assert_eq!(out, Bytes::from(vec![0x12, 0x34]));
    }

    #[test]
    fn signing_methods_error_before_the_host() {
        let provider = StubHost.provider(Chain::from_id(100));
        let err = block_on(provider.raw_request::<_, String>("eth_sendRawTransaction".into(), ()))
            .expect_err("write method is rejected");
        let payload = err.as_error_resp().expect("json-rpc error response");
        assert_eq!(payload.code, -32601);
    }

    #[test]
    fn block_on_drives_plain_futures() {
        assert_eq!(block_on(async { 7 }), 7);
    }

    #[test]
    #[should_panic(expected = "did not resolve synchronously")]
    fn block_on_panics_when_a_future_is_not_synchronously_ready() {
        block_on(std::future::pending::<()>());
    }
}
