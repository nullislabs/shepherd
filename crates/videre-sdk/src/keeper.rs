//! The generic keeper run: [`Keeper::run`] assembles the world-neutral
//! `nexum_sdk::keeper` stores ([`WatchSet`], [`Gates`], [`Retrier`],
//! [`Journal`]) over a [`Poller`] and routes submissions through the
//! [`VenueTransport`] seam. [`Outcome`] is the shared [`Poller::Outcome`]
//! a keeper's pollers produce.

use nexum_sdk::host::{Fault, LocalStoreHost};
use nexum_sdk::keeper::{
    Gates, Journal, Mark, Poller, Reservation, Retrier, RetryAction, Tick, WatchRef, WatchSet,
};
use nexum_sdk::prelude::{hex, keccak256};

use crate::client::{VenueId, VenueTransport};
use crate::{SubmitOutcome, UnsignedTx, VenueFault};

/// What one poll asks the run to do with its watch.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Outcome {
    /// Submit these encoded intent-body bytes to the bound venue.
    Submit(Vec<u8>),
    /// Nothing to do yet; the next tick re-polls.
    WaitBlock,
    /// Gate the watch for `seconds` on the epoch clock.
    Backoff {
        /// Seconds to wait before the next poll.
        seconds: u64,
    },
    /// The commitment is spent or unservable; drop the watch.
    Drop,
}

/// Default `[limits.reconcile]` `max_per_tick`: stranded reservations
/// the top-of-sweep reconcile pass resolves per run.
pub const DEFAULT_RECONCILE_BUDGET: usize = 16;

/// A keeper: one poller bound to one venue, run over the
/// keeper stores.
pub struct Keeper<S, P> {
    source: S,
    venues: P,
    venue: VenueId,
    max_per_tick: usize,
}

impl<S, P> Keeper<S, P> {
    /// Bind a source to the venue id its submissions route to. The
    /// reconcile budget defaults to [`DEFAULT_RECONCILE_BUDGET`].
    pub fn new(source: S, venues: P, venue: impl Into<VenueId>) -> Self {
        Self {
            source,
            venues,
            venue: venue.into(),
            max_per_tick: DEFAULT_RECONCILE_BUDGET,
        }
    }

    /// Override the per-tick reconcile budget (`[limits.reconcile]`
    /// `max_per_tick`). The fresh-watch loop is never budget-bounded;
    /// only the reconcile pass is.
    #[must_use]
    pub fn with_reconcile_budget(mut self, max_per_tick: usize) -> Self {
        self.max_per_tick = max_per_tick;
        self
    }

    /// The venue every submission routes to.
    pub fn venue(&self) -> &VenueId {
        &self.venue
    }
}

impl<S, P: VenueTransport> Keeper<S, P> {
    /// Run one sweep at `tick`: the [`reconcile`] pass first (budget-bounded),
    /// then every gate-ready watch is polled and an [`Outcome::Submit`]
    /// body reserved on its [`submission_key`] before the venue await,
    /// committed on acceptance.
    ///
    /// Reconcile-not-release contract: `release` runs only on a known
    /// synchronous non-accept (`requires-signing` or a venue refusal); a
    /// crash, trap, `OutOfFuel`, or deadline-cancel mid-await leaves the
    /// `RESERVED` marker for the next tick's reconcile pass. A `RESERVED`
    /// marker at the submit arm is owned by this tick's reconcile and never
    /// re-POSTed; a `COMMITTED` marker is an idempotent skip. Store faults
    /// abort the run (bar the best-effort commit and marker clear); venue
    /// refusals fold into per-watch retry actions.
    pub async fn run<H>(&self, host: &H, tick: &Tick) -> Result<RunReport, Fault>
    where
        H: LocalStoreHost,
        S: Poller<H, Outcome = Outcome>,
    {
        let watches = WatchSet::new(host);
        let gates = Gates::new(host);
        let retrier = Retrier::new(host);
        let journal = Journal::submitted(host);
        let mut report = RunReport::default();

        let rec = reconcile(&self.venue, &self.venues, &journal, tick, self.max_per_tick).await?;
        report.reconciled_committed = rec.committed;
        report.reconciled_released = rec.released;
        report.reconciled_pending = rec.pending;
        report.reconciled_gated = rec.gated;
        report.reconciled_unsigned = rec.unsigned.len();
        report.unsigned.extend(rec.unsigned);

        for key in watches.list()? {
            let Some(watch) = WatchRef::parse(&key) else {
                report.skipped += 1;
                continue;
            };
            if !gates.is_ready(watch, tick.block, tick.epoch_s)? {
                report.gated += 1;
                continue;
            }
            let Some(params) = watches.get(watch)? else {
                report.skipped += 1;
                continue;
            };
            report.polled += 1;

            let action = match self.source.poll(host, watch, &params, tick) {
                Outcome::Submit(body) => {
                    let key = submission_key(&self.venue, &body);
                    match journal.mark(&key)? {
                        // Durable already: idempotent skip, no venue call.
                        Some(Mark::Committed) => {
                            report.duplicates += 1;
                            continue;
                        }
                        // This tick's reconcile pass owns the reservation;
                        // never a second POST here.
                        Some(Mark::Reserved) => {
                            report.retried += 1;
                            continue;
                        }
                        None => {
                            // Reserve the real body before the await: a
                            // crash, trap, or deadline-cancel now strands a
                            // RESERVED marker the next tick's reconcile
                            // pass resolves, never a silent drop.
                            journal.reserve(&key, &body)?;
                            match self.venues.submit(&self.venue, body).await {
                                Ok(SubmitOutcome::Accepted(_)) => {
                                    // Best-effort: a commit fault just
                                    // reconciles next tick.
                                    let _ = journal.commit(&key);
                                    if let Err(fault) = retrier.clear_refusal(watch) {
                                        tracing::error!(
                                            %fault,
                                            "refusal-marker clear failed after commit",
                                        );
                                    }
                                    report.submitted += 1;
                                    continue;
                                }
                                Ok(SubmitOutcome::RequiresSigning(tx)) => {
                                    journal.release(&key)?;
                                    report.unsigned.push(tx);
                                    continue;
                                }
                                // A known synchronous non-accept: release
                                // the reserve, then fold the refusal.
                                Err(fault) => {
                                    journal.release(&key)?;
                                    retry_action(&fault)
                                }
                            }
                        }
                    }
                }
                Outcome::WaitBlock => RetryAction::TryNextBlock,
                Outcome::Backoff { seconds } => RetryAction::Backoff { seconds },
                Outcome::Drop => RetryAction::Drop,
            };
            match action {
                RetryAction::Drop => report.dropped += 1,
                _ => report.retried += 1,
            }
            retrier.apply(watch, action, tick)?;
        }
        Ok(report)
    }
}

/// Top-of-sweep reconcile pass: resolve each stranded reservation from
/// [`Journal::pending`] against the venue, budget-bounded by
/// `max_per_tick`. A `RESERVED` marker is an unknown submit outcome and
/// is NEVER skipped.
///
/// - `next_eligible > tick.epoch_s`: still backing off, left untouched.
/// - accepted: committed.
/// - `requires-signing`: released, the tx surfaced to the caller.
/// - terminal refusal (a [`RetryAction::Drop`] fault): released.
/// - transient refusal: left `RESERVED` for the next tick; a rate-limit
///   hint re-parks `next_eligible`.
pub async fn reconcile<H, P>(
    venue: &VenueId,
    venues: &P,
    journal: &Journal<'_, H>,
    tick: &Tick,
    max_per_tick: usize,
) -> Result<ReconcileReport, Fault>
where
    H: LocalStoreHost,
    P: VenueTransport,
{
    let mut report = ReconcileReport::default();
    let mut spent = 0usize;
    for Reservation {
        key,
        next_eligible,
        body,
    } in journal.pending()?
    {
        if next_eligible > tick.epoch_s {
            report.gated += 1;
            continue;
        }
        if spent >= max_per_tick {
            break;
        }
        spent += 1;
        match venues.submit(venue, body.clone()).await {
            Ok(SubmitOutcome::Accepted(_)) => {
                journal.commit(&key)?;
                report.committed += 1;
            }
            Ok(SubmitOutcome::RequiresSigning(tx)) => {
                journal.release(&key)?;
                report.unsigned.push(tx);
            }
            Err(fault) if is_terminal(&fault) => {
                journal.release(&key)?;
                report.released += 1;
            }
            Err(VenueFault::RateLimited {
                retry_after_ms: Some(ms),
            }) => {
                journal.park(&key, &body, tick.epoch_s.saturating_add(ms.div_ceil(1000)))?;
                report.pending += 1;
            }
            // Transient (timeout, unavailable, throttle without a hint):
            // leave the marker for the next tick.
            Err(_) => report.pending += 1,
        }
    }
    Ok(report)
}

/// A venue refusal no resubmit can cure: its [`retry_action`] drops the
/// watch, so the reservation is safe to release.
fn is_terminal(fault: &VenueFault) -> bool {
    matches!(retry_action(fault), RetryAction::Drop)
}

/// One run's tally, by watch disposition.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct RunReport {
    /// Watches polled.
    pub polled: usize,
    /// Watches skipped by an unexpired gate.
    pub gated: usize,
    /// Watches skipped unread: a malformed key or a vanished row.
    pub skipped: usize,
    /// Bodies the venue accepted, submission key newly journalled.
    pub submitted: usize,
    /// Bodies an earlier run journalled, skipped without a venue call.
    pub duplicates: usize,
    /// Watches left in place for a later tick, plus submit arms the
    /// reconcile pass already owns this tick.
    pub retried: usize,
    /// Watches dropped.
    pub dropped: usize,
    /// Stranded reservations the reconcile pass committed this tick.
    pub reconciled_committed: usize,
    /// Reservations the reconcile pass released on a terminal refusal.
    pub reconciled_released: usize,
    /// Reservations the reconcile pass left `RESERVED` for a later tick.
    pub reconciled_pending: usize,
    /// Reservations still inside their backoff window this tick.
    pub reconciled_gated: usize,
    /// Reservations the reconcile pass answered `requires-signing`; the
    /// txs ride [`unsigned`](Self::unsigned).
    pub reconciled_unsigned: usize,
    /// Transactions the venue answered `requires-signing`; a run cannot
    /// sign, so the caller owns them. Fresh-watch and reconcile answers.
    pub unsigned: Vec<UnsignedTx>,
}

/// One reconcile pass's tally, by reservation disposition.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct ReconcileReport {
    /// Stranded reservations the venue re-accepted, now committed.
    pub committed: usize,
    /// Reservations released on a terminal venue refusal.
    pub released: usize,
    /// Reservations left `RESERVED` for a later tick on a transient
    /// fault.
    pub pending: usize,
    /// Reservations still inside their backoff window, untouched.
    pub gated: usize,
    /// Transactions the venue answered `requires-signing`; released and
    /// handed to the caller.
    pub unsigned: Vec<UnsignedTx>,
}

/// Deterministic pre-submit journal key: the venue id and the keccak-256
/// of the body as a fixed-length suffix, so the key is unambiguous
/// whatever the venue id contains.
pub fn submission_key(venue: &VenueId, body: &[u8]) -> String {
    format!("{venue}:{}", hex::encode_prefixed(keccak256(body)))
}

/// Fold a venue refusal into a retry action: a throttle hint becomes an
/// epoch gate, transient failures retry next block, and refusals no retry
/// can cure drop the watch.
pub fn retry_action(fault: &VenueFault) -> RetryAction {
    match fault {
        VenueFault::RateLimited {
            retry_after_ms: Some(ms),
        } => RetryAction::Backoff {
            seconds: ms.div_ceil(1000),
        },
        VenueFault::RateLimited {
            retry_after_ms: None,
        }
        | VenueFault::Timeout
        | VenueFault::Unavailable(_) => RetryAction::TryNextBlock,
        VenueFault::UnknownVenue
        | VenueFault::InvalidBody(_)
        | VenueFault::Unsupported
        | VenueFault::Denied(_)
        | VenueFault::InvalidReceipt
        | VenueFault::ReceiptMismatch => RetryAction::Drop,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::HashSet;

    use nexum_sdk::host::{Fault, LocalStoreHost as _};
    use nexum_sdk::keeper::{Gates, Journal, Mark, Tick, WatchRef, WatchSet};
    use nexum_sdk::prelude::{Address, B256, hex, keccak256};
    use nexum_sdk_test::{MockLocalStore, TrapStore};

    use super::{Keeper, Outcome, RunReport, submission_key};
    use crate::client::{VenueId, VenueTransport};
    use crate::{IntentStatus, Quotation, SubmitOutcome, UnsignedTx, VenueFault};

    /// Drive a run on the test's synchronous boundary.
    fn run<F: std::future::Future>(future: F) -> F::Output {
        match crate::client::poll_once(future) {
            std::task::Poll::Ready(output) => output,
            std::task::Poll::Pending => panic!("run futures complete in one poll"),
        }
    }

    /// Answers every poll with one programmed outcome.
    struct StubSource(Outcome);

    impl<H> nexum_sdk::keeper::Poller<H> for StubSource {
        type Outcome = Outcome;

        fn poll(&self, _host: &H, _watch: WatchRef<'_>, _params: &[u8], _tick: &Tick) -> Outcome {
            self.0.clone()
        }
    }

    /// Pops one programmed outcome per poll, from the back.
    struct SeqSource(RefCell<Vec<Outcome>>);

    impl<H> nexum_sdk::keeper::Poller<H> for SeqSource {
        type Outcome = Outcome;

        fn poll(&self, _host: &H, _watch: WatchRef<'_>, _params: &[u8], _tick: &Tick) -> Outcome {
            self.0.borrow_mut().pop().unwrap_or(Outcome::WaitBlock)
        }
    }

    /// Answers every submit with one programmed outcome, logging bodies.
    struct StubVenue {
        outcome: Result<SubmitOutcome, VenueFault>,
        submitted: RefCell<Vec<Vec<u8>>>,
    }

    impl StubVenue {
        fn new(outcome: Result<SubmitOutcome, VenueFault>) -> Self {
            Self {
                outcome,
                submitted: RefCell::new(Vec::new()),
            }
        }
    }

    impl crate::client::sealed::SealedTransport for &StubVenue {}

    impl VenueTransport for &StubVenue {
        async fn quote(&self, _venue: &VenueId, _body: Vec<u8>) -> Result<Quotation, VenueFault> {
            unreachable!("quote not exercised")
        }

        async fn submit(
            &self,
            _venue: &VenueId,
            body: Vec<u8>,
        ) -> Result<SubmitOutcome, VenueFault> {
            self.submitted.borrow_mut().push(body);
            self.outcome.clone()
        }

        async fn status(
            &self,
            _venue: &VenueId,
            _receipt: &[u8],
        ) -> Result<IntentStatus, VenueFault> {
            unreachable!("status not exercised")
        }

        async fn cancel(&self, _venue: &VenueId, _receipt: &[u8]) -> Result<(), VenueFault> {
            unreachable!("cancel not exercised")
        }
    }

    const TICK: Tick = Tick {
        chain_id: 1,
        block: 100,
        epoch_s: 1_000,
    };

    fn put_watch(host: &MockLocalStore) -> String {
        WatchSet::new(host)
            .put(&Address::ZERO, &B256::ZERO, b"params")
            .expect("mock store accepts the watch")
    }

    fn keeper(outcome: Outcome, venue: &StubVenue) -> Keeper<StubSource, &StubVenue> {
        Keeper::new(StubSource(outcome), venue, "stub")
    }

    #[test]
    fn accepted_body_is_journalled_and_never_resubmitted() {
        let host = MockLocalStore::default();
        put_watch(&host);
        let venue = StubVenue::new(Ok(SubmitOutcome::Accepted(vec![0xA5, 0x5A])));
        let keeper = keeper(Outcome::Submit(b"body".to_vec()), &venue);

        let report = run(keeper.run(&host, &TICK)).expect("keeper runs");
        assert_eq!(report.polled, 1);
        assert_eq!(report.submitted, 1);
        assert_eq!(venue.submitted.borrow().as_slice(), [b"body".to_vec()]);

        let journal = Journal::submitted(&host);
        let key = format!("stub:{}", hex::encode_prefixed(keccak256(b"body")));
        assert!(journal.contains(&key).expect("journal reads"));
        assert_eq!(WatchSet::new(&host).list().expect("list reads").len(), 1);

        // A later run re-polls the watch but never re-posts the body.
        let report = run(keeper.run(&host, &TICK)).expect("keeper runs");
        assert_eq!(report.submitted, 0);
        assert_eq!(report.duplicates, 1);
        assert_eq!(venue.submitted.borrow().len(), 1);
    }

    #[test]
    fn accepted_body_clears_the_refusal_marker() {
        let host = MockLocalStore::default();
        let key = put_watch(&host);
        let watch = WatchRef::parse(&key).expect("well-formed key");
        host.set(&watch.refused_key(), &50_u64.to_le_bytes())
            .expect("marker writes");
        let venue = StubVenue::new(Ok(SubmitOutcome::Accepted(vec![1])));

        let report = run(keeper(Outcome::Submit(b"body".to_vec()), &venue).run(&host, &TICK))
            .expect("keeper runs");
        assert_eq!(report.submitted, 1);
        assert!(
            host.get(&watch.refused_key())
                .expect("marker reads")
                .is_none(),
            "acceptance must clear the first-refusal marker",
        );
    }

    #[test]
    fn refusal_marker_clear_fault_does_not_abort_a_journalled_acceptance() {
        let host = MockLocalStore::default();
        put_watch(&host);
        host.fail_on("refused:", Fault::Unavailable("store down".into()));
        let venue = StubVenue::new(Ok(SubmitOutcome::Accepted(vec![1])));

        let report = run(keeper(Outcome::Submit(b"body".to_vec()), &venue).run(&host, &TICK))
            .expect("marker-clear fault must not abort the run");
        assert_eq!(report.submitted, 1);
        let key = format!("stub:{}", hex::encode_prefixed(keccak256(b"body")));
        assert!(
            Journal::submitted(&host)
                .contains(&key)
                .expect("journal reads"),
            "acceptance must be journalled before the marker clear",
        );
    }

    #[test]
    fn a_changed_body_submits_afresh() {
        let host = MockLocalStore::default();
        put_watch(&host);
        let venue = StubVenue::new(Ok(SubmitOutcome::Accepted(vec![1])));
        // Polls pop from the back: `one` first, then `two`.
        let source = SeqSource(RefCell::new(vec![
            Outcome::Submit(b"two".to_vec()),
            Outcome::Submit(b"one".to_vec()),
        ]));
        let keeper = Keeper::new(source, &venue, "stub");

        assert_eq!(
            run(keeper.run(&host, &TICK))
                .expect("keeper runs")
                .submitted,
            1
        );
        assert_eq!(
            run(keeper.run(&host, &TICK))
                .expect("keeper runs")
                .submitted,
            1
        );
        assert_eq!(
            venue.submitted.borrow().as_slice(),
            [b"one".to_vec(), b"two".to_vec()]
        );
    }

    #[test]
    fn requires_signing_hands_the_transaction_to_the_caller() {
        let host = MockLocalStore::default();
        put_watch(&host);
        let tx = UnsignedTx {
            chain: 1,
            to: vec![0x11; 20],
            value: Vec::new(),
            data: vec![0xFE],
        };
        let venue = StubVenue::new(Ok(SubmitOutcome::RequiresSigning(tx.clone())));
        let keeper = keeper(Outcome::Submit(b"body".to_vec()), &venue);

        let report = run(keeper.run(&host, &TICK)).expect("keeper runs");
        assert_eq!(report.unsigned, vec![tx.clone()]);
        assert_eq!(report.submitted, 0);

        // Nothing accepted, nothing journalled: the next run
        // surfaces the same transaction again.
        let report = run(keeper.run(&host, &TICK)).expect("keeper runs");
        assert_eq!(report.unsigned, vec![tx]);
    }

    #[test]
    fn gated_watch_is_not_polled() {
        let host = MockLocalStore::default();
        let key = put_watch(&host);
        let watch = WatchRef::parse(&key).expect("well-formed key");
        Gates::new(&host)
            .set_next_block(watch, TICK.block + 1)
            .expect("gate writes");
        let venue = StubVenue::new(Ok(SubmitOutcome::Accepted(vec![1])));

        let report = run(keeper(Outcome::Submit(b"body".to_vec()), &venue).run(&host, &TICK))
            .expect("keeper runs");
        assert_eq!(report.gated, 1);
        assert_eq!(report.polled, 0);
        assert!(venue.submitted.borrow().is_empty());
    }

    #[test]
    fn drop_outcome_removes_the_watch() {
        let host = MockLocalStore::default();
        put_watch(&host);
        let venue = StubVenue::new(Ok(SubmitOutcome::Accepted(vec![1])));

        let report = run(keeper(Outcome::Drop, &venue).run(&host, &TICK)).expect("keeper runs");
        assert_eq!(report.dropped, 1);
        assert!(WatchSet::new(&host).list().expect("list reads").is_empty());
    }

    #[test]
    fn backoff_outcome_gates_the_watch_on_the_epoch_clock() {
        let host = MockLocalStore::default();
        put_watch(&host);
        let venue = StubVenue::new(Ok(SubmitOutcome::Accepted(vec![1])));
        let keeper = keeper(Outcome::Backoff { seconds: 30 }, &venue);

        let report = run(keeper.run(&host, &TICK)).expect("keeper runs");
        assert_eq!(report.retried, 1);

        // Still inside the backoff window: gated, not polled.
        let report = run(keeper.run(&host, &TICK)).expect("keeper runs");
        assert_eq!(report.gated, 1);

        // At the threshold the gate opens again.
        let later = Tick {
            epoch_s: TICK.epoch_s + 30,
            ..TICK
        };
        let report = run(keeper.run(&host, &later)).expect("keeper runs");
        assert_eq!(report.polled, 1);
    }

    #[test]
    fn rate_limited_refusal_backs_off_by_the_venue_hint() {
        let host = MockLocalStore::default();
        put_watch(&host);
        let venue = StubVenue::new(Err(VenueFault::RateLimited {
            retry_after_ms: Some(2_500),
        }));
        let keeper = keeper(Outcome::Submit(b"body".to_vec()), &venue);

        let report = run(keeper.run(&host, &TICK)).expect("keeper runs");
        assert_eq!(report.retried, 1);

        // 2500 ms rounds up to a 3 s epoch gate.
        let at_2s = Tick {
            epoch_s: TICK.epoch_s + 2,
            ..TICK
        };
        assert_eq!(
            run(keeper.run(&host, &at_2s)).expect("keeper runs").gated,
            1
        );
        let at_3s = Tick {
            epoch_s: TICK.epoch_s + 3,
            ..TICK
        };
        assert_eq!(
            run(keeper.run(&host, &at_3s)).expect("keeper runs").polled,
            1
        );
    }

    #[test]
    fn non_retryable_refusal_drops_the_watch() {
        let host = MockLocalStore::default();
        put_watch(&host);
        let venue = StubVenue::new(Err(VenueFault::Denied("blocked".into())));

        let report = run(keeper(Outcome::Submit(b"body".to_vec()), &venue).run(&host, &TICK))
            .expect("keeper runs");
        assert_eq!(report.dropped, 1);
        assert!(WatchSet::new(&host).list().expect("list reads").is_empty());
    }

    #[test]
    fn transient_refusal_leaves_the_watch_for_the_next_tick() {
        let host = MockLocalStore::default();
        put_watch(&host);
        let venue = StubVenue::new(Err(VenueFault::Unavailable("down".into())));
        let keeper = keeper(Outcome::Submit(b"body".to_vec()), &venue);

        let report = run(keeper.run(&host, &TICK)).expect("keeper runs");
        assert_eq!(report.retried, 1);
        assert_eq!(
            run(keeper.run(&host, &TICK)).expect("keeper runs").polled,
            1
        );
    }

    #[test]
    fn empty_watch_set_reports_nothing() {
        let host = MockLocalStore::default();
        let venue = StubVenue::new(Ok(SubmitOutcome::Accepted(vec![1])));

        let report =
            run(keeper(Outcome::WaitBlock, &venue).run(&host, &TICK)).expect("keeper runs");
        assert_eq!(report, RunReport::default());
    }

    // ---- #573 reserve/commit + top-of-sweep reconcile ----

    const STUB: &str = "stub";

    /// The submit key the run reserves for `body` at the stub venue.
    fn stub_key(body: &[u8]) -> String {
        submission_key(&VenueId::from_static(STUB), body)
    }

    /// Seed a stranded `RESERVED` marker, as a prior tick's reserve
    /// whose submit outcome never landed.
    fn seed_reserved(host: &MockLocalStore, body: &[u8]) {
        Journal::submitted(host)
            .reserve(&stub_key(body), body)
            .expect("reserve writes");
    }

    /// Venue that counts POSTs and models re-POST idempotency: a held
    /// body re-accepts, an unheld one gets the programmed `outcome` and
    /// joins the held set on acceptance.
    struct CountingVenue {
        outcome: RefCell<Result<SubmitOutcome, VenueFault>>,
        posts: RefCell<Vec<Vec<u8>>>,
        held: RefCell<HashSet<Vec<u8>>>,
    }

    impl CountingVenue {
        fn new(outcome: Result<SubmitOutcome, VenueFault>) -> Self {
            Self {
                outcome: RefCell::new(outcome),
                posts: RefCell::new(Vec::new()),
                held: RefCell::new(HashSet::new()),
            }
        }

        fn accepting() -> Self {
            Self::new(Ok(SubmitOutcome::Accepted(vec![0xAB])))
        }

        fn post_count(&self) -> usize {
            self.posts.borrow().len()
        }

        fn held_count(&self) -> usize {
            self.held.borrow().len()
        }

        /// Pre-seed a held body: a POST received before the caller lost
        /// its outcome to a deadline-cancel.
        fn preload(&self, body: &[u8]) {
            self.held.borrow_mut().insert(body.to_vec());
        }
    }

    impl crate::client::sealed::SealedTransport for &CountingVenue {}

    impl VenueTransport for &CountingVenue {
        async fn quote(&self, _venue: &VenueId, _body: Vec<u8>) -> Result<Quotation, VenueFault> {
            unreachable!("quote not exercised")
        }

        async fn submit(
            &self,
            _venue: &VenueId,
            body: Vec<u8>,
        ) -> Result<SubmitOutcome, VenueFault> {
            self.posts.borrow_mut().push(body.clone());
            if self.held.borrow().contains(&body) {
                return Ok(SubmitOutcome::Accepted(vec![0xAB]));
            }
            let outcome = self.outcome.borrow().clone();
            if let Ok(SubmitOutcome::Accepted(_)) = &outcome {
                self.held.borrow_mut().insert(body);
            }
            outcome
        }

        async fn status(
            &self,
            _venue: &VenueId,
            _receipt: &[u8],
        ) -> Result<IntentStatus, VenueFault> {
            unreachable!("status not exercised")
        }

        async fn cancel(&self, _venue: &VenueId, _receipt: &[u8]) -> Result<(), VenueFault> {
            unreachable!("cancel not exercised")
        }
    }

    /// Wraps a store, faulting the first `COMMITTED` write to `submitted:`
    /// once: models an accepted submit whose commit write faults, leaving
    /// the `RESERVED` marker with no release.
    struct FlakyCommit {
        inner: MockLocalStore,
        arm: Cell<bool>,
    }

    impl FlakyCommit {
        fn new() -> Self {
            Self {
                inner: MockLocalStore::default(),
                arm: Cell::new(true),
            }
        }
    }

    impl nexum_sdk::host::LocalStoreHost for FlakyCommit {
        fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Fault> {
            self.inner.get(key)
        }

        fn set(&self, key: &str, value: &[u8]) -> Result<(), Fault> {
            // 0x02 is the journal COMMITTED tag.
            if self.arm.get() && key.starts_with("submitted:") && value.first() == Some(&0x02) {
                self.arm.set(false);
                return Err(Fault::Unavailable("commit write faulted".into()));
            }
            self.inner.set(key, value)
        }

        fn delete(&self, key: &str) -> Result<(), Fault> {
            self.inner.delete(key)
        }

        fn list_keys(&self, prefix: &str) -> Result<Vec<String>, Fault> {
            self.inner.list_keys(prefix)
        }

        fn contains(&self, key: &str) -> Result<bool, Fault> {
            self.inner.contains(key)
        }

        fn len(&self, key: &str) -> Result<Option<u64>, Fault> {
            nexum_sdk::host::LocalStoreHost::len(&self.inner, key)
        }

        fn count(&self, prefix: &str) -> Result<u64, Fault> {
            self.inner.count(prefix)
        }
    }

    /// A keeper whose source never submits: exercises the reconcile pass
    /// alone.
    fn idle_keeper(venue: &CountingVenue) -> Keeper<StubSource, &CountingVenue> {
        Keeper::new(StubSource(Outcome::WaitBlock), venue, STUB)
    }

    fn mark(host: &impl nexum_sdk::host::LocalStoreHost, body: &[u8]) -> Option<Mark> {
        Journal::submitted(host)
            .mark(&stub_key(body))
            .expect("mark reads")
    }

    #[test]
    fn reserve_commit_happy_path_then_idempotent_skip() {
        let host = MockLocalStore::default();
        put_watch(&host);
        let venue = CountingVenue::accepting();
        let keeper = Keeper::new(StubSource(Outcome::Submit(b"body".to_vec())), &venue, STUB);

        // Tick A: None -> reserve -> Accepted -> commit.
        let report = run(keeper.run(&host, &TICK)).expect("keeper runs");
        assert_eq!(report.submitted, 1);
        assert_eq!(venue.post_count(), 1);
        assert_eq!(mark(&host, b"body"), Some(Mark::Committed));

        // Tick B: the COMMITTED marker is an idempotent skip, zero calls.
        let report = run(keeper.run(&host, &TICK)).expect("keeper runs");
        assert_eq!(report.duplicates, 1);
        assert_eq!(report.submitted, 0);
        assert_eq!(venue.post_count(), 1);
    }

    #[test]
    fn w1_reserved_but_venue_never_saw_post_reconciles() {
        let host = MockLocalStore::default();
        seed_reserved(&host, b"order");
        let venue = CountingVenue::accepting();

        // Tick B: the reconcile pass resubmits the stranded reservation.
        let report = run(idle_keeper(&venue).run(&host, &TICK)).expect("keeper runs");
        assert_eq!(report.reconciled_committed, 1);
        assert_eq!(venue.post_count(), 1);
        assert_eq!(venue.held_count(), 1, "exactly one held order");
        assert_eq!(mark(&host, b"order"), Some(Mark::Committed));
    }

    #[test]
    fn w2_accepted_but_commit_faults_reconciles_without_double_holding() {
        let host = FlakyCommit::new();
        WatchSet::new(&host)
            .put(&Address::ZERO, &B256::ZERO, b"params")
            .expect("watch writes");
        let venue = CountingVenue::accepting();
        let keeper = Keeper::new(StubSource(Outcome::Submit(b"order".to_vec())), &venue, STUB);

        // Tick A: venue accepts (POST #1) but the commit write faults; the
        // RESERVED marker persists, no release runs.
        let report = run(keeper.run(&host, &TICK)).expect("keeper runs");
        assert_eq!(report.submitted, 1);
        assert_eq!(mark(&host, b"order"), Some(Mark::Reserved));

        // Tick B: reconcile resubmits (POST #2), the venue dedups, commit
        // lands - two POSTs but one held order.
        let report = run(keeper.run(&host, &TICK)).expect("keeper runs");
        assert_eq!(report.reconciled_committed, 1);
        assert_eq!(venue.post_count(), 2);
        assert_eq!(venue.held_count(), 1, "one held order despite two POSTs");
        assert_eq!(mark(&host, b"order"), Some(Mark::Committed));
    }

    #[test]
    fn w3_cancelled_during_submit_reconciles_both_ways() {
        // Sub-case (a): the venue DID receive it before the cancel.
        let host = MockLocalStore::default();
        seed_reserved(&host, b"order");
        let venue = CountingVenue::accepting();
        venue.preload(b"order");
        let report = run(idle_keeper(&venue).run(&host, &TICK)).expect("keeper runs");
        assert_eq!(report.reconciled_committed, 1);
        assert_eq!(venue.post_count(), 1);
        assert_eq!(venue.held_count(), 1);
        assert_eq!(mark(&host, b"order"), Some(Mark::Committed));

        // Sub-case (b), the venue did NOT receive it, is W1 above: a fresh
        // Accepted then commit, one held order.
    }

    #[test]
    fn reconcile_requires_signing_releases_and_never_re_enumerates() {
        let host = MockLocalStore::default();
        seed_reserved(&host, b"order");
        let tx = UnsignedTx {
            chain: 1,
            to: vec![0x11; 20],
            value: Vec::new(),
            data: vec![0xFE],
        };
        let venue = CountingVenue::new(Ok(SubmitOutcome::RequiresSigning(tx.clone())));

        let report = run(idle_keeper(&venue).run(&host, &TICK)).expect("keeper runs");
        assert_eq!(report.reconciled_unsigned, 1);
        assert_eq!(report.unsigned, vec![tx]);
        assert_eq!(mark(&host, b"order"), None);

        // The reservation is gone; later ticks do not re-enumerate it.
        for _ in 0..3 {
            let report = run(idle_keeper(&venue).run(&host, &TICK)).expect("keeper runs");
            assert_eq!(report.reconciled_unsigned, 0);
            assert!(Journal::submitted(&host).pending().unwrap().is_empty());
        }
        assert_eq!(venue.post_count(), 1);
    }

    #[test]
    fn reconcile_rate_limit_parks_then_reposts_after_the_window() {
        let host = MockLocalStore::default();
        seed_reserved(&host, b"order");
        let venue = CountingVenue::new(Err(VenueFault::RateLimited {
            retry_after_ms: Some(2_000),
        }));

        // T: parked with next_eligible = T + 2, one POST.
        let report = run(idle_keeper(&venue).run(&host, &TICK)).expect("keeper runs");
        assert_eq!(report.reconciled_pending, 1);
        assert_eq!(venue.post_count(), 1);

        // T + 1: inside the window, gated, no POST.
        let at_1 = Tick {
            epoch_s: TICK.epoch_s + 1,
            ..TICK
        };
        let report = run(idle_keeper(&venue).run(&host, &at_1)).expect("keeper runs");
        assert_eq!(report.reconciled_gated, 1);
        assert_eq!(venue.post_count(), 1);

        // T + 2: the window elapsed, exactly one more POST.
        let at_2 = Tick {
            epoch_s: TICK.epoch_s + 2,
            ..TICK
        };
        run(idle_keeper(&venue).run(&host, &at_2)).expect("keeper runs");
        assert_eq!(venue.post_count(), 2);
    }

    #[test]
    fn reconcile_terminal_refusal_releases_and_stays_gone() {
        let host = MockLocalStore::default();
        seed_reserved(&host, b"order");
        let venue = CountingVenue::new(Err(VenueFault::Denied("blocked".into())));

        let report = run(idle_keeper(&venue).run(&host, &TICK)).expect("keeper runs");
        assert_eq!(report.reconciled_released, 1);
        assert_eq!(mark(&host, b"order"), None);
        assert_eq!(venue.post_count(), 1);

        // No later reconcile resurrects it.
        let report = run(idle_keeper(&venue).run(&host, &TICK)).expect("keeper runs");
        assert_eq!(report.reconciled_released, 0);
        assert_eq!(venue.post_count(), 1);
    }

    #[test]
    fn reconcile_transient_refusal_keeps_the_marker_reserved() {
        let host = MockLocalStore::default();
        seed_reserved(&host, b"order");
        let venue = CountingVenue::new(Err(VenueFault::Unavailable("down".into())));

        let report = run(idle_keeper(&venue).run(&host, &TICK)).expect("keeper runs");
        assert_eq!(report.reconciled_pending, 1);
        assert_eq!(mark(&host, b"order"), Some(Mark::Reserved));
        assert_eq!(venue.post_count(), 1);

        // Still RESERVED next tick, reconciled again.
        let report = run(idle_keeper(&venue).run(&host, &TICK)).expect("keeper runs");
        assert_eq!(report.reconciled_pending, 1);
        assert_eq!(venue.post_count(), 2);
    }

    #[test]
    fn reconcile_budget_bounds_the_pass_but_not_the_fresh_watch() {
        let host = MockLocalStore::default();
        for body in [b"o1".as_slice(), b"o2", b"o3"] {
            seed_reserved(&host, body);
        }
        put_watch(&host);
        let venue = CountingVenue::accepting();
        let keeper = Keeper::new(StubSource(Outcome::Submit(b"fresh".to_vec())), &venue, STUB)
            .with_reconcile_budget(2);

        // At most two orphans reconciled, yet the fresh watch still
        // submits its own order this tick.
        let report = run(keeper.run(&host, &TICK)).expect("keeper runs");
        assert_eq!(report.reconciled_committed, 2);
        assert_eq!(report.submitted, 1);
        assert_eq!(Journal::submitted(&host).pending().unwrap().len(), 1);

        // The remaining orphan reconciles on a later tick.
        let report = run(keeper.run(&host, &TICK)).expect("keeper runs");
        assert_eq!(report.reconciled_committed, 1);
        assert!(Journal::submitted(&host).pending().unwrap().is_empty());
    }

    #[test]
    fn anti_572_reserved_marker_drives_a_reconcile_post_not_a_duplicate_skip() {
        let host = MockLocalStore::default();
        seed_reserved(&host, b"order");
        let venue = CountingVenue::accepting();

        // The #572 design would skip a RESERVED marker as a duplicate and
        // drop it forever; the reconcile pass MUST resubmit.
        let report = run(idle_keeper(&venue).run(&host, &TICK)).expect("keeper runs");
        assert_eq!(report.duplicates, 0, "a RESERVED marker is not a duplicate");
        assert!(venue.post_count() >= 1, "the reconcile pass must POST");
        assert_eq!(report.reconciled_committed, 1);
        assert_eq!(mark(&host, b"order"), Some(Mark::Committed));
    }

    #[test]
    fn anti_572_crash_between_submit_and_commit_leaves_reserved_not_none() {
        let host = FlakyCommit::new();
        WatchSet::new(&host)
            .put(&Address::ZERO, &B256::ZERO, b"params")
            .expect("watch writes");
        let venue = CountingVenue::accepting();
        let keeper = Keeper::new(StubSource(Outcome::Submit(b"order".to_vec())), &venue, STUB);

        // The faulted commit is a proxy for a crash between submit and
        // commit: the marker MUST be RESERVED, never released to None.
        run(keeper.run(&host, &TICK)).expect("keeper runs");
        assert_eq!(mark(&host, b"order"), Some(Mark::Reserved));
        assert_ne!(mark(&host, b"order"), None);
    }

    // ---- #609 trap-injection: convergence from every torn prefix ----
    //
    // Review rule the sweep enforces: no in-store invariant may span
    // two `set` calls unless the intermediate state is self-healing or
    // the writes ride the atomic `apply` batch verb (#609). The
    // reserve/commit journal passes because each of its intermediate
    // states - nothing, RESERVED, COMMITTED-sans-marker-clear - is one
    // the next sweep's reconcile pass resolves on its own.

    /// Seed one watch on a trap store and pair it with an accepting
    /// venue and a submitting keeper.
    fn trap_rig(
        venue: &CountingVenue,
    ) -> (
        TrapStore<MockLocalStore>,
        Keeper<StubSource, &CountingVenue>,
    ) {
        let host = TrapStore::new(MockLocalStore::default());
        WatchSet::new(&host)
            .put(&Address::ZERO, &B256::ZERO, b"params")
            .expect("watch writes");
        let keeper = Keeper::new(StubSource(Outcome::Submit(b"order".to_vec())), venue, STUB);
        (host, keeper)
    }

    /// Writes one accepted submit tick performs, pinned by a dry run:
    /// the reserve set, the commit set, and the refusal-marker delete.
    fn accepted_tick_writes() -> u64 {
        let venue = CountingVenue::accepting();
        let (host, keeper) = trap_rig(&venue);
        let seeded = host.writes();
        run(keeper.run(&host, &TICK)).expect("dry run completes");
        assert_eq!(mark(&host, b"order"), Some(Mark::Committed));
        host.writes() - seeded
    }

    #[test]
    fn trap_at_every_write_prefix_reconciles_to_exactly_one_held_order() {
        let total = accepted_tick_writes();
        assert_eq!(total, 3, "reserve set, commit set, refusal-marker delete");

        // Trap the tick after each n of its writes: every torn prefix
        // from nothing-landed through all-but-the-last.
        for n in 0..total {
            let venue = CountingVenue::accepting();
            let (host, keeper) = trap_rig(&venue);
            host.arm_after(n);
            let _ = run(keeper.run(&host, &TICK));
            assert!(host.tripped(), "prefix {n}: the trap must fire mid-tick");

            // Restart from the torn store: the next sweep's reconcile
            // pass plus the fresh-watch loop must converge.
            host.disarm();
            run(keeper.run(&host, &TICK)).expect("recovery tick runs");
            assert_eq!(
                mark(&host, b"order"),
                Some(Mark::Committed),
                "prefix {n}: the journal must end COMMITTED",
            );
            assert_eq!(
                venue.held_count(),
                1,
                "prefix {n}: exactly one held order, whatever the POST count",
            );
            assert!(
                Journal::submitted(&host)
                    .pending()
                    .expect("journal reads")
                    .is_empty(),
                "prefix {n}: no reservation may stay stranded",
            );

            // Steady state: a further tick is a pure idempotent skip.
            let posts = venue.post_count();
            let report = run(keeper.run(&host, &TICK)).expect("steady tick runs");
            assert_eq!(report.duplicates, 1, "prefix {n}: COMMITTED skips");
            assert_eq!(venue.post_count(), posts, "prefix {n}: no further POST");
            assert_eq!(venue.held_count(), 1, "prefix {n}: still one held order");
        }
    }
}
