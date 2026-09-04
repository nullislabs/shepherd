//! Due index over the commitment set.
//!
//! Without it the run loop reads every commitment's gates on every
//! block, so cost scales with commitments watched rather than with
//! commitments due.
//!
//! There is one ordered range per [`Clock`], because a block height and
//! a wall-clock second cannot be ordered against each other. Both are
//! scanned each tick and each stops at its first entry past the tick.
//!
//! A commitment with no gate is indexed on [`Clock::Epoch`] at zero, so
//! it stays in the window and the gate check decides. Only a future
//! block or timestamp takes one out of the scan, which is what the
//! index exists for.
//!
//! A third range, [`expiry_key`], is keyed by a submitted order's
//! `validTo`. It collects journal rows of a commitment that never tears
//! down.

use nexum_sdk::host::{Fault, ListQuery, LocalStoreHost};
use nexum_sdk::keeper::{CommitmentRef, Gates};

/// Index prefixes, one per gate dimension. A schedule is either a block
/// height or a wall-clock second, and the two cannot be ordered against
/// each other, so they are separate ordered ranges scanned together.
const BLOCK: &str = "due-b:";
const EPOCH: &str = "due-t:";

/// Expiry range over submitted orders, ordered by the order's `validTo`.
const EXPIRY: &str = "exp-t:";

/// Commitments one clock may contribute to a tick.
///
/// A backlog larger than this is served across ticks: polling a
/// commitment reschedules it, which moves it out of the head of the
/// range, so the next tick sees the ones behind it.
const PER_CLOCK: usize = 512;

/// Expired submissions one tick may collect.
const PER_SWEEP: usize = 512;

/// Which clock a schedule is measured against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clock {
    /// Block height.
    Block,
    /// Unix seconds.
    Epoch,
}

impl Clock {
    const fn prefix(self) -> &'static str {
        match self {
            Self::Block => BLOCK,
            Self::Epoch => EPOCH,
        }
    }
}

impl From<Clock> for u8 {
    fn from(clock: Clock) -> Self {
        match clock {
            Clock::Block => 0,
            Clock::Epoch => 1,
        }
    }
}

impl TryFrom<u8> for Clock {
    type Error = MalformedRow;

    fn try_from(tag: u8) -> Result<Self, Self::Error> {
        match tag {
            0 => Ok(Self::Block),
            1 => Ok(Self::Epoch),
            _ => Err(MalformedRow),
        }
    }
}

/// A stored row this build did not write: the wrong length, or a tag
/// outside the values it knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("stored row does not match the layout this build writes")]
pub struct MalformedRow;

/// Where a commitment currently sits in the index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Slot {
    clock: Clock,
    at: u64,
}

impl Slot {
    /// Clock tag then the instant, little-endian.
    const LEN: usize = 1 + size_of::<u64>();
}

impl From<Slot> for [u8; Slot::LEN] {
    fn from(slot: Slot) -> Self {
        let mut row = [0u8; Slot::LEN];
        row[0] = slot.clock.into();
        row[1..].copy_from_slice(&slot.at.to_le_bytes());
        row
    }
}

impl TryFrom<&[u8]> for Slot {
    type Error = MalformedRow;

    /// Rejects an unknown clock tag rather than reading it as one of
    /// the two, which would delete the wrong index entry and leave the
    /// real one behind.
    fn try_from(row: &[u8]) -> Result<Self, Self::Error> {
        let [tag, at @ ..] = <[u8; Slot::LEN]>::try_from(row).map_err(|_| MalformedRow)?;
        Ok(Self {
            clock: tag.try_into()?,
            at: u64::from_le_bytes(at),
        })
    }
}

/// `due-{b,t}:{at:016x}:{owner}:{hash}`. Fixed-width hex so the
/// B-tree's lexicographic order is the numeric order of `at`. The
/// owner and hash suffix keeps entries distinct when several
/// commitments fall due at the same instant.
#[must_use]
pub fn key(commitment: CommitmentRef<'_>, clock: Clock, at: u64) -> String {
    format!(
        "{}{at:016x}:{}:{}",
        clock.prefix(),
        commitment.owner_hex(),
        commitment.hash_hex()
    )
}

/// The commitment key an index key points at.
#[must_use]
pub fn commitment_of(index_key: &str) -> Option<String> {
    let rest = index_key
        .strip_prefix(BLOCK)
        .or_else(|| index_key.strip_prefix(EPOCH))?;
    let (_at, tail) = rest.split_once(':')?;
    Some(format!("commitment:{tail}"))
}

/// Store a commitment and index it as due at once.
///
/// The one door into the commitment set: a row written without an index
/// entry is invisible to the run loop and never polled, so writing and
/// arming belong together rather than at two call sites.
///
/// # Errors
/// Propagates the store failure.
pub fn admit<H: LocalStoreHost>(
    host: &H,
    owner: &alloy_primitives::Address,
    hash: &alloy_primitives::B256,
    row: &[u8],
) -> Result<String, Fault> {
    let key = nexum_sdk::keeper::CommitmentSet::new(host).put(owner, hash, row)?;
    if let Some(commitment) = CommitmentRef::parse(&key) {
        arm(host, commitment, Clock::Epoch, 0)?;
    }
    Ok(key)
}

/// Index `commitment` as due at `at` on `clock`, replacing any earlier
/// entry on either clock.
///
/// # Errors
/// Propagates the store failure.
pub fn arm<H: LocalStoreHost>(
    host: &H,
    commitment: CommitmentRef<'_>,
    clock: Clock,
    at: u64,
) -> Result<(), Fault> {
    disarm(host, commitment)?;
    host.set(&key(commitment, clock, at), &[])?;
    host.set(
        &at_key(commitment),
        &<[u8; Slot::LEN]>::from(Slot { clock, at }),
    )
}

/// Drop `commitment` from both the index and its pointer row.
///
/// # Errors
/// Propagates the store failure.
pub fn disarm<H: LocalStoreHost>(host: &H, commitment: CommitmentRef<'_>) -> Result<(), Fault> {
    if let Some(slot) = current(host, commitment)? {
        host.delete(&key(commitment, slot.clock, slot.at))?;
    }
    host.delete(&at_key(commitment))
}

/// Pointer row naming which clock and instant `commitment` sits at.
///
/// Deliberately not derived from the gates: a teardown deletes them,
/// and a position recoverable only from a deleted row cannot be cleaned
/// up, leaving an entry pointing at a commitment that no longer exists.
/// It also records the clock, which no single gate can say.
fn at_key(commitment: CommitmentRef<'_>) -> String {
    format!(
        "due-at:{}:{}",
        commitment.owner_hex(),
        commitment.hash_hex()
    )
}

fn current<H: LocalStoreHost>(
    host: &H,
    commitment: CommitmentRef<'_>,
) -> Result<Option<Slot>, Fault> {
    Ok(host
        .get(&at_key(commitment))?
        .as_deref()
        .and_then(|row| Slot::try_from(row).ok()))
}

/// Commitment keys due at this tick, from both clocks.
///
/// Each range is ordered, so each scan stops at its first entry past
/// the tick: everything after it is later still, and a commitment
/// waiting on a future block or timestamp is never read. Several
/// commitments falling due at the same instant all appear, because the
/// owner and hash suffix keeps their keys distinct.
///
/// # Errors
/// Propagates the store failure.
pub fn due_now<H: LocalStoreHost>(host: &H, block: u64, now_s: u64) -> Result<Vec<String>, Fault> {
    let mut out = scan(host, Clock::Block, block)?;
    out.extend(scan(host, Clock::Epoch, now_s)?);
    Ok(out)
}

fn scan<H: LocalStoreHost>(host: &H, clock: Clock, now: u64) -> Result<Vec<String>, Fault> {
    let prefix = clock.prefix();
    let mut out = Vec::new();
    let mut start_after = String::new();
    loop {
        let page = host.list_entries(&ListQuery {
            prefix,
            start_after: &start_after,
            limit: u32::try_from(PER_CLOCK).unwrap_or(u32::MAX),
            scan_limit: u32::try_from(PER_CLOCK).unwrap_or(u32::MAX),
            filter: None,
        })?;
        for (index_key, _) in &page.entries {
            let Some(at) = index_key
                .strip_prefix(prefix)
                .and_then(|rest| rest.split_once(':'))
                .and_then(|(at, _)| u64::from_str_radix(at, 16).ok())
            else {
                continue;
            };
            if at > now {
                return Ok(out);
            }
            if let Some(commitment) = commitment_of(index_key) {
                out.push(commitment);
            }
            // Bound what one tick pulls into guest memory. Never silent:
            // a truncated tick is a backlog the operator should see.
            if out.len() >= PER_CLOCK {
                tracing::warn!(
                    prefix,
                    served = out.len(),
                    "due backlog exceeds the per-tick cap; the remainder waits for the next tick"
                );
                return Ok(out);
            }
        }
        if page.exhausted {
            return Ok(out);
        }
        let Some(resume) = page.last_examined else {
            return Ok(out);
        };
        start_after = resume;
    }
}

/// One submitted order past its `validTo`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expired {
    /// Journal key, as passed to `Journal::reserve`.
    pub intent_id: String,
    /// Commitment key that submitted it.
    pub commitment: String,
    /// The order's `validTo`, in Unix seconds.
    pub valid_to: u64,
}

/// `exp-t:{valid_to:016x}:{intent_id}`. Fixed-width hex, so the B-tree's
/// lexicographic order is the numeric order of `valid_to`.
#[must_use]
pub fn expiry_key(valid_to: u64, intent_id: &str) -> String {
    format!("{EXPIRY}{valid_to:016x}:{intent_id}")
}

/// Index a submitted order's journal row by when it expires.
///
/// The COMMITTED marker is one tag byte, so a row carries no expiry of
/// its own. The commitment rides in the value because the key is
/// ordered by time, not by owner.
///
/// # Errors
/// Propagates the store failure.
pub fn note_expiry<H: LocalStoreHost>(
    host: &H,
    commitment: CommitmentRef<'_>,
    valid_to: u64,
    intent_id: &str,
) -> Result<(), Fault> {
    host.set(
        &expiry_key(valid_to, intent_id),
        commitment.key().as_bytes(),
    )
}

/// Submissions whose `validTo` is before `now_s`.
///
/// Ordered, so the scan stops at the first entry still valid. An order
/// is valid for the whole of its `validTo` second.
///
/// # Errors
/// Propagates the store failure.
pub fn expired<H: LocalStoreHost>(host: &H, now_s: u64) -> Result<Vec<Expired>, Fault> {
    let mut out = Vec::new();
    let mut start_after = String::new();
    loop {
        let page = host.list_entries(&ListQuery {
            prefix: EXPIRY,
            start_after: &start_after,
            limit: u32::try_from(PER_SWEEP).unwrap_or(u32::MAX),
            scan_limit: u32::try_from(PER_SWEEP).unwrap_or(u32::MAX),
            filter: None,
        })?;
        for (index_key, commitment) in &page.entries {
            let Some((valid_to, intent_id)) = index_key
                .strip_prefix(EXPIRY)
                .and_then(|rest| rest.split_once(':'))
                .and_then(|(at, id)| Some((u64::from_str_radix(at, 16).ok()?, id)))
            else {
                continue;
            };
            if valid_to >= now_s {
                return Ok(out);
            }
            let Ok(commitment) = String::from_utf8(commitment.clone()) else {
                continue;
            };
            out.push(Expired {
                intent_id: intent_id.to_owned(),
                commitment,
                valid_to,
            });
            // Bounded, but never silent.
            if out.len() >= PER_SWEEP {
                tracing::warn!(
                    collected = out.len(),
                    "expiry backlog exceeds the per-tick cap; the remainder waits for the next tick"
                );
                return Ok(out);
            }
        }
        if page.exhausted {
            return Ok(out);
        }
        let Some(resume) = page.last_examined else {
            return Ok(out);
        };
        start_after = resume;
    }
}

/// Drop an expiry entry once its rows are collected.
///
/// # Errors
/// Propagates the store failure.
pub fn forget_expiry<H: LocalStoreHost>(
    host: &H,
    valid_to: u64,
    intent_id: &str,
) -> Result<(), Fault> {
    host.delete(&expiry_key(valid_to, intent_id))
}

/// Re-arm `commitment` on the epoch clock and set its epoch gate.
///
/// Index first, then gate: a fault between them leaves the commitment
/// in the scan with a stale gate, which costs a wasted read, where the
/// reverse would gate it out of a scan that no longer lists it.
///
/// # Errors
/// Propagates the store failure.
pub fn schedule_epoch<H: LocalStoreHost>(
    host: &H,
    gates: &Gates<'_, H>,
    commitment: CommitmentRef<'_>,
    next_epoch_s: u64,
) -> Result<(), Fault> {
    arm(host, commitment, Clock::Epoch, next_epoch_s)?;
    gates.set_next_epoch(commitment, next_epoch_s)
}

/// Re-arm `commitment` on the block clock and set its block gate.
///
/// Ordered as [`schedule_epoch`], and for the same reason.
///
/// # Errors
/// Propagates the store failure.
pub fn schedule_block<H: LocalStoreHost>(
    host: &H,
    gates: &Gates<'_, H>,
    commitment: CommitmentRef<'_>,
    next_block: u64,
) -> Result<(), Fault> {
    arm(host, commitment, Clock::Block, next_block)?;
    gates.set_next_block(commitment, next_block)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_of(slot: Slot) -> [u8; Slot::LEN] {
        slot.into()
    }

    #[test]
    fn a_slot_round_trips() {
        for clock in [Clock::Block, Clock::Epoch] {
            let slot = Slot {
                clock,
                at: 1_234_567,
            };
            assert_eq!(Slot::try_from(&row_of(slot)[..]), Ok(slot));
        }
    }

    /// A row this build did not write is rejected rather than read as
    /// one of the two clocks, which would delete the wrong index entry
    /// and leave the real one behind.
    #[test]
    fn an_unknown_clock_tag_is_rejected() {
        let mut row = row_of(Slot {
            clock: Clock::Epoch,
            at: 9,
        });
        row[0] = 7;
        assert_eq!(Slot::try_from(&row[..]), Err(MalformedRow));
    }

    #[test]
    fn a_wrong_length_row_is_rejected() {
        assert_eq!(Slot::try_from(&[][..]), Err(MalformedRow));
        assert_eq!(Slot::try_from(&[1; Slot::LEN - 1][..]), Err(MalformedRow));
        assert_eq!(Slot::try_from(&[1; Slot::LEN + 1][..]), Err(MalformedRow));
    }

    /// Fixed-width hex, so the store's lexicographic order is the
    /// numeric order the scan depends on.
    #[test]
    fn index_keys_sort_numerically() {
        let commitment = CommitmentRef::parse(
            "commitment:0x00112233445566778899aabbccddeeff00112233:0x0000000000000000000000000000000000000000000000000000000000000001",
        )
        .expect("valid key");
        let mut keys = [
            key(commitment, Clock::Epoch, 10),
            key(commitment, Clock::Epoch, 9),
            key(commitment, Clock::Epoch, 100),
        ];
        keys.sort();
        assert_eq!(
            keys,
            [
                key(commitment, Clock::Epoch, 9),
                key(commitment, Clock::Epoch, 10),
                key(commitment, Clock::Epoch, 100),
            ],
        );
    }
}
