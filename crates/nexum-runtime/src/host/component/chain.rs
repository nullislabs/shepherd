//! Chain backend seam: raw JSON-RPC dispatch plus block/chain-log
//! subscriptions, mirroring the inherent `ProviderPool` API.

use std::future::Future;

use alloy_chains::Chain;
use alloy_rpc_types_eth::Filter;

use crate::host::provider_pool::{BlockStream, CanonicalLogStream, ProviderError, ProviderPool};

/// Permitted read surface, re-exported from `nexum-world`.
pub use nexum_world::ChainMethod;

/// Async chain backend; methods mirror [`ProviderPool`].
pub trait ChainProvider {
    /// Open a `newHeads` block subscription on `chain`.
    fn subscribe_blocks(
        &self,
        chain: Chain,
    ) -> impl Future<Output = Result<BlockStream, ProviderError>> + Send;

    /// Current head block number (`eth_blockNumber`).
    fn block_number(&self, chain: Chain)
    -> impl Future<Output = Result<u64, ProviderError>> + Send;

    /// Open a canonical (reorg-aware) `eth_getLogs` log poller on
    /// `chain` from `start_block`.
    fn watch_chain_logs(
        &self,
        chain: Chain,
        filter: Filter,
        start_block: u64,
    ) -> Result<CanonicalLogStream, ProviderError>;

    /// Raw JSON-RPC dispatch; `params_json` is the JSON params array.
    fn request(
        &self,
        chain: Chain,
        method: ChainMethod,
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

    fn block_number(
        &self,
        chain: Chain,
    ) -> impl Future<Output = Result<u64, ProviderError>> + Send {
        ProviderPool::block_number(self, chain)
    }

    fn watch_chain_logs(
        &self,
        chain: Chain,
        filter: Filter,
        start_block: u64,
    ) -> Result<CanonicalLogStream, ProviderError> {
        ProviderPool::watch_chain_logs(self, chain, filter, start_block)
    }

    fn request(
        &self,
        chain: Chain,
        method: ChainMethod,
        params_json: String,
    ) -> impl Future<Output = Result<String, ProviderError>> + Send {
        ProviderPool::request(self, chain, method, params_json)
    }
}
