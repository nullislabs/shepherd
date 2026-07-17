//! ComposableCoW poll seam: the structured [`Verdict`] and the
//! quarantined [`LegacyRevertAdapter`].
//!
//! Every strategy poll resolves to a [`Verdict`] - the structured
//! outcome mirroring the composable-cow fork's structured generator.
//! The keeper run and each strategy module
//! dispatch on the `Verdict` variants alone; nothing downstream knows
//! how the outcome was produced.
//!
//! The deployed ComposableCoW 1.x contract does not speak that
//! structured vocabulary. Its `getTradeableOrderWithSignature` reverts
//! with one of five custom errors when the conditional order is not
//! ready, expired, or otherwise non-tradeable. That reverting wire is
//! frozen: this module keeps decoding it, but the decode is quarantined
//! behind [`LegacyRevertAdapter`], which maps each legacy revert onto a
//! [`Verdict`]. When the fork's structured generator ships, the adapter
//! is the single seam that retires - the `Verdict` surface and every
//! dispatch site stay put.
//!
//! Source for the Solidity errors:
//! `cowprotocol/composable-cow/src/interfaces/IConditionalOrder.sol`.

use alloy_primitives::{Bytes, U256};
use alloy_sol_types::{SolError, sol};
use cowprotocol::GPv2OrderData;
use nexum_sdk::host::ChainError;

sol! {
    /// Five custom errors `IConditionalOrder.verify` reverts with -
    /// the deployed ComposableCoW 1.x error surface. Selector source
    /// for [`LegacyRevertAdapter::decode`]. The wire shape mirrors the
    /// Solidity definitions verbatim so the four-byte selectors
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

/// Structured outcome of a single watch poll, mirroring the
/// composable-cow fork's structured generator.
///
/// Every variant except `Post` carries a `reason`: the raw 4-byte
/// selector the outcome was derived from, for logging only - no
/// behaviour keys off it. It is `[0; 4]` when the outcome is synthetic
/// (no selector available, e.g. a transport fault). `Post` is the only
/// variant [`LegacyRevertAdapter`] never produces; it comes from the
/// successful return path each strategy constructs at the call site.
#[derive(Debug)]
pub enum Verdict {
    /// Conditional order is tradeable now; submit `order` with the
    /// embedded EIP-1271 `signature` blob. `GPv2OrderData` is boxed to
    /// keep the enum cache-friendly (~300 bytes vs. a few for the other
    /// variants).
    Post {
        /// The 12-field order ready to submit.
        order: Box<GPv2OrderData>,
        /// EIP-1271 wire-form signature (raw verifier bytes; the
        /// orderbook prepends `from` before settlement).
        signature: Bytes,
        /// Advisory Unix timestamp (seconds) the fork's generator hints
        /// the next poll at. `0` when synthetic - the legacy adapter
        /// has no such hint, so the submit path ignores it.
        next_poll_timestamp: u64,
    },
    /// Retry once the wall clock (Unix seconds, UTC) reaches
    /// `wait_until`.
    WaitTimestamp {
        /// Unix timestamp (seconds) to re-poll at or after.
        wait_until: u64,
        /// Source selector, log only.
        reason: [u8; 4],
    },
    /// Retry once the block number reaches `wait_until`.
    WaitBlock {
        /// Block number to re-poll at or after.
        wait_until: u64,
        /// Source selector, log only.
        reason: [u8; 4],
    },
    /// Retry on the very next block - typical for time-sliced TWAP
    /// schedules and other handlers that re-check on every tick.
    TryNextBlock {
        /// Source selector, log only.
        reason: [u8; 4],
    },
    /// Order is dead - drop the watch. Aggregates the legacy
    /// `OrderNotValid` and `PollNever` reverts and any unrecognised
    /// contract-level rejection.
    Invalid {
        /// Source selector, log only.
        reason: [u8; 4],
    },
    /// The generator needs off-chain input before it can produce an
    /// order. Never produced by [`LegacyRevertAdapter`]; the
    /// keeper run parks the watch untouched.
    NeedsInput {
        /// Source selector, log only.
        reason: [u8; 4],
    },
}

/// Quarantined decoder for the deployed ComposableCoW 1.x reverting
/// wire. Maps each legacy `getTradeableOrderWithSignature` revert onto
/// a [`Verdict`]; this is the single seam that retires when the fork's
/// structured generator ships.
#[derive(Debug, Clone, Copy)]
pub struct LegacyRevertAdapter;

impl LegacyRevertAdapter {
    /// Decode a `getTradeableOrderWithSignature` revert payload into a
    /// [`Verdict`].
    ///
    /// Returns `None` when the selector is not one of the five
    /// [`IConditionalOrder`] errors - including a bare `Error(string)`
    /// require-revert. [`classify`](Self::classify) is the lifecycle
    /// policy on top: it treats any such foreign selector as a
    /// permanent contract-level rejection.
    #[must_use]
    pub fn decode(data: &[u8]) -> Option<Verdict> {
        if data.len() < 4 {
            return None;
        }
        let reason: [u8; 4] = data[..4].try_into().ok()?;
        let body = &data[4..];
        match reason {
            s if s == IConditionalOrder::OrderNotValid::SELECTOR => {
                Some(Verdict::Invalid { reason })
            }
            s if s == IConditionalOrder::PollTryNextBlock::SELECTOR => {
                Some(Verdict::TryNextBlock { reason })
            }
            s if s == IConditionalOrder::PollTryAtBlock::SELECTOR => {
                let decoded = IConditionalOrder::PollTryAtBlock::abi_decode_raw(body).ok()?;
                Some(Verdict::WaitBlock {
                    wait_until: u256_to_u64_saturating(decoded.blockNumber),
                    reason,
                })
            }
            s if s == IConditionalOrder::PollTryAtEpoch::SELECTOR => {
                let decoded = IConditionalOrder::PollTryAtEpoch::abi_decode_raw(body).ok()?;
                Some(Verdict::WaitTimestamp {
                    wait_until: u256_to_u64_saturating(decoded.timestamp),
                    reason,
                })
            }
            s if s == IConditionalOrder::PollNever::SELECTOR => Some(Verdict::Invalid { reason }),
            _ => None,
        }
    }

    /// Classify a failed poll `eth_call` into a [`Verdict`] - the one
    /// policy for what a poll failure means to the watch lifecycle.
    ///
    /// A revert payload big enough to carry a selector that
    /// [`decode`](Self::decode) does not recognise maps to `Invalid`:
    /// it is a contract-level rejection outside the `IConditionalOrder`
    /// vocabulary (a handler-specific error, typically permanent), and
    /// retrying it on every block loops forever. Only payload-free
    /// failures - transport faults and reverts whose `data` is absent
    /// or shorter than a selector - stay `TryNextBlock`.
    #[must_use]
    pub fn classify(err: &ChainError) -> Verdict {
        match err {
            ChainError::Rpc(rpc) => match rpc.data.as_deref() {
                Some(data) if data.len() >= 4 => {
                    let reason: [u8; 4] = data[..4].try_into().unwrap_or([0; 4]);
                    Self::decode(data).unwrap_or(Verdict::Invalid { reason })
                }
                _ => Verdict::TryNextBlock { reason: [0; 4] },
            },
            // `ChainError` is `#[non_exhaustive]`: transport faults and
            // any future case are payload-free, so they stay retryable.
            _ => Verdict::TryNextBlock { reason: [0; 4] },
        }
    }
}

fn u256_to_u64_saturating(v: U256) -> u64 {
    u64::try_from(v).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_not_valid_maps_to_invalid() {
        let err = IConditionalOrder::OrderNotValid {
            reason: "expired".to_string(),
        };
        assert!(matches!(
            LegacyRevertAdapter::decode(&err.abi_encode()),
            Some(Verdict::Invalid { .. })
        ));
    }

    #[test]
    fn poll_never_maps_to_invalid() {
        let err = IConditionalOrder::PollNever {
            reason: "cancelled".to_string(),
        };
        assert!(matches!(
            LegacyRevertAdapter::decode(&err.abi_encode()),
            Some(Verdict::Invalid { .. })
        ));
    }

    #[test]
    fn try_next_block() {
        let err = IConditionalOrder::PollTryNextBlock {
            reason: "noop".to_string(),
        };
        assert!(matches!(
            LegacyRevertAdapter::decode(&err.abi_encode()),
            Some(Verdict::TryNextBlock { .. })
        ));
    }

    #[test]
    fn try_at_block_carries_number() {
        let err = IConditionalOrder::PollTryAtBlock {
            blockNumber: U256::from(12_345_678_u64),
            reason: "wait".to_string(),
        };
        assert!(matches!(
            LegacyRevertAdapter::decode(&err.abi_encode()),
            Some(Verdict::WaitBlock {
                wait_until: 12_345_678,
                ..
            })
        ));
    }

    #[test]
    fn try_at_epoch_carries_timestamp() {
        let err = IConditionalOrder::PollTryAtEpoch {
            timestamp: U256::from(1_700_000_000_u64),
            reason: "soon".to_string(),
        };
        assert!(matches!(
            LegacyRevertAdapter::decode(&err.abi_encode()),
            Some(Verdict::WaitTimestamp {
                wait_until: 1_700_000_000,
                ..
            })
        ));
    }

    #[test]
    fn decoded_reason_carries_the_selector() {
        let err = IConditionalOrder::PollTryNextBlock {
            reason: "noop".to_string(),
        };
        let Some(Verdict::TryNextBlock { reason }) = LegacyRevertAdapter::decode(&err.abi_encode())
        else {
            panic!("expected TryNextBlock");
        };
        assert_eq!(reason, IConditionalOrder::PollTryNextBlock::SELECTOR);
    }

    #[test]
    fn unknown_selector_returns_none() {
        let mut data = vec![0xde, 0xad, 0xbe, 0xef];
        data.extend_from_slice(&[0u8; 32]);
        assert!(LegacyRevertAdapter::decode(&data).is_none());
    }

    #[test]
    fn truncated_returns_none() {
        assert!(LegacyRevertAdapter::decode(&[0x01, 0x02]).is_none());
    }

    #[test]
    fn u256_saturates_at_max() {
        assert_eq!(u256_to_u64_saturating(U256::MAX), u64::MAX);
        assert_eq!(u256_to_u64_saturating(U256::from(42_u64)), 42);
    }

    // ---- LegacyRevertAdapter::classify ----

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
            LegacyRevertAdapter::classify(&rpc(Some(revert))),
            Verdict::WaitBlock {
                wait_until: 777,
                ..
            }
        ));
    }

    /// A handler-specific selector outside the `IConditionalOrder`
    /// vocabulary is a permanent contract-level rejection: it must map
    /// to `Invalid`, not re-poll every block forever.
    #[test]
    fn classify_unrecognised_selector_is_invalid() {
        let mut data = vec![0x7a, 0x93, 0x32, 0x34];
        data.extend_from_slice(&[0u8; 32]);
        assert!(matches!(
            LegacyRevertAdapter::classify(&rpc(Some(data))),
            Verdict::Invalid { .. }
        ));
        // A bare 4-byte selector with no body classifies the same way.
        assert!(matches!(
            LegacyRevertAdapter::classify(&rpc(Some(vec![0x2c, 0x7c, 0xa6, 0xd7]))),
            Verdict::Invalid { .. }
        ));
    }

    #[test]
    fn classify_payload_free_failures_stay_try_next_block() {
        assert!(matches!(
            LegacyRevertAdapter::classify(&rpc(None)),
            Verdict::TryNextBlock { .. }
        ));
        assert!(matches!(
            LegacyRevertAdapter::classify(&rpc(Some(Vec::new()))),
            Verdict::TryNextBlock { .. }
        ));
        // Sub-selector payloads cannot name a contract error.
        assert!(matches!(
            LegacyRevertAdapter::classify(&rpc(Some(vec![0x01, 0x02]))),
            Verdict::TryNextBlock { .. }
        ));
        assert!(matches!(
            LegacyRevertAdapter::classify(&ChainError::Fault(Fault::Timeout)),
            Verdict::TryNextBlock { .. }
        ));
    }

    use proptest::prelude::*;

    proptest! {
        /// `decode` on arbitrary revert bytes never panics and returns
        /// `None` for inputs shorter than the 4-byte EVM selector.
        #[test]
        fn decode_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..64)) {
            let outcome = LegacyRevertAdapter::decode(&bytes);
            if bytes.len() < 4 {
                prop_assert!(outcome.is_none());
            }
        }
    }
}
