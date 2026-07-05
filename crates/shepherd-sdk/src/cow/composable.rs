//! ComposableCoW poll-revert decoding.
//!
//! `ComposableCoW.getTradeableOrderWithSignature` reverts with one of
//! five custom errors when the conditional order is not ready, expired,
//! or otherwise non-tradeable. This module mirrors that error surface
//! and maps each revert to the typed [`PollOutcome`] every TWAP /
//! strategy module dispatches on.
//!
//! Source for the Solidity errors:
//! `cowprotocol/composable-cow/src/interfaces/IConditionalOrder.sol`.

use alloy_primitives::{Bytes, U256};
use alloy_sol_types::{SolError, sol};
use cowprotocol::GPv2OrderData;

sol! {
    /// Five custom errors `IConditionalOrder.verify` reverts with.
    /// Selector source for [`decode_revert`]. The wire shape mirrors
    /// the Solidity definitions verbatim so the four-byte selectors
    /// computed here match what the contract emits.
    #[derive(Debug)]
    interface IConditionalOrder {
        /// `OrderNotValid(string)` - the order condition is permanently
        /// not met. Watch towers drop.
        error OrderNotValid(string reason);
        /// `PollTryNextBlock(string)` - try again on the next block.
        error PollTryNextBlock(string reason);
        /// `PollTryAtBlock(uint256, string)` - try at or after the
        /// given block number.
        error PollTryAtBlock(uint256 blockNumber, string reason);
        /// `PollTryAtEpoch(uint256, string)` - try at or after the
        /// given Unix timestamp (seconds).
        error PollTryAtEpoch(uint256 timestamp, string reason);
        /// `PollNever(string)` - the conditional order is dead.
        error PollNever(string reason);
    }
}

/// Outcome of a single watch poll. Mirrors the enum shape:
/// `Ready` carries the materials the submit path needs; the other
/// variants drive the lifecycle handler.
///
/// `Ready` is intentionally never produced by [`decode_revert`] - it
/// only comes from the successful return path the poll module
/// constructs at the call site.
#[derive(Debug)]
pub enum PollOutcome {
    /// Conditional order is tradeable now; submit `order` with the
    /// embedded EIP-1271 `signature` blob. `GPv2OrderData` is boxed
    /// to keep the enum cache-friendly (~300 bytes vs. ~8 for the
    /// other variants).
    Ready {
        /// The 12-field order ready to submit.
        order: Box<GPv2OrderData>,
        /// EIP-1271 wire-form signature (raw verifier bytes; the
        /// orderbook prepends `from` before settlement).
        signature: Bytes,
    },
    /// Retry on the very next block - typical for time-sliced TWAP
    /// schedules and other handlers that re-check on every tick.
    TryNextBlock,
    /// Retry once block number reaches the embedded value.
    TryOnBlock(u64),
    /// Retry once the wall clock (Unix seconds, UTC) reaches the
    /// embedded value.
    TryAtEpoch(u64),
    /// Order is dead - drop the watch. Aggregates `OrderNotValid` and
    /// `PollNever` reverts; the original reason string is dropped
    /// because the lifecycle handler does not key off it today.
    DontTryAgain,
}

/// Decode a `getTradeableOrderWithSignature` revert payload into a
/// [`PollOutcome`].
///
/// Returns `None` when the selector is not one of the five
/// [`IConditionalOrder`] errors - including a bare `Error(string)`
/// require-revert. Callers should treat that as `TryNextBlock` (the
/// safe default) so a transient RPC blip does not drop a still-valid
/// watch.
#[must_use]
pub fn decode_revert(data: &[u8]) -> Option<PollOutcome> {
    if data.len() < 4 {
        return None;
    }
    let selector: [u8; 4] = data[..4].try_into().ok()?;
    let body = &data[4..];
    match selector {
        s if s == IConditionalOrder::OrderNotValid::SELECTOR => Some(PollOutcome::DontTryAgain),
        s if s == IConditionalOrder::PollTryNextBlock::SELECTOR => Some(PollOutcome::TryNextBlock),
        s if s == IConditionalOrder::PollTryAtBlock::SELECTOR => {
            let decoded = IConditionalOrder::PollTryAtBlock::abi_decode_raw(body).ok()?;
            Some(PollOutcome::TryOnBlock(u256_to_u64_saturating(
                decoded.blockNumber,
            )))
        }
        s if s == IConditionalOrder::PollTryAtEpoch::SELECTOR => {
            let decoded = IConditionalOrder::PollTryAtEpoch::abi_decode_raw(body).ok()?;
            Some(PollOutcome::TryAtEpoch(u256_to_u64_saturating(
                decoded.timestamp,
            )))
        }
        s if s == IConditionalOrder::PollNever::SELECTOR => Some(PollOutcome::DontTryAgain),
        _ => None,
    }
}

fn u256_to_u64_saturating(v: U256) -> u64 {
    u64::try_from(v).unwrap_or(u64::MAX)
}

/// Decode a hex string carrying revert bytes (optionally `0x`-prefixed,
/// optionally JSON-quoted) into a [`PollOutcome`] via [`decode_revert`].
///
/// This is the bridge between the host's structured error data (a hex
/// string in `host-error.data`) and the typed [`PollOutcome`] dispatch.
///
/// # Example
///
/// ```
/// use alloy_sol_types::SolError;
/// use shepherd_sdk::cow::{IConditionalOrder, PollOutcome, decode_revert_hex};
///
/// // Simulate the host forwarding an OrderNotValid revert payload.
/// let revert = IConditionalOrder::OrderNotValid {
///     reason: "expired".into(),
/// }
/// .abi_encode();
/// let host_data = format!("\"0x{}\"", alloy_primitives::hex::encode(&revert));
///
/// assert!(matches!(
///     decode_revert_hex(&host_data),
///     Some(PollOutcome::DontTryAgain),
/// ));
/// ```
#[must_use]
pub fn decode_revert_hex(s: &str) -> Option<PollOutcome> {
    let stripped = s.trim_matches('"');
    let stripped = stripped.strip_prefix("0x").unwrap_or(stripped);
    let bytes = alloy_primitives::hex::decode(stripped).ok()?;
    decode_revert(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_not_valid_maps_to_drop() {
        let err = IConditionalOrder::OrderNotValid {
            reason: "expired".to_string(),
        };
        assert!(matches!(
            decode_revert(&err.abi_encode()),
            Some(PollOutcome::DontTryAgain)
        ));
    }

    #[test]
    fn poll_never_maps_to_drop() {
        let err = IConditionalOrder::PollNever {
            reason: "cancelled".to_string(),
        };
        assert!(matches!(
            decode_revert(&err.abi_encode()),
            Some(PollOutcome::DontTryAgain)
        ));
    }

    #[test]
    fn try_next_block() {
        let err = IConditionalOrder::PollTryNextBlock {
            reason: "noop".to_string(),
        };
        assert!(matches!(
            decode_revert(&err.abi_encode()),
            Some(PollOutcome::TryNextBlock)
        ));
    }

    #[test]
    fn try_at_block_carries_number() {
        let err = IConditionalOrder::PollTryAtBlock {
            blockNumber: U256::from(12_345_678_u64),
            reason: "wait".to_string(),
        };
        assert!(matches!(
            decode_revert(&err.abi_encode()),
            Some(PollOutcome::TryOnBlock(12_345_678))
        ));
    }

    #[test]
    fn try_at_epoch_carries_timestamp() {
        let err = IConditionalOrder::PollTryAtEpoch {
            timestamp: U256::from(1_700_000_000_u64),
            reason: "soon".to_string(),
        };
        assert!(matches!(
            decode_revert(&err.abi_encode()),
            Some(PollOutcome::TryAtEpoch(1_700_000_000))
        ));
    }

    #[test]
    fn unknown_selector_returns_none() {
        let mut data = vec![0xde, 0xad, 0xbe, 0xef];
        data.extend_from_slice(&[0u8; 32]);
        assert!(decode_revert(&data).is_none());
    }

    #[test]
    fn truncated_returns_none() {
        assert!(decode_revert(&[0x01, 0x02]).is_none());
    }

    #[test]
    fn u256_saturates_at_max() {
        assert_eq!(u256_to_u64_saturating(U256::MAX), u64::MAX);
        assert_eq!(u256_to_u64_saturating(U256::from(42_u64)), 42);
    }

    #[test]
    fn decode_revert_hex_strips_prefix_and_quotes() {
        let err = IConditionalOrder::PollTryAtBlock {
            blockNumber: U256::from(42_u64),
            reason: "x".to_string(),
        };
        let payload = alloy_primitives::hex::encode_prefixed(err.abi_encode());
        let quoted = format!("\"{payload}\"");
        assert!(matches!(
            decode_revert_hex(&quoted),
            Some(PollOutcome::TryOnBlock(42))
        ));
    }

    #[test]
    fn decode_revert_hex_handles_unprefixed_naked_hex() {
        let err = IConditionalOrder::PollTryNextBlock {
            reason: "noop".to_string(),
        };
        let payload = alloy_primitives::hex::encode(err.abi_encode());
        assert!(matches!(
            decode_revert_hex(&payload),
            Some(PollOutcome::TryNextBlock)
        ));
    }
}
