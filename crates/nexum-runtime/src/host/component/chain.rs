//! Chain backend seam: raw JSON-RPC dispatch plus block/log
//! subscriptions, mirroring the inherent `ProviderPool` API.

use std::future::Future;

use alloy_chains::Chain;
use alloy_rpc_types_eth::Filter;
use strum::{EnumString, IntoStaticStr};

use crate::host::provider_pool::{BlockStream, LogStream, ProviderError, ProviderPool};

/// The permitted JSON-RPC read surface as a closed type. Methods that
/// sign or mutate node state have no variant, so a guest-supplied
/// signing method (for example `eth_sign` or `eth_sendTransaction`)
/// cannot be represented and never reaches the provider. This is the
/// structural ceiling; an operator allowlist narrows within it and
/// never widens it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr)]
pub enum ChainMethod {
    #[strum(serialize = "eth_blockNumber")]
    EthBlockNumber,
    #[strum(serialize = "eth_call")]
    EthCall,
    #[strum(serialize = "eth_chainId")]
    EthChainId,
    #[strum(serialize = "eth_estimateGas")]
    EthEstimateGas,
    #[strum(serialize = "eth_feeHistory")]
    EthFeeHistory,
    #[strum(serialize = "eth_gasPrice")]
    EthGasPrice,
    #[strum(serialize = "eth_maxPriorityFeePerGas")]
    EthMaxPriorityFeePerGas,
    #[strum(serialize = "eth_getBalance")]
    EthGetBalance,
    #[strum(serialize = "eth_getBlockByHash")]
    EthGetBlockByHash,
    #[strum(serialize = "eth_getBlockByNumber")]
    EthGetBlockByNumber,
    #[strum(serialize = "eth_getBlockReceipts")]
    EthGetBlockReceipts,
    #[strum(serialize = "eth_getCode")]
    EthGetCode,
    #[strum(serialize = "eth_getLogs")]
    EthGetLogs,
    #[strum(serialize = "eth_getProof")]
    EthGetProof,
    #[strum(serialize = "eth_getStorageAt")]
    EthGetStorageAt,
    #[strum(serialize = "eth_getTransactionByHash")]
    EthGetTransactionByHash,
    #[strum(serialize = "eth_getTransactionCount")]
    EthGetTransactionCount,
    #[strum(serialize = "eth_getTransactionReceipt")]
    EthGetTransactionReceipt,
    #[strum(serialize = "net_version")]
    NetVersion,
}

impl ChainMethod {
    /// The wire method name forwarded to the provider. `&'static`
    /// because the permitted set is closed, so the name drops straight
    /// into alloy's `Cow<'static, str>` method slot without allocating.
    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

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

    /// Raw JSON-RPC dispatch. `method` is a permitted read-surface
    /// method; `params_json` is the JSON params array.
    fn request(
        &self,
        chain: Chain,
        method: ChainMethod,
        params_json: String,
    ) -> impl Future<Output = Result<String, ProviderError>> + Send;

    /// Fetch the latest block number.
    fn get_block_number(
        &self,
        chain: Chain,
    ) -> impl Future<Output = Result<u64, ProviderError>> + Send;

    /// Fetch historical logs matching `filter`.
    fn get_logs(
        &self,
        chain: Chain,
        filter: Filter,
    ) -> impl Future<Output = Result<Vec<alloy_rpc_types_eth::Log>, ProviderError>> + Send;
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
        method: ChainMethod,
        params_json: String,
    ) -> impl Future<Output = Result<String, ProviderError>> + Send {
        ProviderPool::request(self, chain, method, params_json)
    }

    fn get_block_number(
        &self,
        chain: Chain,
    ) -> impl Future<Output = Result<u64, ProviderError>> + Send {
        ProviderPool::get_block_number(self, chain)
    }

    fn get_logs(
        &self,
        chain: Chain,
        filter: Filter,
    ) -> impl Future<Output = Result<Vec<alloy_rpc_types_eth::Log>, ProviderError>> + Send {
        ProviderPool::get_logs(self, chain, filter)
    }
}

#[cfg(test)]
mod tests {
    use super::ChainMethod;

    #[test]
    fn read_surface_methods_parse() {
        for m in [
            "eth_call",
            "eth_blockNumber",
            "eth_getBalance",
            "eth_getLogs",
            "eth_getTransactionReceipt",
            "net_version",
        ] {
            assert!(ChainMethod::try_from(m).is_ok(), "{m} should parse");
        }
    }

    #[test]
    fn signing_and_mutating_methods_have_no_variant() {
        for m in [
            "eth_sign",
            "eth_signTransaction",
            "eth_sendTransaction",
            "eth_sendRawTransaction",
            "eth_accounts",
            "personal_sign",
            "personal_unlockAccount",
            "admin_peers",
            "debug_traceCall",
            "miner_start",
            "eth_notAMethod",
            "",
        ] {
            assert!(ChainMethod::try_from(m).is_err(), "{m} must be rejected");
        }
    }

    #[test]
    fn as_str_round_trips_the_wire_name() {
        assert_eq!(ChainMethod::EthCall.as_str(), "eth_call");
        assert_eq!(
            ChainMethod::try_from(ChainMethod::EthGetBalance.as_str()).unwrap(),
            ChainMethod::EthGetBalance,
        );
    }
}
