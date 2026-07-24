//! Keeper run: the poll-loop composition conditional-
//! commitment modules share.
//!
//! [`run`] first drives the shared
//! [`reconcile`](videre_sdk::reconcile) pass over the `submitted:`
//! reserve/commit journal, then walks the keeper watch set, polls each
//! gate-ready watch through a [`Poller`], and runs the
//! [`Verdict`]'s effect: lifecycle outcomes update the gate and
//! watch stores, `Post` reserves the encoded body on the venue-and-body
//! submission key and drives one submission through the typed
//! [`CowClient`] onto the `videre:venue/client` seam, committing on
//! acceptance, with the keeper [`Retrier`] as the failure dispatch. A
//! reservation whose submit outcome is lost is resubmitted by the next
//! tick's reconcile pass, never dropped.
//!
//! Store faults abort the run (the next tick replays it);
//! submission failures never do - they fold into a
//! [`RetryAction`] through the videre
//! [`retry_action`] table, a `denied` refusal re-entering the CoW
//! classification through its errorType prefix
//! ([`classify_denied`]) so a one-shot row survives the coarse
//! collapse, the ledger applies the effect, and the
//! run moves on. Diagnostics go through the guest `tracing` facade -
//! the same channel strategy code logs on - so module tests observe
//! the composed behaviour with one capture.

use alloy_primitives::{Address, Bytes, hex};
use cow_venue::assembly::{gpv2_to_order_data, order_data_to_body};
use cow_venue::{CowClient, CowIntent, CowIntentBody, CowVenue, SignedOrder, classify_denied};
use cowprotocol::GPv2OrderData;
use nexum_sdk::host::{Fault, LocalStoreHost};
use nexum_sdk::keeper::{
    Gates, Journal, Mark, Poller, Retrier, RetryAction, Tick, WatchRef, WatchSet,
};
use std::task::Poll;

use videre_sdk::client::poll_once;
use videre_sdk::keeper::{retry_action, submission_key};
use videre_sdk::{
    ClientError, IntentBody as _, SubmitOutcome, Venue as _, VenueFault, VenueTransport,
};

use crate::Verdict;

/// Poll every gate-ready watch once at `tick` and run each outcome's
/// effect. The top-of-sweep [`reconcile`](videre_sdk::reconcile) pass
/// resolves stranded reservations first, then one source poll per ready
/// watch; a `Post` outcome makes at most one venue submit through
/// `venue`.
pub fn run<H, S, T>(host: &H, venue: &CowClient<T>, source: &S, tick: &Tick) -> Result<(), Fault>
where
    H: LocalStoreHost,
    S: Poller<H, Outcome = Verdict>,
    T: VenueTransport,
{
    // Resolve any stranded reservation before polling fresh watches, so a
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

    let watches = WatchSet::new(host);
    let gates = Gates::new(host);
    for key in watches.list()? {
        let Some(watch) = WatchRef::parse(&key) else {
            continue;
        };
        if !gates.is_ready(watch, tick.block, tick.epoch_s)? {
            continue;
        }
        let Some(params) = watches.get(watch)? else {
            continue;
        };
        match source.poll(host, watch, &params, tick) {
            Verdict::Post {
                order, signature, ..
            } => {
                submit_ready(host, venue, watch, &order, signature, tick, source.label())?;
            }
            Verdict::TryNextBlock { .. } => {}
            Verdict::WaitBlock { wait_until, .. } => gates.set_next_block(watch, wait_until)?,
            Verdict::WaitTimestamp { wait_until, .. } => gates.set_next_epoch(watch, wait_until)?,
            Verdict::Invalid { .. } => {
                // The removal is permanent; leave a trace of it even
                // for sources that do not log their own outcomes.
                tracing::info!("{} dropped watch {}", source.label(), watch.key());
                watches.remove(watch)?;
            }
            Verdict::NeedsInput { .. } => {
                tracing::info!("watch {} parked awaiting input", watch.key());
            }
        }
    }
    Ok(())
}

/// Submit one freshly-polled `Ready` order through the typed client on
/// the `submitted:` reserve/commit journal, dispatching any venue
/// refusal through the retry ledger.
///
/// The journal keys on the deterministic venue-and-body submission key.
/// A `COMMITTED` marker is an idempotent skip; a `RESERVED` marker is
/// owned by this tick's reconcile pass and never re-submitted here. A
/// fresh order reserves its encoded body before the submit and commits
/// on acceptance; release runs only on a known synchronous non-accept,
/// never on a pending or accepted path.
fn submit_ready<H, T>(
    host: &H,
    venue: &CowClient<T>,
    watch: WatchRef<'_>,
    order: &GPv2OrderData,
    signature: Bytes,
    tick: &Tick,
    label: &str,
) -> Result<(), Fault>
where
    H: LocalStoreHost,
    T: VenueTransport,
{
    let Ok(owner) = watch.owner_hex().parse::<Address>() else {
        tracing::warn!(
            "watch {} carries an unparseable owner; skipping submit",
            watch.key(),
        );
        return Ok(());
    };

    let Some(order_data) = gpv2_to_order_data(order) else {
        // An unknown enum marker means the SDK cannot express this
        // payload yet; skip rather than drop so an SDK upgrade can
        // still pick the watch up.
        tracing::warn!(
            "{label} submit skipped for {owner:#x}: GPv2OrderData carried an unknown enum marker"
        );
        return Ok(());
    };

    let intent = CowIntentBody::V1(CowIntent::Signed(SignedOrder {
        order: order_data_to_body(&order_data),
        owner: owner.into_array(),
        signature: signature.to_vec(),
    }));
    // Reserve the exact wire bytes the venue submit and the reconcile
    // resubmit both carry, so the id and the reservation agree.
    let encoded = match intent.to_bytes() {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::error!("intent body encode failed: {err}");
            return Ok(());
        }
    };
    let intent_id = submission_key(&CowVenue::ID, &encoded);
    let journal = Journal::submitted(host);
    match journal.mark(&intent_id)? {
        Some(Mark::Committed) => {
            tracing::info!("{label} {intent_id} already committed; skipping re-submit");
            return Ok(());
        }
        Some(Mark::Reserved) => {
            // Owned by this tick's reconcile pass; never a second submit.
            tracing::info!("{label} {intent_id} reserved; reconcile owns it");
            return Ok(());
        }
        None => {}
    }
    // Reserve the real body before any network work: a crash or lost
    // outcome now strands a RESERVED marker the next tick's reconcile
    // resolves, never a silent drop (#572).
    journal.reserve(&intent_id, &encoded)?;

    let Poll::Ready(outcome) = poll_once(venue.submit(&intent)) else {
        // Guest transports never suspend; a pending future means a
        // foreign transport misbehaved. Leave the marker RESERVED for the
        // next tick's reconcile and retry the watch next block; never
        // release on a pending path.
        tracing::error!("{label} submit future suspended; retrying next block");
        return Retrier::new(host).apply(watch, RetryAction::TryNextBlock, tick);
    };
    match outcome {
        Ok(SubmitOutcome::Accepted(receipt)) => {
            // The submit landed; commit the reservation best-effort. A
            // commit fault leaves the marker RESERVED for reconcile, never
            // released or aborted.
            if let Err(fault) = journal.commit(&intent_id) {
                tracing::error!("submitted {intent_id} but commit write failed: {fault}");
            }
            // An acceptance ends any refusal episode: clear the
            // first-refusal marker so a later independent refusal earns a
            // fresh one-block grace.
            if let Err(fault) = Retrier::new(host).clear_refusal(watch) {
                tracing::error!("submitted {intent_id} but refusal-marker clear failed: {fault}");
            }
            tracing::info!(
                "submitted {intent_id} (receipt {})",
                hex::encode_prefixed(&receipt),
            );
        }
        Ok(SubmitOutcome::RequiresSigning(_)) => {
            // A known non-accept: a run cannot sign, so release the reserve
            // and re-pose the ask next tick.
            journal.release(&intent_id)?;
            tracing::warn!("{label} submit for {owner:#x} requires signing; not journalled");
        }
        Err(ClientError::Body(err)) => {
            // A known non-accept before any order is placed: release.
            journal.release(&intent_id)?;
            tracing::error!("intent body encode failed: {err}");
        }
        Err(ClientError::Venue(fault)) => {
            // A known venue refusal: release the reserve, then fold it
            // through the ledger.
            journal.release(&intent_id)?;
            let action = match &fault {
                VenueFault::Denied(detail) => classify_denied(detail),
                other => retry_action(other),
            };
            Retrier::new(host).apply(watch, action, tick)?;
            match action {
                RetryAction::TryNextBlock => tracing::warn!("submit retry-next-block: {fault}"),
                RetryAction::Backoff { seconds } => {
                    tracing::warn!("submit backoff {seconds}s: {fault}");
                }
                RetryAction::DropOnRepeat => tracing::warn!("submit drop-on-repeat: {fault}"),
                RetryAction::Drop => tracing::warn!("submit dropped watch: {fault}"),
                // `RetryAction` is non-exhaustive; the ledger already
                // ran the effect, so the log needs only the name.
                other => {
                    let action_label: &'static str = other.into();
                    tracing::warn!("submit retry action {action_label}: {fault}");
                }
            }
        }
        // `ClientError` is non-exhaustive; an unknown outcome is neither a
        // known accept nor a known refusal, so leave the marker RESERVED
        // for reconcile rather than releasing.
        Err(err) => tracing::error!("submit failed: {err}"),
    }
    Ok(())
}
