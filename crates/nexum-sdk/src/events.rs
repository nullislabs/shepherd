//! Chain-log delivery at the guest WIT edge.
//!
//! Modules receive on-chain logs as the native [`Log`] (alloy's
//! `eth_getLogs` shape), not an SDK-invented view. The host packs each
//! log into the WIT `chain-log` record; [`assemble_log`] rebuilds the
//! alloy value from that record's raw fields. The per-module bind macro
//! emits the `From<chain-log>` glue that calls this, so a strategy holds
//! `&[Log]` and decodes `sol!` events against [`Log::inner`].

use alloy_primitives::{Address, B256, Bytes, Log as PrimitiveLog, LogData};

/// The alloy RPC log delivered to modules for chain-log events.
pub use alloy_rpc_types_eth::Log;

/// Assemble an alloy [`Log`] from a WIT `chain-log` record's raw fields.
///
/// Fixed-width byte fields are right-aligned into their EVM word (20 bytes
/// for the address, 32 for topics and hashes): a well-formed host frame
/// carries the exact width and copies verbatim, while a malformed one is
/// clamped rather than trapping the guest.
#[expect(clippy::too_many_arguments, reason = "mirrors the flat WIT record")]
pub fn assemble_log(
    address: &[u8],
    topics: &[Vec<u8>],
    data: &[u8],
    block_hash: Option<&[u8]>,
    block_number: Option<u64>,
    block_timestamp: Option<u64>,
    transaction_hash: Option<&[u8]>,
    transaction_index: Option<u64>,
    log_index: Option<u64>,
    removed: bool,
) -> Log {
    let topics = topics.iter().map(|t| word(t)).collect();
    Log {
        inner: PrimitiveLog {
            address: address20(address),
            data: LogData::new_unchecked(topics, Bytes::copy_from_slice(data)),
        },
        block_hash: block_hash.map(word),
        block_number,
        block_timestamp,
        transaction_hash: transaction_hash.map(word),
        transaction_index,
        log_index,
        removed,
    }
}

/// Right-align up to 32 bytes into an EVM word.
fn word(bytes: &[u8]) -> B256 {
    let mut out = [0u8; 32];
    let n = bytes.len().min(32);
    out[32 - n..].copy_from_slice(&bytes[..n]);
    B256::from(out)
}

/// Right-align up to 20 bytes into an address.
fn address20(bytes: &[u8]) -> Address {
    let mut out = [0u8; 20];
    let n = bytes.len().min(20);
    out[20 - n..].copy_from_slice(&bytes[..n]);
    Address::from(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assembles_full_mined_log() {
        let addr = [0x11u8; 20];
        let topic = [0x22u8; 32];
        let hash = [0x33u8; 32];
        let log = assemble_log(
            &addr,
            &[topic.to_vec()],
            &[1, 2, 3],
            Some(&hash),
            Some(42),
            Some(1_700_000_000),
            Some(hash.to_vec().as_slice()),
            Some(7),
            Some(9),
            true,
        );
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
        let log = assemble_log(
            &[0u8; 20],
            &[],
            &[],
            None,
            None,
            None,
            None,
            None,
            None,
            false,
        );
        assert!(log.block_hash.is_none());
        assert!(log.block_number.is_none());
        assert!(log.transaction_hash.is_none());
        assert!(log.log_index.is_none());
        assert!(!log.removed);
    }

    #[test]
    fn undersized_word_is_left_padded() {
        assert_eq!(word(&[0xab]), B256::with_last_byte(0xab));
    }
}
