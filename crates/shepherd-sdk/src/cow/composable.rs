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
use nexum_sdk::host::ChainError;

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
/// require-revert. [`classify_poll_error`] is the lifecycle policy on
/// top: it treats any such foreign selector as a permanent
/// contract-level rejection.
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

/// Classify a failed poll `eth_call` into a [`PollOutcome`] - the one
/// policy for what a poll failure means to the watch lifecycle.
///
/// A revert payload big enough to carry a selector that
/// [`decode_revert`] does not recognise maps to `DontTryAgain`: it is
/// a contract-level rejection outside the `IConditionalOrder`
/// vocabulary (a handler-specific error, typically permanent), and
/// retrying it on every block loops forever. Only payload-free
/// failures - transport faults and reverts whose `data` is absent or
/// shorter than a selector - stay `TryNextBlock`.
#[must_use]
pub fn classify_poll_error(err: &ChainError) -> PollOutcome {
    match err {
        ChainError::Rpc(rpc) => match rpc.data.as_deref() {
            Some(data) if data.len() >= 4 => {
                decode_revert(data).unwrap_or(PollOutcome::DontTryAgain)
            }
            _ => PollOutcome::TryNextBlock,
        },
        ChainError::Fault(_) => PollOutcome::TryNextBlock,
    }
}

fn u256_to_u64_saturating(v: U256) -> u64 {
    u64::try_from(v).unwrap_or(u64::MAX)
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

    // ---- classify_poll_error ----

    use nexum_sdk::host::{Fault, RpcError};

    fn rpc(data: Option<Vec<u8>>) -> ChainError {
        ChainError::Rpc(RpcError {
            code: -32000,
            message: "execution reverted".into(),
            data: data.map(Into::into),
        })
    }

    #[test]
    fn classify_dispatches_a_recognised_selector() {
        let revert = IConditionalOrder::PollTryAtBlock {
            blockNumber: U256::from(777_u64),
            reason: "wait".to_string(),
        }
        .abi_encode();
        assert!(matches!(
            classify_poll_error(&rpc(Some(revert))),
            PollOutcome::TryOnBlock(777)
        ));
    }

    /// A handler-specific selector outside the `IConditionalOrder`
    /// vocabulary is a permanent contract-level rejection: it must
    /// drop, not re-poll every block forever.
    #[test]
    fn classify_unrecognised_selector_drops() {
        let mut data = vec![0x7a, 0x93, 0x32, 0x34];
        data.extend_from_slice(&[0u8; 32]);
        assert!(matches!(
            classify_poll_error(&rpc(Some(data))),
            PollOutcome::DontTryAgain
        ));
        // A bare 4-byte selector with no body classifies the same way.
        assert!(matches!(
            classify_poll_error(&rpc(Some(vec![0x2c, 0x7c, 0xa6, 0xd7]))),
            PollOutcome::DontTryAgain
        ));
    }

    #[test]
    fn classify_payload_free_failures_stay_try_next_block() {
        assert!(matches!(
            classify_poll_error(&rpc(None)),
            PollOutcome::TryNextBlock
        ));
        assert!(matches!(
            classify_poll_error(&rpc(Some(Vec::new()))),
            PollOutcome::TryNextBlock
        ));
        // Sub-selector payloads cannot name a contract error.
        assert!(matches!(
            classify_poll_error(&rpc(Some(vec![0x01, 0x02]))),
            PollOutcome::TryNextBlock
        ));
        assert!(matches!(
            classify_poll_error(&ChainError::Fault(Fault::Timeout)),
            PollOutcome::TryNextBlock
        ));
    }
}
