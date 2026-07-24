//! Keeper sweep: the poll-loop composition conditional-
//! commitment modules share.
//!
//! [`run`] walks the keeper watch set, polls each gate-ready
//! watch through a [`ConditionalSource`], and runs the
//! [`Verdict`]'s effect: lifecycle outcomes update the gate and
//! watch stores, `Post` drives one submission through the typed
//! [`CowClient`] onto the `videre:venue/client` seam with the
//! `submitted:` journal as the idempotency guard - keyed on the
//! venue-and-body [`intent_id`] - and the keeper [`Retrier`]
//! as the failure dispatch.
//!
//! Store faults abort the sweep (the next tick replays it);
//! submission failures never do - they fold into a
//! [`RetryAction`] through the videre
//! [`retry_action`] table, a `denied` refusal re-entering the CoW
//! classification through its errorType prefix
//! ([`classify_denied`]) so a one-shot row survives the coarse
//! collapse, the ledger applies the effect, and the
//! sweep moves on. Diagnostics go through the guest `tracing` facade -
//! the same channel strategy code logs on - so module tests observe
//! the composed behaviour with one capture.

use alloy_primitives::{Address, Bytes, hex};
use cow_venue::assembly::{gpv2_to_order_data, order_data_to_body};
use cow_venue::{CowClient, CowIntent, CowIntentBody, SignedOrder, classify_denied, intent_id};
use cowprotocol::GPv2OrderData;
use nexum_sdk::host::{Fault, LocalStoreHost};
use nexum_sdk::keeper::{
    ConditionalSource, Gates, Journal, Retrier, RetryAction, Tick, WatchRef, WatchSet,
};
use videre_sdk::keeper::retry_action;
use videre_sdk::{ClientError, SubmitOutcome, VenueFault, VenueTransport, rt};

use crate::Verdict;

/// Poll every gate-ready watch once at `tick` and run each outcome's
/// effect. One source poll per ready watch; a `Post` outcome makes at
/// most one venue submit through `venue`.
pub fn run<H, S, T>(host: &H, venue: &CowClient<T>, source: &S, tick: &Tick) -> Result<(), Fault>
where
    H: LocalStoreHost,
    S: ConditionalSource<H, Outcome = Verdict>,
    T: VenueTransport,
{
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

/// Submit one freshly-polled `Ready` order through the typed client,
/// guarding on the `submitted:` journal and dispatching any venue
/// refusal through the retry ledger.
///
/// The journal keys on the deterministic venue-and-body [`intent_id`],
/// derived before any network work from the same body bytes the venue
/// submit carries, so the guard is independent of where assembly
/// happens. The id is reserved before the submit and committed on
/// acceptance, so a record write that faults after acceptance still
/// leaves a marker and the next tick does not re-post; a non-accepting
/// outcome releases the reservation. The venue's receipt rides the log
/// only.
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
    let intent_id = match intent_id(&intent) {
        Ok(id) => id,
        Err(err) => {
            tracing::error!("intent body encode failed: {err}");
            return Ok(());
        }
    };
    let journal = Journal::submitted(host);
    if journal.contains(&intent_id)? {
        tracing::info!("{label} {intent_id} already submitted; skipping re-submit");
        return Ok(());
    }
    // Reserve before the venue call: a record write that faults after
    // acceptance still leaves this marker, so no next tick re-posts an
    // accepted body. A non-accepting outcome releases it.
    journal.reserve(&intent_id)?;

    let Some(outcome) = rt::complete(venue.submit(&intent)) else {
        // Guest transports never suspend; a pending future means a
        // foreign transport misbehaved. The watch stays for the next
        // tick.
        journal.release(&intent_id)?;
        tracing::error!("{label} submit future suspended; retrying next tick");
        return Ok(());
    };
    match outcome {
        Ok(SubmitOutcome::Accepted(receipt)) => {
            // An acceptance ends any refusal episode: clear the
            // first-refusal marker so a later independent refusal
            // earns a fresh one-block grace.
            if let Err(fault) = Retrier::new(host).clear_refusal(watch) {
                tracing::error!("submitted {intent_id} but refusal-marker clear failed: {fault}");
            }
            // Commit best-effort: the reservation already guards the
            // re-post, so a record fault here must not abort the sweep
            // or unwind the accepted order.
            if let Err(fault) = journal.record(&intent_id) {
                tracing::error!("submitted {intent_id} but journal write failed: {fault}");
            }
            tracing::info!(
                "submitted {intent_id} (receipt {})",
                hex::encode_prefixed(&receipt),
            );
        }
        Ok(SubmitOutcome::RequiresSigning(_)) => {
            // A sweep cannot sign; the reservation is released, so the
            // next tick surfaces the same ask afresh.
            journal.release(&intent_id)?;
            tracing::warn!("{label} submit for {owner:#x} requires signing; not journalled");
        }
        Err(ClientError::Body(err)) => {
            journal.release(&intent_id)?;
            tracing::error!("intent body encode failed: {err}");
        }
        Err(ClientError::Venue(fault)) => {
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
        // `ClientError` is non-exhaustive; a future case releases the
        // reservation and leaves the watch for the next tick.
        Err(err) => {
            journal.release(&intent_id)?;
            tracing::error!("submit failed: {err}");
        }
    }
    Ok(())
}
