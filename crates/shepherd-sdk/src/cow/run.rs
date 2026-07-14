//! Keeper run: the poll-loop composition conditional-
//! commitment modules share.
//!
//! [`run`] walks the keeper watch set, polls each gate-ready
//! watch through a [`ConditionalSource`], and runs the
//! [`Verdict`]'s effect: lifecycle outcomes update the gate and
//! watch stores, `Post` drives one submission through the
//! [`CowApiHost`](super::CowApiHost) seam with the `submitted:`
//! journal as the idempotency guard and the keeper [`Retrier`]
//! as the failure dispatch.
//!
//! Store faults abort the sweep (the next tick replays it);
//! submission failures never do - they classify into a
//! [`RetryAction`], the ledger applies the effect, and the sweep
//! moves on. Diagnostics go through the guest `tracing` facade -
//! the same channel strategy code logs on - so module tests observe
//! the composed behaviour with one capture.

use alloy_primitives::{Address, Bytes};
use cowprotocol::{GPv2OrderData, OrderCreation, OrderData, Signature};
use nexum_sdk::host::Fault;
use nexum_sdk::keeper::{
    ConditionalSource, Gates, Journal, Retrier, RetryAction, Tick, WatchRef, WatchSet,
};

use super::{
    CowApiError, CowHost, Verdict, classify_submit_error, gpv2_to_order_data, is_already_submitted,
    order_uid_hex,
};

/// Poll every gate-ready watch once at `tick` and run each outcome's
/// effect. One source poll per ready watch; a `Post` outcome makes at
/// most one `submit_order` call.
pub fn run<H, S>(host: &H, source: &S, tick: &Tick) -> Result<(), Fault>
where
    H: CowHost,
    S: ConditionalSource<H, Outcome = Verdict>,
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
                submit_ready(host, watch, &order, signature, tick, source.label())?;
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

/// Submit one freshly-polled `Ready` order, guarding on the
/// `submitted:` journal and dispatching any failure through the retry
/// ledger.
///
/// The UID is deterministic from on-chain inputs, so the idempotency
/// check runs before any network work; the same value keys the journal
/// marker after, so the read and write paths agree.
fn submit_ready<H: CowHost>(
    host: &H,
    watch: WatchRef<'_>,
    order: &GPv2OrderData,
    signature: Bytes,
    tick: &Tick,
    label: &str,
) -> Result<(), Fault> {
    let Ok(owner) = watch.owner_hex().parse::<Address>() else {
        tracing::warn!(
            "watch {} carries an unparseable owner; skipping submit",
            watch.key(),
        );
        return Ok(());
    };

    let journal = Journal::submitted(host);
    let client_uid = order_uid_hex(tick.chain_id, order, owner);
    if let Some(uid) = client_uid.as_deref()
        && journal.contains(uid)?
    {
        tracing::info!("{label} {uid} already submitted; skipping re-submit");
        return Ok(());
    }

    let Some(order_data) = gpv2_to_order_data(order) else {
        // An unknown enum marker means the SDK cannot express this
        // payload yet; skip rather than drop so an SDK upgrade can
        // still pick the watch up.
        tracing::warn!(
            "{label} submit skipped for {owner:#x}: GPv2OrderData carried an unknown enum marker"
        );
        return Ok(());
    };
    let creation = match build_order_creation(&order_data, signature, owner) {
        Ok(creation) => creation,
        Err(err) => {
            // A constructor rejection (zero `from`, `validTo` beyond
            // the client-side max horizon) is deterministic for this
            // polled payload: keeping the watch would re-poll and
            // re-warn on every block forever. Drop through the ledger
            // - the same net effect as the pre-keeper flow, where
            // the orderbook rejected the shipped body and the
            // classifier dropped the watch.
            tracing::warn!("{label} submit dropped watch for {owner:#x}: {err}");
            Retrier::new(host).apply(watch, RetryAction::Drop, tick.epoch_s)?;
            return Ok(());
        }
    };
    let body = match serde_json::to_vec(&creation) {
        Ok(body) => body,
        Err(e) => {
            tracing::error!("OrderCreation JSON encode failed: {e}");
            return Ok(());
        }
    };

    match host.submit_order(tick.chain_id, &body) {
        Ok(server_uid) => {
            // Prefer the client-computed UID so the guard above reads
            // what this writes; a divergence would be a protocol bug
            // worth a warning, never a silently split keyspace.
            let marker = client_uid.as_deref().unwrap_or(server_uid.as_str());
            // The submit already succeeded; a journal-store fault here
            // must not abort the sweep or unwind the accepted order.
            // Log and carry on - the already-submitted arm keeps the
            // next tick's re-post idempotent.
            if let Err(fault) = journal.record(marker) {
                tracing::error!("submitted {marker} but journal write failed: {fault}");
            }
            if let Some(client) = client_uid.as_deref()
                && client != server_uid
            {
                tracing::warn!(
                    "{label} UID divergence: client={client} server={server_uid} \
                     (marker keyed on the client UID)"
                );
            }
            tracing::info!("submitted {marker}");
        }
        Err(CowApiError::Rejected(rejection)) if is_already_submitted(&rejection) => {
            // Success wearing an error status: the orderbook already
            // holds this order. Record the receipt and keep the watch
            // so the next tick short-circuits instead of re-posting.
            // As above, a journal fault post-submit only forfeits the
            // short-circuit; it must not abort the sweep.
            if let Some(uid) = client_uid.as_deref()
                && let Err(fault) = journal.record(uid)
            {
                tracing::error!("orderbook already holds {uid} but journal write failed: {fault}");
            }
            tracing::info!(
                "orderbook already holds this order ({}); receipt recorded",
                rejection.error_type,
            );
        }
        Err(err) => {
            let action = classify_submit_error(&err);
            Retrier::new(host).apply(watch, action, tick.epoch_s)?;
            match action {
                RetryAction::TryNextBlock => tracing::warn!("submit retry-next-block: {err}"),
                RetryAction::Backoff { seconds } => {
                    tracing::warn!("submit backoff {seconds}s: {err}");
                }
                RetryAction::Drop => tracing::warn!("submit dropped watch: {err}"),
                // `RetryAction` is non-exhaustive; the ledger already
                // ran the effect, so the log needs only the name.
                other => {
                    let action_label: &'static str = other.into();
                    tracing::warn!("submit retry action {action_label}: {err}");
                }
            }
        }
    }
    Ok(())
}

/// Assemble the `OrderCreation` body the orderbook expects from a
/// polled conditional order. The signed `appData` digest goes out
/// verbatim in the hash-only wire shape (watch-tower parity), and the
/// signature is EIP-1271 - the conditional-order contract is the
/// verifier.
///
/// An `Err` is a client-side precondition failure that would recur on
/// every retry of the same payload; the caller drops the watch.
fn build_order_creation(
    order_data: &OrderData,
    signature: Bytes,
    from: Address,
) -> Result<OrderCreation, cowprotocol::Error> {
    let signature = Signature::Eip1271(signature.to_vec());
    OrderCreation::new_app_data_hash_only(order_data, signature, from, None)
}
