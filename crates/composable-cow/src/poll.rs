//! The structured poll [`Verdict`] and its [`NextPoll`] schedule.
//!
//! Every poll resolves to a [`Verdict`]; the keeper run dispatches on
//! its variants alone. The fork wire that produces them lives in
//! [`fork`](super::fork).

use alloy_primitives::{Bytes, Selector, U256};
use cowprotocol::GPv2OrderData;

/// When to poll after a posted order.
///
/// The fork's `nextPollTimestamp` is advisory and, per its NatSpec,
/// only meaningful on `POST`. Two sentinels carry meaning the raw
/// integer cannot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NextPoll {
    /// Poll at this Unix timestamp, in seconds.
    At(u64),
    /// Wire `0`: poll at the posted order's `validTo + 1`.
    AtValidToPlus1,
    /// Wire `u256::MAX`: the final order, so stop polling.
    Never,
}

impl NextPoll {
    /// Classify a raw `nextPollTimestamp`. A value between `u64::MAX`
    /// and the sentinel saturates rather than wrapping into a near-term
    /// poll.
    #[must_use]
    pub fn from_wire(raw: U256) -> Self {
        if raw.is_zero() {
            Self::AtValidToPlus1
        } else if raw == U256::MAX {
            Self::Never
        } else {
            Self::At(u64::try_from(raw).unwrap_or(u64::MAX))
        }
    }
}

/// Structured outcome of a single commitment poll.
///
/// Every variant but `Post` carries `reason`, the source selector,
/// for logging only; zero when synthetic.
#[derive(Debug)]
pub enum Verdict {
    /// Tradeable now; submit `order` with its EIP-1271 `signature`.
    Post {
        /// Order ready to submit.
        order: Box<GPv2OrderData>,
        /// EIP-1271 signature blob (raw verifier bytes; the orderbook
        /// prepends `from` before settlement).
        signature: Bytes,
        /// Advisory next-poll hint; `None` on the legacy wire, which
        /// carries no hint at all.
        next_poll: Option<NextPoll>,
    },
    /// Retry once the wall clock (Unix seconds) reaches `wait_until`.
    WaitTimestamp {
        /// Re-poll at or after this Unix timestamp (seconds).
        wait_until: u64,
        /// Source selector, log only.
        reason: Selector,
    },
    /// Retry once the block number reaches `wait_until`.
    WaitBlock {
        /// Re-poll at or after this block number.
        wait_until: u64,
        /// Source selector, log only.
        reason: Selector,
    },
    /// Retry on the next block.
    TryNextBlock {
        /// Source selector, log only.
        reason: Selector,
    },
    /// Order is dead; drop the commitment.
    Invalid {
        /// Source selector, log only.
        reason: Selector,
    },
    /// Generator needs off-chain input; the keeper parks the commitment.
    NeedsInput {
        /// Source selector, log only.
        reason: Selector,
    },
}
