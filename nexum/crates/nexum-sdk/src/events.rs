//! Chain-log delivery at the guest WIT edge.
//!
//! Modules receive on-chain logs as the native [`Log`] (alloy's
//! `eth_getLogs` shape), not an SDK-invented view. The host packs each log
//! into the WIT `chain-log` record; [`ChainLogParts`] borrows that record's
//! raw fields and `From` rebuilds the alloy value. The per-module bind macro
//! emits the `From<chain-log>` glue that routes through this, so a strategy
//! holds `&[Log]` and decodes `sol!` events against [`Log::inner`].

use alloy_primitives::{Address, B256, Bytes, Log as PrimitiveLog, LogData};

/// The alloy RPC log delivered to modules for chain-log events.
pub use alloy_rpc_types_eth::Log;

/// Borrowed raw fields of a WIT `chain-log` record, assembled into an alloy
/// [`Log`] via `From`.
///
/// Fixed-width byte fields are right-aligned into their EVM word (20 bytes for
/// the address, 32 for topics and hashes). The host is the sole runtime and
/// the frames it emits are well-formed by construction, so an out-of-width
/// field is a host bug that traps loudly rather than being silently reshaped.
#[derive(Default)]
pub struct ChainLogParts<'a> {
    /// 20-byte contract address.
    pub address: &'a [u8],
    /// Indexed topics, each a 32-byte word.
    pub topics: &'a [Vec<u8>],
    /// ABI-encoded non-indexed data.
    pub data: &'a [u8],
    /// Block hash; `None` for a pending log.
    pub block_hash: Option<&'a [u8]>,
    /// Block number; `None` for a pending log.
    pub block_number: Option<u64>,
    /// Block timestamp; `None` for a pending log.
    pub block_timestamp: Option<u64>,
    /// Transaction hash; `None` for a pending log.
    pub transaction_hash: Option<&'a [u8]>,
    /// Transaction index; `None` for a pending log.
    pub transaction_index: Option<u64>,
    /// Log index; `None` for a pending log.
    pub log_index: Option<u64>,
    /// Whether the log was removed by a reorg.
    pub removed: bool,
}

impl From<ChainLogParts<'_>> for Log {
    fn from(p: ChainLogParts<'_>) -> Self {
        Log {
            inner: PrimitiveLog {
                address: Address::left_padding_from(p.address),
                // Topics arrive from an alloy provider `Log` (at most 4 by the
                // EVM rule, enforced upstream) via the trusted host, so the
                // unchecked constructor is sound.
                data: LogData::new_unchecked(
                    p.topics
                        .iter()
                        .map(|t| B256::left_padding_from(t))
                        .collect(),
                    Bytes::copy_from_slice(p.data),
                ),
            },
            block_hash: p.block_hash.map(B256::left_padding_from),
            block_number: p.block_number,
            block_timestamp: p.block_timestamp,
            transaction_hash: p.transaction_hash.map(B256::left_padding_from),
            transaction_index: p.transaction_index,
            log_index: p.log_index,
            removed: p.removed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assembles_full_mined_log() {
        let addr = [0x11u8; 20];
        let topic = [0x22u8; 32];
        let hash = [0x33u8; 32];
        let log: Log = ChainLogParts {
            address: &addr,
            topics: &[topic.to_vec()],
            data: &[1, 2, 3],
            block_hash: Some(&hash),
            block_number: Some(42),
            block_timestamp: Some(1_700_000_000),
            transaction_hash: Some(&hash),
            transaction_index: Some(7),
            log_index: Some(9),
            removed: true,
        }
        .into();
        assert_eq!(log.address().as_slice(), addr);
        assert_eq!(log.topics(), &[B256::from(topic)]);
        assert_eq!(log.inner.data.data.as_ref(), &[1, 2, 3]);
        assert_eq!(log.block_hash, Some(B256::from(hash)));
        assert_eq!(log.block_number, Some(42));
        assert_eq!(log.block_timestamp, Some(1_700_000_000));
        assert_eq!(log.transaction_index, Some(7));
        assert_eq!(log.log_index, Some(9));
        assert!(log.removed);
    }

    #[test]
    fn pending_log_leaves_block_fields_absent() {
        let log: Log = ChainLogParts {
            address: &[0u8; 20],
            ..Default::default()
        }
        .into();
        assert!(log.block_hash.is_none());
        assert!(log.block_number.is_none());
        assert!(log.transaction_hash.is_none());
        assert!(log.log_index.is_none());
        assert!(!log.removed);
    }

    #[test]
    fn undersized_word_is_left_padded() {
        let log: Log = ChainLogParts {
            address: &[0u8; 20],
            topics: &[vec![0xab]],
            ..Default::default()
        }
        .into();
        assert_eq!(log.topics(), &[B256::with_last_byte(0xab)]);
    }
}
