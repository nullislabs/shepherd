//! ComposableCoW poll seam: the structured [`Verdict`] and the
//! quarantined [`LegacyRevertAdapter`].
//!
//! Every module poll resolves to a [`Verdict`]; the keeper run and
//! modules dispatch on its variants alone. The deployed
//! ComposableCoW 1.x contract instead reverts with one of five custom
//! errors; [`LegacyRevertAdapter`] decodes that wire onto a [`Verdict`]
//! and is the single seam that retires when the structured generator
//! ships.

use alloy_primitives::{Bytes, U256};
use alloy_sol_types::{SolError, sol};
use cowprotocol::GPv2OrderData;
use nexum_sdk::host::ChainError;

sol! {
    /// Deployed ComposableCoW 1.x custom error surface; selector source
    /// for [`LegacyRevertAdapter::decode`].
    #[derive(Debug)]
    interface IConditionalOrder {
        /// Order condition permanently unmet; drop.
        error OrderNotValid(string reason);
        /// Retry on the next block.
        error PollTryNextBlock(string reason);
        /// Retry at or after `blockNumber`.
        error PollTryAtBlock(uint256 blockNumber, string reason);
        /// Retry at or after `timestamp` (Unix seconds).
        error PollTryAtEpoch(uint256 timestamp, string reason);
        /// Conditional order is dead.
        error PollNever(string reason);
    }
}

/// Structured outcome of a single commitment poll.
///
/// Every variant but `Post` carries `reason`, the source 4-byte
/// selector for logging only; `[0; 4]` when synthetic. `Post` is the
/// only variant [`LegacyRevertAdapter`] never produces.
#[derive(Debug)]
pub enum Verdict {
    /// Tradeable now; submit `order` with its EIP-1271 `signature`.
    Post {
        /// Order ready to submit.
        order: Box<GPv2OrderData>,
        /// EIP-1271 signature blob (raw verifier bytes; the orderbook
        /// prepends `from` before settlement).
        signature: Bytes,
        /// Advisory next-poll hint (Unix seconds); `None` when synthetic.
        next_poll_timestamp: Option<u64>,
    },
    /// Retry once the wall clock (Unix seconds) reaches `wait_until`.
    WaitTimestamp {
        /// Re-poll at or after this Unix timestamp (seconds).
        wait_until: u64,
        /// Source selector, log only.
        reason: [u8; 4],
    },
    /// Retry once the block number reaches `wait_until`.
    WaitBlock {
        /// Re-poll at or after this block number.
        wait_until: u64,
        /// Source selector, log only.
        reason: [u8; 4],
    },
    /// Retry on the next block.
    TryNextBlock {
        /// Source selector, log only.
        reason: [u8; 4],
    },
    /// Order is dead; drop the commitment.
    Invalid {
        /// Source selector, log only.
        reason: [u8; 4],
    },
    /// Generator needs off-chain input; the keeper parks the commitment.
    /// Never produced by [`LegacyRevertAdapter`].
    NeedsInput {
        /// Source selector, log only.
        reason: [u8; 4],
    },
}

/// Quarantined decoder for the deployed ComposableCoW 1.x reverting
/// wire; maps each revert onto a [`Verdict`].
#[derive(Debug, Clone, Copy)]
pub struct LegacyRevertAdapter;

impl LegacyRevertAdapter {
    /// Decode a revert payload into a [`Verdict`], or `None` when the
    /// selector is not one of the five [`IConditionalOrder`] errors.
    /// [`classify`](Self::classify) is the lifecycle policy on top.
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

    /// Classify a failed poll `eth_call` into a [`Verdict`]: the one
    /// policy for what a poll failure means to the commitment lifecycle.
    ///
    /// A recognised revert decodes; an unrecognised selector maps to
    /// `Invalid` (a permanent contract-level rejection that would
    /// otherwise loop every block); payload-free failures (transport
    /// faults, sub-selector data) stay `TryNextBlock`.
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

    /// A selector outside the `IConditionalOrder` vocabulary maps to
    /// `Invalid`, not re-poll forever.
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
        /// `decode` never panics; `None` below the 4-byte selector length.
        #[test]
        fn decode_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..64)) {
            let outcome = LegacyRevertAdapter::decode(&bytes);
            if bytes.len() < 4 {
                prop_assert!(outcome.is_none());
            }
        }
    }
}
