//! The generic keeper sweep: one pass assembling the world-neutral
//! stores - [`WatchSet`] to [`Gates`] to [`ConditionalSource::poll`] to
//! [`Retrier`] to [`Journal`] - and routing submissions through the
//! [`VenueTransport`] seam.
//!
//! [`Sweep`] is the shared poll outcome: the concrete
//! [`ConditionalSource::Outcome`] a keeper's sources produce so
//! [`Keeper::sweep`] can act on every one of them. The world-neutral
//! primitives stay in `nexum_sdk::keeper`; this module only assembles
//! them.

use nexum_sdk::host::{Fault, LocalStoreHost};
use nexum_sdk::keeper::{
    ConditionalSource, Gates, Journal, Retrier, RetryAction, Tick, WatchRef, WatchSet,
};
use nexum_sdk::prelude::{hex, keccak256};

use crate::client::{VenueId, VenueTransport};
use crate::{SubmitOutcome, UnsignedTx, VenueFault};

/// What one poll asks the sweep to do with its watch.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Sweep {
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

/// A keeper: one conditional source bound to one venue, swept over the
/// keeper stores.
pub struct Keeper<S, P> {
    source: S,
    venues: P,
    venue: VenueId,
}

impl<S, P> Keeper<S, P> {
    /// Bind a source to the venue id its submissions route to.
    pub fn new(source: S, venues: P, venue: impl Into<VenueId>) -> Self {
        Self {
            source,
            venues,
            venue: venue.into(),
        }
    }

    /// The venue every submission routes to.
    pub fn venue(&self) -> &VenueId {
        &self.venue
    }
}

impl<S, P: VenueTransport> Keeper<S, P> {
    /// Sweep the watch set once at `tick`: poll every ready watch,
    /// submit [`Sweep::Submit`] bodies through the venue seam, and
    /// run every other outcome and every venue refusal through the
    /// [`Retrier`]. A venue-and-body key is checked against the
    /// `submitted:` [`Journal`] before every submit and recorded on
    /// acceptance, so an accepted body never reaches the venue twice;
    /// a `requires-signing` answer journals nothing and is surfaced
    /// afresh each sweep. Store faults abort the sweep; venue refusals
    /// never do - they fold into per-watch retry actions.
    pub async fn sweep<H>(&self, host: &H, tick: &Tick) -> Result<SweepReport, Fault>
    where
        H: LocalStoreHost,
        S: ConditionalSource<H, Outcome = Sweep>,
    {
        let watches = WatchSet::new(host);
        let gates = Gates::new(host);
        let retrier = Retrier::new(host);
        let journal = Journal::submitted(host);
        let mut report = SweepReport::default();

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
                Sweep::Submit(body) => {
                    let key = submission_key(&self.venue, &body);
                    if journal.contains(&key)? {
                        report.duplicates += 1;
                        continue;
                    }
                    match self.venues.submit(&self.venue, body).await {
                        Ok(SubmitOutcome::Accepted(_)) => {
                            journal.record(&key)?;
                            report.submitted += 1;
                            continue;
                        }
                        Ok(SubmitOutcome::RequiresSigning(tx)) => {
                            report.unsigned.push(tx);
                            continue;
                        }
                        Err(fault) => retry_action(fault),
                    }
                }
                Sweep::WaitBlock => RetryAction::TryNextBlock,
                Sweep::Backoff { seconds } => RetryAction::Backoff { seconds },
                Sweep::Drop => RetryAction::Drop,
            };
            match action {
                RetryAction::Drop => report.dropped += 1,
                _ => report.retried += 1,
            }
            retrier.apply(watch, action, tick.epoch_s)?;
        }
        Ok(report)
    }
}

/// One sweep's tally, by watch disposition.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct SweepReport {
    /// Watches polled.
    pub polled: usize,
    /// Watches skipped by an unexpired gate.
    pub gated: usize,
    /// Watches skipped unread: a malformed key, or a row that vanished
    /// mid-sweep.
    pub skipped: usize,
    /// Bodies the venue accepted, submission key newly journalled.
    pub submitted: usize,
    /// Bodies whose key an earlier sweep had journalled, skipped
    /// without a venue call.
    pub duplicates: usize,
    /// Watches left in place for a later tick.
    pub retried: usize,
    /// Watches dropped.
    pub dropped: usize,
    /// Transactions the venue answered `requires-signing`; a sweep
    /// cannot sign, so the caller owns them.
    pub unsigned: Vec<UnsignedTx>,
}

/// Deterministic pre-submit journal key: the venue id and the
/// keccak-256 of the body. The hash is a fixed-length suffix, so the
/// key is unambiguous whatever the venue id contains.
fn submission_key(venue: &VenueId, body: &[u8]) -> String {
    format!("{venue}:{}", hex::encode_prefixed(keccak256(body)))
}

/// Fold a venue refusal into the retry action the ledger runs: the
/// throttle hint becomes an epoch gate, transient failures retry next
/// block, and refusals no retry can cure drop the watch.
fn retry_action(fault: VenueFault) -> RetryAction {
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
        | VenueFault::Denied(_) => RetryAction::Drop,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use nexum_sdk::keeper::{Gates, Journal, Tick, WatchRef, WatchSet};
    use nexum_sdk::prelude::{Address, B256, hex, keccak256};
    use nexum_sdk_test::MockLocalStore;

    use super::{Keeper, Sweep, SweepReport};
    use crate::client::{VenueId, VenueTransport};
    use crate::{IntentStatus, Quotation, SubmitOutcome, UnsignedTx, VenueFault};

    /// Drive a sweep on the test's synchronous boundary.
    fn run<F: std::future::Future>(future: F) -> F::Output {
        crate::rt::complete(future).expect("sweep futures complete in one poll")
    }

    /// Answers every poll with one programmed outcome.
    struct StubSource(Sweep);

    impl<H> nexum_sdk::keeper::ConditionalSource<H> for StubSource {
        type Outcome = Sweep;

        fn poll(&self, _host: &H, _watch: WatchRef<'_>, _params: &[u8], _tick: &Tick) -> Sweep {
            self.0.clone()
        }
    }

    /// Pops one programmed outcome per poll, from the back.
    struct SeqSource(RefCell<Vec<Sweep>>);

    impl<H> nexum_sdk::keeper::ConditionalSource<H> for SeqSource {
        type Outcome = Sweep;

        fn poll(&self, _host: &H, _watch: WatchRef<'_>, _params: &[u8], _tick: &Tick) -> Sweep {
            self.0.borrow_mut().pop().unwrap_or(Sweep::WaitBlock)
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

    fn keeper(outcome: Sweep, venue: &StubVenue) -> Keeper<StubSource, &StubVenue> {
        Keeper::new(StubSource(outcome), venue, "stub")
    }

    #[test]
    fn accepted_body_is_journalled_and_never_resubmitted() {
        let host = MockLocalStore::default();
        put_watch(&host);
        let venue = StubVenue::new(Ok(SubmitOutcome::Accepted(vec![0xA5, 0x5A])));
        let keeper = keeper(Sweep::Submit(b"body".to_vec()), &venue);

        let report = run(keeper.sweep(&host, &TICK)).expect("sweep runs");
        assert_eq!(report.polled, 1);
        assert_eq!(report.submitted, 1);
        assert_eq!(venue.submitted.borrow().as_slice(), [b"body".to_vec()]);

        let journal = Journal::submitted(&host);
        let key = format!("stub:{}", hex::encode_prefixed(keccak256(b"body")));
        assert!(journal.contains(&key).expect("journal reads"));
        assert_eq!(WatchSet::new(&host).list().expect("list reads").len(), 1);

        // A later sweep re-polls the watch but never re-posts the body.
        let report = run(keeper.sweep(&host, &TICK)).expect("sweep runs");
        assert_eq!(report.submitted, 0);
        assert_eq!(report.duplicates, 1);
        assert_eq!(venue.submitted.borrow().len(), 1);
    }

    #[test]
    fn a_changed_body_submits_afresh() {
        let host = MockLocalStore::default();
        put_watch(&host);
        let venue = StubVenue::new(Ok(SubmitOutcome::Accepted(vec![1])));
        // Polls pop from the back: `one` first, then `two`.
        let source = SeqSource(RefCell::new(vec![
            Sweep::Submit(b"two".to_vec()),
            Sweep::Submit(b"one".to_vec()),
        ]));
        let keeper = Keeper::new(source, &venue, "stub");

        assert_eq!(
            run(keeper.sweep(&host, &TICK))
                .expect("sweep runs")
                .submitted,
            1
        );
        assert_eq!(
            run(keeper.sweep(&host, &TICK))
                .expect("sweep runs")
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
        let keeper = keeper(Sweep::Submit(b"body".to_vec()), &venue);

        let report = run(keeper.sweep(&host, &TICK)).expect("sweep runs");
        assert_eq!(report.unsigned, vec![tx.clone()]);
        assert_eq!(report.submitted, 0);

        // Nothing accepted, nothing journalled: the next sweep
        // surfaces the same transaction again.
        let report = run(keeper.sweep(&host, &TICK)).expect("sweep runs");
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

        let report = run(keeper(Sweep::Submit(b"body".to_vec()), &venue).sweep(&host, &TICK))
            .expect("sweep runs");
        assert_eq!(report.gated, 1);
        assert_eq!(report.polled, 0);
        assert!(venue.submitted.borrow().is_empty());
    }

    #[test]
    fn drop_outcome_removes_the_watch() {
        let host = MockLocalStore::default();
        put_watch(&host);
        let venue = StubVenue::new(Ok(SubmitOutcome::Accepted(vec![1])));

        let report = run(keeper(Sweep::Drop, &venue).sweep(&host, &TICK)).expect("sweep runs");
        assert_eq!(report.dropped, 1);
        assert!(WatchSet::new(&host).list().expect("list reads").is_empty());
    }

    #[test]
    fn backoff_outcome_gates_the_watch_on_the_epoch_clock() {
        let host = MockLocalStore::default();
        put_watch(&host);
        let venue = StubVenue::new(Ok(SubmitOutcome::Accepted(vec![1])));
        let keeper = keeper(Sweep::Backoff { seconds: 30 }, &venue);

        let report = run(keeper.sweep(&host, &TICK)).expect("sweep runs");
        assert_eq!(report.retried, 1);

        // Still inside the backoff window: gated, not polled.
        let report = run(keeper.sweep(&host, &TICK)).expect("sweep runs");
        assert_eq!(report.gated, 1);

        // At the threshold the gate opens again.
        let later = Tick {
            epoch_s: TICK.epoch_s + 30,
            ..TICK
        };
        let report = run(keeper.sweep(&host, &later)).expect("sweep runs");
        assert_eq!(report.polled, 1);
    }

    #[test]
    fn rate_limited_refusal_backs_off_by_the_venue_hint() {
        let host = MockLocalStore::default();
        put_watch(&host);
        let venue = StubVenue::new(Err(VenueFault::RateLimited {
            retry_after_ms: Some(2_500),
        }));
        let keeper = keeper(Sweep::Submit(b"body".to_vec()), &venue);

        let report = run(keeper.sweep(&host, &TICK)).expect("sweep runs");
        assert_eq!(report.retried, 1);

        // 2500 ms rounds up to a 3 s epoch gate.
        let at_2s = Tick {
            epoch_s: TICK.epoch_s + 2,
            ..TICK
        };
        assert_eq!(
            run(keeper.sweep(&host, &at_2s)).expect("sweep runs").gated,
            1
        );
        let at_3s = Tick {
            epoch_s: TICK.epoch_s + 3,
            ..TICK
        };
        assert_eq!(
            run(keeper.sweep(&host, &at_3s)).expect("sweep runs").polled,
            1
        );
    }

    #[test]
    fn non_retryable_refusal_drops_the_watch() {
        let host = MockLocalStore::default();
        put_watch(&host);
        let venue = StubVenue::new(Err(VenueFault::Denied("blocked".into())));

        let report = run(keeper(Sweep::Submit(b"body".to_vec()), &venue).sweep(&host, &TICK))
            .expect("sweep runs");
        assert_eq!(report.dropped, 1);
        assert!(WatchSet::new(&host).list().expect("list reads").is_empty());
    }

    #[test]
    fn transient_refusal_leaves_the_watch_for_the_next_tick() {
        let host = MockLocalStore::default();
        put_watch(&host);
        let venue = StubVenue::new(Err(VenueFault::Unavailable("down".into())));
        let keeper = keeper(Sweep::Submit(b"body".to_vec()), &venue);

        let report = run(keeper.sweep(&host, &TICK)).expect("sweep runs");
        assert_eq!(report.retried, 1);
        assert_eq!(
            run(keeper.sweep(&host, &TICK)).expect("sweep runs").polled,
            1
        );
    }

    #[test]
    fn empty_watch_set_reports_nothing() {
        let host = MockLocalStore::default();
        let venue = StubVenue::new(Ok(SubmitOutcome::Accepted(vec![1])));

        let report = run(keeper(Sweep::WaitBlock, &venue).sweep(&host, &TICK)).expect("sweep runs");
        assert_eq!(report, SweepReport::default());
    }
}
