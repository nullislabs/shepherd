//! Chain backend seam: raw JSON-RPC dispatch plus block/log
//! subscriptions, mirroring the inherent `ProviderPool` API.

use std::future::Future;

use alloy_chains::Chain;
use alloy_rpc_types_eth::Filter;

use crate::host::provider_pool::{BlockStream, LogStream, ProviderError, ProviderPool};

/// Async chain backend. Methods mirror [`ProviderPool`] one-to-one;
/// the `impl Future + Send` form bakes in the Send bound generic
/// consumers need across `.await` in tokio tasks (not dyn-compatible).
pub trait ChainProvider {
    /// Open a `newHeads` block subscription on `chain`.
    fn subscribe_blocks(
        &self,
        chain: Chain,
    ) -> impl Future<Output = Result<BlockStream, ProviderError>> + Send;

    /// Open an `eth_subscribe(logs, filter)` stream on `chain`.
    fn subscribe_logs(
        &self,
        chain: Chain,
        filter: Filter,
    ) -> impl Future<Output = Result<LogStream, ProviderError>> + Send;

    /// Raw JSON-RPC dispatch; `params_json` is the JSON params array.
    fn request(
        &self,
        chain: Chain,
        method: String,
        params_json: String,
    ) -> impl Future<Output = Result<String, ProviderError>> + Send;
}

impl ChainProvider for ProviderPool {
    fn subscribe_blocks(
        &self,
        chain: Chain,
    ) -> impl Future<Output = Result<BlockStream, ProviderError>> + Send {
        ProviderPool::subscribe_blocks(self, chain)
    }

    fn subscribe_logs(
        &self,
        chain: Chain,
        filter: Filter,
    ) -> impl Future<Output = Result<LogStream, ProviderError>> + Send {
        ProviderPool::subscribe_logs(self, chain, filter)
    }

    fn request(
        &self,
        chain: Chain,
        method: String,
        params_json: String,
    ) -> impl Future<Output = Result<String, ProviderError>> + Send {
        ProviderPool::request(self, chain, method, params_json)
    }
}
