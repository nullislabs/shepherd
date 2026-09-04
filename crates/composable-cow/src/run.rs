//! Keeper run: the poll-loop composition conditional-commitment modules
//! share.
//!
//! [`run`] first drives the shared [`reconcile`](videre_sdk::reconcile)
//! pass over the `submitted:` reserve/commit journal, then polls each
//! gate-ready commitment through a [`Poller`] and applies its
//! [`Verdict`]: lifecycle outcomes update the gate and commitment
//! stores; `Post` reserves the encoded body, submits once through the
//! typed `CowClient`, and commits on acceptance. A reservation whose
//! outcome is lost is resubmitted by the next tick's reconcile pass,
//! never dropped.
//!
//! Store faults abort the run (the next tick replays it); a submission
//! failure folds into a [`RetryAction`], a `denied` refusal re-entering
//! the CoW classification by its errorType prefix ([`classify_denied`]).
//! Diagnostics go through the guest `tracing` facade.

use alloy_primitives::{Address, Bytes, hex};
use cow_venue::assembly::{gpv2_to_order_data, order_data_to_body};
use cow_venue::{CowClient, CowIntent, CowIntentBody, CowVenue, SignedOrder, classify_denied};
use cowprotocol::GPv2OrderData;
use nexum_sdk::host::{Fault, ListQuery, LocalStoreHost};
use nexum_sdk::keeper::{
    CommitmentRef, CommitmentSet, Disposition, Gates, Guarded, Journal, Mark, Poller, Retrier,
    RetryAction, Tick,
};
use std::task::Poll;

use videre_sdk::client::poll_once;
use videre_sdk::keeper::{retry_action, submission_key};
use videre_sdk::{
    ClientError, IntentBody as _, SubmitOutcome, Venue as _, VenueFault, VenueTransport,
};

use crate::{NextPoll, ParkReason, Verdict};

/// Poll every gate-ready commitment once at `tick` and apply each outcome.
/// The top-of-sweep [`reconcile`](videre_sdk::reconcile) pass resolves
/// stranded reservations first; a `Post` makes at most one submit.
pub fn run<H, S, T>(host: &H, venue: &CowClient<T>, source: &S, tick: &Tick) -> Result<(), Fault>
where
    H: LocalStoreHost,
    S: Poller<H, Outcome = Verdict>,
    T: VenueTransport,
{
    // Ahead of reconcile, so an expired order is collected rather than
    // resubmitted.
    sweep_expired(host, tick.epoch_s)?;
    // Resolve any stranded reservation before polling fresh commitments, so a
    // submit whose outcome was lost is resubmitted, never dropped (#572).
    // The helper is async and the guest boundary synchronous, so drive it
    // with `poll_once`.
    let journal = Journal::submitted(host);
    match poll_once(videre_sdk::reconcile(
        &CowVenue::ID,
        venue.transport(),
        &journal,
        tick,
        videre_sdk::DEFAULT_RECONCILE_BUDGET,
    )) {
        Poll::Ready(res) => {
            res?;
        }
        Poll::Pending => {
            // A misbehaving guest transport suspended; leave the RESERVED
            // markers for the next tick rather than dropping them.
            tracing::error!("cow reconcile suspended; skipping this tick");
        }
    }

    let commitments = CommitmentSet::new(host);
    let gates = Gates::new(host);
    // Only commitments the index says are due; one waiting on a future
    // block or timestamp is never read.
    for key in &crate::due::due_now(host, tick.block, tick.epoch_s)? {
        let Some(commitment) = CommitmentRef::parse(key) else {
            continue;
        };
        if !gates.is_ready(commitment, tick.block, tick.epoch_s)? {
            continue;
        }
        let Some(params) = commitments.get(commitment)? else {
            continue;
        };
        match source.poll(host, commitment, &params, tick) {
            Verdict::Post {
                order,
                signature,
                next_poll,
            } => {
                let submitted = submit_ready(
                    host,
                    venue,
                    commitment,
                    &order,
                    signature,
                    tick,
                    source.label(),
                )?;
                // The retry ledger can drop a commitment from inside the
                // SDK, which knows nothing about the index, so the entry
                // would outlive the row it points at.
                if commitments.get(commitment)?.is_none() {
                    // `retire` is idempotent, and the row is already
                    // gone, so this collects what the ledger could not
                    // reach: the index entry and the journal rows.
                    retire(host, commitment)?;
                } else if submitted == Submitted::Accepted {
                    match next_poll {
                        // The generator posted its last order, so
                        // nothing will re-arm this commitment.
                        Some(NextPoll::Never) => {
                            tracing::info!(
                                "{} completed commitment {} after its final order",
                                source.label(),
                                commitment.key()
                            );
                            retire(host, commitment)?;
                        }
                        Some(hint) => {
                            let at = crate::fork::schedule_at(hint, order.validTo);
                            apply_schedule(host, &gates, commitment, at, tick)?;
                        }
                        // The legacy wire carried no hint; leave the
                        // commitment where the index already has it.
                        None => {}
                    }
                }
            }
            Verdict::TryNextBlock { .. } => {}
            Verdict::WaitBlock { wait_until, .. } => {
                crate::due::schedule_block(host, &gates, commitment, wait_until)?;
            }
            Verdict::WaitTimestamp { wait_until, .. } => {
                apply_schedule(host, &gates, commitment, wait_until, tick)?;
            }
            Verdict::Invalid { .. } => {
                // The removal is permanent; leave a trace of it even
                // for sources that do not log their own outcomes.
                tracing::info!("{} dropped commitment {}", source.label(), commitment.key());
                retire(host, commitment)?;
            }
            Verdict::Park { why, .. } => {
                let row = Vec::from(ParkedRow {
                    why,
                    since_block: tick.block,
                    params: &params,
                });
                host.set(&parked_key(commitment), &row)?;
                crate::due::disarm(host, commitment)?;
                gates.clear(commitment)?;
                tracing::info!(
                    "{} parked commitment {}: {why:?}",
                    source.label(),
                    commitment.key()
                );
            }
            Verdict::Complete => {
                // The generator reported no successor. Nothing will ever
                // re-arm this, so keeping the row would leak it.
                tracing::info!(
                    "{} completed commitment {}",
                    source.label(),
                    commitment.key()
                );
                retire(host, commitment)?;
            }
        }
    }
    Ok(())
}

/// A commitment out of the poll rotation, and why.
fn parked_key(commitment: CommitmentRef<'_>) -> String {
    format!(
        "parked:{}:{}",
        commitment.owner_hex(),
        commitment.hash_hex()
    )
}

/// Whether a submit attempt posted the order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Submitted {
    /// On the book. The generator's hint governs the next poll.
    Accepted,
    /// Not posted. The retry ledger owns the schedule, so a hint must
    /// not push the retry out to the next tranche.
    Deferred,
}

/// Apply a schedule to `commitment`.
///
/// A time at or before this tick means "as soon as possible", which is
/// the next block. Expressing that as a block gate keeps the floor at
/// one block on any chain rather than assuming a block time.
fn apply_schedule<H: LocalStoreHost>(
    host: &H,
    gates: &Gates<'_, H>,
    commitment: CommitmentRef<'_>,
    at_s: u64,
    tick: &Tick,
) -> Result<(), Fault> {
    if at_s <= tick.epoch_s {
        crate::due::schedule_block(host, gates, commitment, tick.block.saturating_add(1))
    } else {
        crate::due::schedule_epoch(host, gates, commitment, at_s)
    }
}

/// Index prefix pairing a commitment with the journal rows it wrote.
const SUBMISSION_INDEX: &str = "watch-sub:";

/// `watch-sub:{owner}:{hash}:{intent_id}`, valued with the order's
/// `validTo` little-endian.
///
/// The journal is keyed on the body digest, which carries no commitment
/// identity; this index is the missing direction. The value carries the
/// expiry because `exp-t:` is ordered by time, so a teardown has no
/// other way to reconstruct that entry.
fn submission_index_key(commitment: CommitmentRef<'_>, intent_id: &str) -> String {
    format!(
        "{SUBMISSION_INDEX}{}:{}:{intent_id}",
        commitment.owner_hex(),
        commitment.hash_hex()
    )
}

/// Prefix covering every journal row a commitment wrote.
fn submission_index_prefix(commitment: CommitmentRef<'_>) -> String {
    format!(
        "{SUBMISSION_INDEX}{}:{}:",
        commitment.owner_hex(),
        commitment.hash_hex()
    )
}

/// Retire a commitment and everything keyed to it.
///
/// The one teardown door. Each caller used to run its own sequence, and
/// they had already drifted: the `Complete` arm removed the row without
/// disarming, leaking an index entry the scan then returned forever.
///
/// # Errors
/// Propagates the store failure.
pub fn retire<H: LocalStoreHost>(host: &H, commitment: CommitmentRef<'_>) -> Result<(), Fault> {
    sweep_submissions(host, commitment)?;
    crate::due::disarm(host, commitment)?;
    unpark(host, commitment)?;
    // Takes the gate and refusal keys with it.
    CommitmentSet::new(host).remove(commitment)
}

/// Release the journal rows this commitment wrote, and drop both
/// indexes over them.
///
/// A hint, not a mirror: `reconcile` releases reservations inside
/// videre-sdk, which cannot update it, so an entry may name a row
/// already gone. `Journal::release` no-ops on an absent key.
///
/// A value this build did not write leaves its expiry entry to
/// [`sweep_expired`] rather than guessing at it.
fn sweep_submissions<H: LocalStoreHost>(
    host: &H,
    commitment: CommitmentRef<'_>,
) -> Result<(), Fault> {
    let prefix = submission_index_prefix(commitment);
    let journal = Journal::submitted(host);
    let mut start_after = String::new();
    loop {
        // Entries, not keys: the value carries the expiry, and a
        // per-key read would be one store round trip per row.
        let page = host.list_entries(&ListQuery {
            prefix: &prefix,
            start_after: &start_after,
            limit: 0,
            scan_limit: 0,
            filter: None,
        })?;
        for (index_key, valid_to) in &page.entries {
            if let Some(intent_id) = index_key.strip_prefix(prefix.as_str()) {
                journal.release(intent_id)?;
                if let Ok(bytes) = <[u8; 8]>::try_from(valid_to.as_slice()) {
                    crate::due::forget_expiry(host, u64::from_le_bytes(bytes), intent_id)?;
                }
            }
            // Last: the entry is the way back to both rows above.
            host.delete(index_key)?;
        }
        if page.exhausted {
            return Ok(());
        }
        let Some(resume) = page.last_examined else {
            return Ok(());
        };
        start_after = resume;
    }
}

/// Collect the journal rows of orders that can no longer fill.
///
/// Nothing tears down a commitment that keeps minting, so a long-lived
/// TWAP outlives every order it posts and those rows have no other
/// collector. Both deletes are no-ops when a teardown got there first.
fn sweep_expired<H: LocalStoreHost>(host: &H, now_s: u64) -> Result<(), Fault> {
    let journal = Journal::submitted(host);
    for entry in &crate::due::expired(host, now_s)? {
        journal.release(&entry.intent_id)?;
        if let Some(commitment) = CommitmentRef::parse(&entry.commitment) {
            host.delete(&submission_index_key(commitment, &entry.intent_id))?;
        }
        // Last: the entry is the only way back to the rows above.
        crate::due::forget_expiry(host, entry.valid_to, &entry.intent_id)?;
    }
    Ok(())
}

/// Why and when a commitment was parked, ahead of its stored row.
///
/// The row rides along so a re-arming pass has the handler without
/// re-reading the commitment. It is carried whole rather than sliced
/// because only the source that wrote it knows its layout; this loop is
/// generic over the row format.
struct ParkedRow<'a> {
    why: ParkReason,
    since_block: u64,
    params: &'a [u8],
}

impl ParkedRow<'_> {
    /// Reason tag, then the block little-endian, then the handler.
    const HEADER: usize = 1 + size_of::<u64>();
}

impl From<ParkedRow<'_>> for Vec<u8> {
    fn from(parked: ParkedRow<'_>) -> Self {
        let mut row = Self::with_capacity(ParkedRow::HEADER + parked.params.len());
        row.push(match parked.why {
            ParkReason::NeedsInput => 0,
            ParkReason::Unpollable => 1,
        });
        row.extend_from_slice(&parked.since_block.to_le_bytes());
        row.extend_from_slice(parked.params);
        row
    }
}

/// Clear a park row, so a re-registered order returns to the rotation.
///
/// # Errors
/// Propagates the store failure.
pub fn unpark<H: LocalStoreHost>(host: &H, commitment: CommitmentRef<'_>) -> Result<(), Fault> {
    host.delete(&parked_key(commitment))
}

/// Submit one polled `Post` order through the guard, folding a refusal
/// into the retry ledger.
fn submit_ready<H, T>(
    host: &H,
    venue: &CowClient<T>,
    commitment: CommitmentRef<'_>,
    order: &GPv2OrderData,
    signature: Bytes,
    tick: &Tick,
    label: &str,
) -> Result<Submitted, Fault>
where
    H: LocalStoreHost,
    T: VenueTransport,
{
    let Ok(owner) = commitment.owner_hex().parse::<Address>() else {
        tracing::warn!(
            "commitment {} carries an unparseable owner; skipping submit",
            commitment.key(),
        );
        return Ok(Submitted::Deferred);
    };

    let Some(order_data) = gpv2_to_order_data(order) else {
        // Unknown enum marker: skip, not drop, so an SDK upgrade picks it up.
        tracing::warn!(
            "{label} submit skipped for {owner:#x}: GPv2OrderData carried an unknown enum marker"
        );
        return Ok(Submitted::Deferred);
    };

    let intent = CowIntentBody::V1(CowIntent::Signed(SignedOrder {
        order: order_data_to_body(&order_data),
        owner,
        signature: signature.to_vec(),
    }));
    // Key on the exact bytes the submit and reconcile both carry.
    let encoded = match intent.to_bytes() {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::error!("intent body encode failed: {err}");
            return Ok(Submitted::Deferred);
        }
    };
    let intent_id = submission_key(&CowVenue::ID, &encoded);
    // Both hints precede the reservation: a fault between them leaves a
    // hint naming no row, which sweeps harmlessly, where the reverse
    // leaves a row no teardown can find.
    let valid_to = u64::from(order.validTo);
    host.set(
        &submission_index_key(commitment, &intent_id),
        &valid_to.to_le_bytes(),
    )?;
    crate::due::note_expiry(host, commitment, valid_to, &intent_id)?;
    let journal = Journal::submitted(host);
    // The guard owns the reserve/commit/reconcile ordering (#572).
    let guarded = poll_once(journal.guard(&intent_id, &encoded, || async {
        let outcome = venue.submit(&intent).await;
        let disposition = match &outcome {
            Ok(SubmitOutcome::Accepted(_)) => Disposition::Commit,
            Ok(SubmitOutcome::RequiresSigning(_))
            | Err(ClientError::Body(_))
            | Err(ClientError::Venue(_)) => Disposition::Release,
            // Unknown outcome: hold for reconcile.
            Err(_) => Disposition::Retain,
        };
        (disposition, outcome)
    }));
    let outcome = match guarded {
        Poll::Ready(res) => match res? {
            Guarded::Skipped(Mark::Committed) => {
                tracing::info!("{label} {intent_id} already committed; skipping re-submit");
                // Already on the book, so the schedule advances as if
                // this attempt had posted it.
                return Ok(Submitted::Accepted);
            }
            Guarded::Skipped(Mark::Reserved) => {
                tracing::info!("{label} {intent_id} reserved; reconcile owns it");
                return Ok(Submitted::Deferred);
            }
            Guarded::Ran(outcome) => outcome,
        },
        Poll::Pending => {
            // Guest transports never suspend; retry next block, reconcile owns the marker.
            tracing::error!("{label} submit future suspended; retrying next block");
            Retrier::new(host).apply(commitment, RetryAction::TryNextBlock, tick)?;
            return Ok(Submitted::Deferred);
        }
    };
    let mut outcome_state = Submitted::Deferred;
    match outcome {
        Ok(SubmitOutcome::Accepted(receipt)) => {
            outcome_state = Submitted::Accepted;
            // Acceptance ends the refusal episode: clear the first-refusal marker.
            if let Err(fault) = Retrier::new(host).clear_refusal(commitment) {
                tracing::error!("submitted {intent_id} but refusal-marker clear failed: {fault}");
            }
            tracing::info!(
                "submitted {intent_id} (receipt {})",
                hex::encode_prefixed(&receipt),
            );
        }
        Ok(SubmitOutcome::RequiresSigning(_)) => {
            tracing::warn!("{label} submit for {owner:#x} requires signing; not journalled");
        }
        Err(ClientError::Body(err)) => {
            tracing::error!("intent body encode failed: {err}");
        }
        Err(ClientError::Venue(fault)) => {
            let action = match &fault {
                VenueFault::Denied(detail) => classify_denied(detail),
                other => retry_action(other),
            };
            Retrier::new(host).apply(commitment, action, tick)?;
            match action {
                RetryAction::TryNextBlock => tracing::warn!("submit retry-next-block: {fault}"),
                RetryAction::Backoff { seconds } => {
                    tracing::warn!("submit backoff {seconds}s: {fault}");
                }
                RetryAction::DropOnRepeat => tracing::warn!("submit drop-on-repeat: {fault}"),
                RetryAction::Drop => tracing::warn!("submit dropped commitment: {fault}"),
                // Non-exhaustive fallthrough: log the action name.
                other => {
                    let action_label: &'static str = other.into();
                    tracing::warn!("submit retry action {action_label}: {fault}");
                }
            }
        }
        Err(err) => tracing::error!("submit failed: {err}"),
    }
    Ok(outcome_state)
}
