//! Keeper run: the poll-loop composition conditional-commitment modules
//! share.
//!
//! [`run`] first drives the shared [`reconcile`](videre_sdk::reconcile)
//! pass over the `submitted:` reserve/commit journal, then polls each
//! gate-ready watch through a [`Poller`] and applies its [`Verdict`]:
//! lifecycle outcomes update the gate and watch stores; `Post` reserves
//! the encoded body, submits once through the typed `CowClient`, and
//! commits on acceptance. A reservation whose outcome is lost is
//! resubmitted by the next tick's reconcile pass, never dropped.
//!
//! Store faults abort the run (the next tick replays it); a submission
//! failure folds into a [`RetryAction`], a `denied` refusal re-entering
//! the CoW classification by its errorType prefix ([`classify_denied`]).
//! Diagnostics go through the guest `tracing` facade.

use alloy_primitives::{Address, Bytes, hex};
use cow_venue::assembly::{gpv2_to_order_data, order_data_to_body};
use cow_venue::{CowClient, CowIntent, CowIntentBody, CowVenue, SignedOrder, classify_denied};
use cowprotocol::GPv2OrderData;
use nexum_sdk::host::{Fault, LocalStoreHost};
use nexum_sdk::keeper::{
    Disposition, Gates, Guarded, Journal, Mark, Poller, Retrier, RetryAction, Tick, WatchRef,
    WatchSet,
};
use std::task::Poll;

use videre_sdk::client::poll_once;
use videre_sdk::keeper::{retry_action, submission_key};
use videre_sdk::{
    ClientError, IntentBody as _, SubmitOutcome, Venue as _, VenueFault, VenueTransport,
};

use crate::Verdict;

/// Poll every gate-ready watch once at `tick` and apply each outcome.
/// The top-of-sweep [`reconcile`](videre_sdk::reconcile) pass resolves
/// stranded reservations first; a `Post` makes at most one submit.
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

/// Submit one polled `Post` order through the guard, folding a refusal
/// into the retry ledger.
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
        // Unknown enum marker: skip, not drop, so an SDK upgrade picks it up.
        tracing::warn!(
            "{label} submit skipped for {owner:#x}: GPv2OrderData carried an unknown enum marker"
        );
        return Ok(());
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
            return Ok(());
        }
    };
    let intent_id = submission_key(&CowVenue::ID, &encoded);
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
                return Ok(());
            }
            Guarded::Skipped(Mark::Reserved) => {
                tracing::info!("{label} {intent_id} reserved; reconcile owns it");
                return Ok(());
            }
            Guarded::Ran(outcome) => outcome,
        },
        Poll::Pending => {
            // Guest transports never suspend; retry next block, reconcile owns the marker.
            tracing::error!("{label} submit future suspended; retrying next block");
            return Retrier::new(host).apply(watch, RetryAction::TryNextBlock, tick);
        }
    };
    match outcome {
        Ok(SubmitOutcome::Accepted(receipt)) => {
            // Acceptance ends the refusal episode: clear the first-refusal marker.
            if let Err(fault) = Retrier::new(host).clear_refusal(watch) {
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
            Retrier::new(host).apply(watch, action, tick)?;
            match action {
                RetryAction::TryNextBlock => tracing::warn!("submit retry-next-block: {fault}"),
                RetryAction::Backoff { seconds } => {
                    tracing::warn!("submit backoff {seconds}s: {fault}");
                }
                RetryAction::DropOnRepeat => tracing::warn!("submit drop-on-repeat: {fault}"),
                RetryAction::Drop => tracing::warn!("submit dropped watch: {fault}"),
                // Non-exhaustive fallthrough: log the action name.
                other => {
                    let action_label: &'static str = other.into();
                    tracing::warn!("submit retry action {action_label}: {fault}");
                }
            }
        }
        Err(err) => tracing::error!("submit failed: {err}"),
    }
    Ok(())
}
