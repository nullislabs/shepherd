//! The typed JSON-RPC method surface, guest side.

use strum::{EnumString, IntoStaticStr};

/// The permitted JSON-RPC read surface as a closed type, mirroring the
/// runtime's `ChainMethod` case for case. Signing and mutating methods
/// have no variant, so they cannot be represented and never cross the
/// WIT edge; [`HostTransport`](super::HostTransport) rejects anything
/// outside this set before calling the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumString, IntoStaticStr)]
pub enum ChainMethod {
    /// `eth_blockNumber`.
    #[strum(serialize = "eth_blockNumber")]
    EthBlockNumber,
    /// `eth_call`.
    #[strum(serialize = "eth_call")]
    EthCall,
    /// `eth_chainId`.
    #[strum(serialize = "eth_chainId")]
    EthChainId,
    /// `eth_estimateGas`.
    #[strum(serialize = "eth_estimateGas")]
    EthEstimateGas,
    /// `eth_feeHistory`.
    #[strum(serialize = "eth_feeHistory")]
    EthFeeHistory,
    /// `eth_gasPrice`.
    #[strum(serialize = "eth_gasPrice")]
    EthGasPrice,
    /// `eth_maxPriorityFeePerGas`.
    #[strum(serialize = "eth_maxPriorityFeePerGas")]
    EthMaxPriorityFeePerGas,
    /// `eth_getBalance`.
    #[strum(serialize = "eth_getBalance")]
    EthGetBalance,
    /// `eth_getBlockByHash`.
    #[strum(serialize = "eth_getBlockByHash")]
    EthGetBlockByHash,
    /// `eth_getBlockByNumber`.
    #[strum(serialize = "eth_getBlockByNumber")]
    EthGetBlockByNumber,
    /// `eth_getBlockReceipts`.
    #[strum(serialize = "eth_getBlockReceipts")]
    EthGetBlockReceipts,
    /// `eth_getCode`.
    #[strum(serialize = "eth_getCode")]
    EthGetCode,
    /// `eth_getLogs`.
    #[strum(serialize = "eth_getLogs")]
    EthGetLogs,
    /// `eth_getProof`.
    #[strum(serialize = "eth_getProof")]
    EthGetProof,
    /// `eth_getStorageAt`.
    #[strum(serialize = "eth_getStorageAt")]
    EthGetStorageAt,
    /// `eth_getTransactionByHash`.
    #[strum(serialize = "eth_getTransactionByHash")]
    EthGetTransactionByHash,
    /// `eth_getTransactionCount`.
    #[strum(serialize = "eth_getTransactionCount")]
    EthGetTransactionCount,
    /// `eth_getTransactionReceipt`.
    #[strum(serialize = "eth_getTransactionReceipt")]
    EthGetTransactionReceipt,
    /// `net_version`.
    #[strum(serialize = "net_version")]
    NetVersion,
}

impl ChainMethod {
    /// The wire method name. `&'static` because the set is closed.
    pub fn as_str(self) -> &'static str {
        self.into()
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
            "admin_peers",
            "debug_traceCall",
            "",
        ] {
            assert!(ChainMethod::try_from(m).is_err(), "{m} must be rejected");
        }
    }

    #[test]
    fn as_str_round_trips_the_wire_name() {
        assert_eq!(ChainMethod::EthCall.as_str(), "eth_call");
        assert_eq!(
            ChainMethod::try_from(ChainMethod::EthGetBalance.as_str()),
            Ok(ChainMethod::EthGetBalance),
        );
    }
}
