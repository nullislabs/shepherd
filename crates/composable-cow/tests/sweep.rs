//! Sweep acceptance tests: `run` over the generic store mocks with a
//! scripted venue transport on the `videre:venue/client` seam.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

use alloy_primitives::{Address, B256, U256, address, hex, keccak256};
use composable_cow::{Verdict, run};
use cow_venue::assembly::{gpv2_to_order_data, order_data_to_body};
use cow_venue::{CowClient, CowIntent, CowIntentBody, CowVenue, SignedOrder};
use cowprotocol::{BuyTokenDestination, GPv2OrderData, OrderKind, SellTokenSource};
use nexum_sdk::host::LocalStoreHost as _;
use nexum_sdk::keeper::{ConditionalSource, Gates, Journal, Tick, WatchRef, WatchSet};
use nexum_sdk_test::{MockHost, capture_tracing};
use videre_sdk::client::sealed::SealedTransport;
use videre_sdk::keeper::submission_key;
use videre_sdk::{
    IntentBody as _, IntentStatus, Quotation, SubmitOutcome, UnsignedTx, Venue as _, VenueFault,
    VenueId, VenueTransport,
};

const SEPOLIA: u64 = 11_155_111;

/// Scripted venue transport: one submit outcome per queued entry,
/// every submit recorded. Quote, status, and cancel are off the sweep
/// path.
#[derive(Default)]
struct MockVenue {
    outcomes: RefCell<VecDeque<Result<SubmitOutcome, VenueFault>>>,
    submits: RefCell<Vec<(String, Vec<u8>)>>,
}

impl MockVenue {
    fn enqueue_submit(&self, outcome: Result<SubmitOutcome, VenueFault>) {
        self.outcomes.borrow_mut().push_back(outcome);
    }

    fn submits(&self) -> Vec<(String, Vec<u8>)> {
        self.submits.borrow().clone()
    }

    fn submit_count(&self) -> usize {
        self.submits.borrow().len()
    }
}

impl SealedTransport for &MockVenue {}

impl VenueTransport for &MockVenue {
    async fn quote(&self, _venue: &VenueId, _body: Vec<u8>) -> Result<Quotation, VenueFault> {
        unreachable!("quote not exercised")
    }

    async fn submit(&self, venue: &VenueId, body: Vec<u8>) -> Result<SubmitOutcome, VenueFault> {
        self.submits.borrow_mut().push((venue.to_string(), body));
        self.outcomes.borrow_mut().pop_front().unwrap_or_else(|| {
            Err(VenueFault::Unavailable(
                "MockVenue: unscripted submit".into(),
            ))
        })
    }

    async fn status(&self, _venue: &VenueId, _receipt: &[u8]) -> Result<IntentStatus, VenueFault> {
        unreachable!("status not exercised")
    }

    async fn cancel(&self, _venue: &VenueId, _receipt: &[u8]) -> Result<(), VenueFault> {
        unreachable!("cancel not exercised")
    }
}

fn client(venue: &MockVenue) -> CowClient<&MockVenue> {
    CowClient::with_transport(venue)
}

/// Closure-backed source so each test scripts its own outcome and
/// observes its own poll calls.
struct FnSource<F>(F);

impl<H, F> ConditionalSource<H> for FnSource<F>
where
    F: Fn(&H, WatchRef<'_>, &[u8], &Tick) -> Verdict,
{
    type Outcome = Verdict;

    fn poll(&self, host: &H, watch: WatchRef<'_>, params: &[u8], tick: &Tick) -> Verdict {
        (self.0)(host, watch, params, tick)
    }
}

/// Pin the closure to the higher-ranked source signature at the
/// construction site so inference never guesses a too-narrow lifetime.
fn src<F>(f: F) -> FnSource<F>
where
    F: Fn(&MockHost, WatchRef<'_>, &[u8], &Tick) -> Verdict,
{
    FnSource(f)
}

fn sample_owner() -> Address {
    address!("00112233445566778899aabbccddeeff00112233")
}

fn sample_hash() -> B256 {
    keccak256(b"conditional order params")
}

fn sample_tick() -> Tick {
    Tick {
        chain_id: SEPOLIA,
        block: 1_000,
        epoch_s: 1_700_000_000,
    }
}

fn submittable_order() -> GPv2OrderData {
    GPv2OrderData {
        sellToken: address!("6810e776880C02933D47DB1b9fc05908e5386b96"),
        buyToken: address!("DAE5F1590db13E3B40423B5b5c5fbf175515910b"),
        receiver: Address::ZERO,
        sellAmount: U256::from(1_000_000_u64),
        buyAmount: U256::from(999_u64),
        validTo: u32::MAX,
        appData: cowprotocol::EMPTY_APP_DATA_HASH,
        feeAmount: U256::ZERO,
        kind: OrderKind::SELL,
        partiallyFillable: false,
        sellTokenBalance: SellTokenSource::ERC20,
        buyTokenBalance: BuyTokenDestination::ERC20,
    }
}

fn ready_outcome(order: &GPv2OrderData) -> Verdict {
    Verdict::Post {
        order: Box::new(order.clone()),
        signature: hex!("c0ffeec0ffeec0ffee").to_vec().into(),
        next_poll_timestamp: 0,
    }
}

fn seed_watch(host: &MockHost) -> String {
    WatchSet::new(host)
        .put(&sample_owner(), &sample_hash(), b"params")
        .unwrap()
}

/// The encoded intent body the sweep submits for `order`.
fn intent_bytes(order: &GPv2OrderData) -> Vec<u8> {
    let order_data = gpv2_to_order_data(order).expect("known markers");
    CowIntentBody::V1(CowIntent::Signed(SignedOrder {
        order: order_data_to_body(&order_data),
        owner: sample_owner().into_array(),
        signature: hex!("c0ffeec0ffeec0ffee").to_vec(),
    }))
    .to_bytes()
    .expect("body encodes")
}

/// The intent-id the sweep journals for `order`: the venue-and-body
/// key over the same signed body `run` derives pre-submit.
fn intent_id(order: &GPv2OrderData) -> String {
    submission_key(&CowVenue::ID, &intent_bytes(order))
}

fn accepted() -> Result<SubmitOutcome, VenueFault> {
    Ok(SubmitOutcome::Accepted(vec![0xAA]))
}

// ---- lifecycle outcomes ----

#[test]
fn try_next_block_leaves_the_store_untouched() {
    let host = MockHost::new();
    seed_watch(&host);
    let before = host.store.snapshot();
    let venue = MockVenue::default();

    run(
        &host,
        &client(&venue),
        &src(|_, _, _, _| Verdict::TryNextBlock { reason: [0; 4] }),
        &sample_tick(),
    )
    .unwrap();

    assert_eq!(host.store.snapshot(), before);
    assert_eq!(venue.submit_count(), 0);
}

#[test]
fn wait_block_sets_the_block_gate() {
    let host = MockHost::new();
    let key = seed_watch(&host);
    let watch = WatchRef::parse(&key).unwrap();
    let venue = MockVenue::default();

    run(
        &host,
        &client(&venue),
        &src(|_, _, _, _| Verdict::WaitBlock {
            wait_until: 2_000,
            reason: [0; 4],
        }),
        &sample_tick(),
    )
    .unwrap();

    assert_eq!(
        host.store.snapshot().get(&watch.next_block_key()).unwrap(),
        &2_000_u64.to_le_bytes().to_vec(),
    );
}

#[test]
fn wait_timestamp_sets_the_epoch_gate() {
    let host = MockHost::new();
    let key = seed_watch(&host);
    let watch = WatchRef::parse(&key).unwrap();
    let venue = MockVenue::default();

    run(
        &host,
        &client(&venue),
        &src(|_, _, _, _| Verdict::WaitTimestamp {
            wait_until: 1_800_000_000,
            reason: [0; 4],
        }),
        &sample_tick(),
    )
    .unwrap();

    assert_eq!(
        host.store.snapshot().get(&watch.next_epoch_key()).unwrap(),
        &1_800_000_000_u64.to_le_bytes().to_vec(),
    );
}

#[test]
fn invalid_removes_the_watch_and_its_gates() {
    let host = MockHost::new();
    let key = seed_watch(&host);
    let watch = WatchRef::parse(&key).unwrap();
    Gates::new(&host).set_next_block(watch, 1).unwrap();
    let venue = MockVenue::default();

    run(
        &host,
        &client(&venue),
        &src(|_, _, _, _| Verdict::Invalid { reason: [0; 4] }),
        &sample_tick(),
    )
    .unwrap();

    assert!(host.store.is_empty(), "watch and gates must go");
}

// ---- gating and skipping ----

#[test]
fn gated_watch_is_not_polled() {
    let host = MockHost::new();
    let key = seed_watch(&host);
    Gates::new(&host)
        .set_next_block(WatchRef::parse(&key).unwrap(), 5_000)
        .unwrap();
    let polls = Cell::new(0_u32);
    let venue = MockVenue::default();

    run(
        &host,
        &client(&venue),
        &src(|_, _, _, _| {
            polls.set(polls.get() + 1);
            Verdict::TryNextBlock { reason: [0; 4] }
        }),
        &sample_tick(),
    )
    .unwrap();

    assert_eq!(polls.get(), 0, "a gated watch must not reach the source");
}

#[test]
fn malformed_watch_rows_are_skipped() {
    let host = MockHost::new();
    host.store.set("watch:no-separator", b"junk").unwrap();
    let polls = Cell::new(0_u32);
    let venue = MockVenue::default();

    run(
        &host,
        &client(&venue),
        &src(|_, _, _, _| {
            polls.set(polls.get() + 1);
            Verdict::TryNextBlock { reason: [0; 4] }
        }),
        &sample_tick(),
    )
    .unwrap();

    assert_eq!(polls.get(), 0);
}

// ---- ready -> submission ----

#[test]
fn ready_submits_once_and_journals_the_intent_id() {
    let host = MockHost::new();
    seed_watch(&host);
    let order = submittable_order();
    let venue = MockVenue::default();
    venue.enqueue_submit(accepted());

    let source = {
        let order = order.clone();
        src(move |_, _, _, _| ready_outcome(&order))
    };
    run(&host, &client(&venue), &source, &sample_tick()).unwrap();

    assert_eq!(venue.submit_count(), 1);
    assert!(
        Journal::submitted(&host)
            .contains(&intent_id(&order))
            .unwrap(),
        "submitted:{{intent_id}} marker must be recorded",
    );

    // The next tick short-circuits on the journal: no second submit.
    run(&host, &client(&venue), &source, &sample_tick()).unwrap();
    assert_eq!(venue.submit_count(), 1);
}

#[test]
fn ready_marker_keys_on_the_intent_id_never_the_server_receipt() {
    let host = MockHost::new();
    seed_watch(&host);
    let order = submittable_order();
    let venue = MockVenue::default();
    venue.enqueue_submit(Ok(SubmitOutcome::Accepted(vec![0xFE, 0xED, 0xFA, 0xCE])));

    let source = {
        let order = order.clone();
        src(move |_, _, _, _| ready_outcome(&order))
    };
    run(&host, &client(&venue), &source, &sample_tick()).unwrap();

    let snapshot = host.store.snapshot();
    assert!(snapshot.contains_key(&format!("submitted:{}", intent_id(&order))));
    assert_eq!(
        snapshot
            .keys()
            .filter(|k| k.starts_with("submitted:"))
            .count(),
        1,
        "marker must key on the pre-submit intent-id, not the server receipt",
    );
}

#[test]
fn ready_skips_the_venue_when_the_intent_id_is_journalled() {
    let host = MockHost::new();
    seed_watch(&host);
    let order = submittable_order();
    Journal::submitted(&host)
        .record(&intent_id(&order))
        .unwrap();
    let polls = Cell::new(0_u32);
    let venue = MockVenue::default();

    run(
        &host,
        &client(&venue),
        &src(|_, _, _, _| {
            polls.set(polls.get() + 1);
            ready_outcome(&order)
        }),
        &sample_tick(),
    )
    .unwrap();

    assert_eq!(polls.get(), 1, "the source is still consulted");
    assert_eq!(
        venue.submit_count(),
        0,
        "the journal guard must short-circuit before any network work",
    );
}

#[test]
fn ready_with_unknown_marker_skips_submit_and_keeps_the_watch() {
    let host = MockHost::new();
    let key = seed_watch(&host);
    let mut order = submittable_order();
    order.kind = B256::repeat_byte(0x42);
    let venue = MockVenue::default();

    run(
        &host,
        &client(&venue),
        &src(move |_, _, _, _| ready_outcome(&order)),
        &sample_tick(),
    )
    .unwrap();

    assert_eq!(venue.submit_count(), 0);
    assert!(host.store.snapshot().contains_key(&key));
}

/// A sweep cannot sign: a `requires-signing` outcome is surfaced, not
/// journalled, so the next tick re-poses the same ask.
#[test]
fn requires_signing_is_surfaced_and_not_journalled() {
    let host = MockHost::new();
    let key = seed_watch(&host);
    let order = submittable_order();
    let venue = MockVenue::default();
    venue.enqueue_submit(Ok(SubmitOutcome::RequiresSigning(UnsignedTx {
        chain: SEPOLIA,
        to: vec![0x11; 20],
        value: Vec::new(),
        data: vec![0x22],
    })));

    let source = src(move |_, _, _, _| ready_outcome(&order));
    let (result, logs) = capture_tracing(|| run(&host, &client(&venue), &source, &sample_tick()));
    result.unwrap();

    assert_eq!(venue.submit_count(), 1);
    let snapshot = host.store.snapshot();
    assert!(snapshot.contains_key(&key), "the watch survives");
    assert!(!snapshot.keys().any(|k| k.starts_with("submitted:")));
    assert!(logs.any(|e| e.message.contains("requires signing")));
}

// ---- submission failure dispatch ----

#[test]
fn transient_fault_keeps_the_watch_ungated() {
    let host = MockHost::new();
    let key = seed_watch(&host);
    let watch_key = WatchRef::parse(&key).unwrap();
    let order = submittable_order();
    let venue = MockVenue::default();
    venue.enqueue_submit(Err(VenueFault::Unavailable("orderbook http 502".into())));

    run(
        &host,
        &client(&venue),
        &src(move |_, _, _, _| ready_outcome(&order)),
        &sample_tick(),
    )
    .unwrap();

    let snapshot = host.store.snapshot();
    assert!(snapshot.contains_key(&key));
    assert!(!snapshot.contains_key(&watch_key.next_block_key()));
    assert!(!snapshot.contains_key(&watch_key.next_epoch_key()));
    assert!(!snapshot.keys().any(|k| k.starts_with("submitted:")));
}

#[test]
fn denied_fault_drops_the_watch_through_the_ledger() {
    let host = MockHost::new();
    let key = seed_watch(&host);
    Gates::new(&host)
        .set_next_block(WatchRef::parse(&key).unwrap(), 1)
        .unwrap();
    let order = submittable_order();
    let venue = MockVenue::default();
    venue.enqueue_submit(Err(VenueFault::Denied("InvalidSignature: bad sig".into())));

    let source = src(move |_, _, _, _| ready_outcome(&order));
    let (result, logs) = capture_tracing(|| run(&host, &client(&venue), &source, &sample_tick()));
    result.unwrap();

    assert!(
        host.store.is_empty(),
        "a permanent refusal must drop the watch and its gates",
    );
    assert!(logs.any(|e| e.message.contains("submit dropped watch")));
}

/// A rate-limit fault with server guidance backs the watch off on the
/// epoch clock - `RetryAction::Backoff` reached through the ledger.
#[test]
fn rate_limited_submit_backs_off_through_the_epoch_gate() {
    let host = MockHost::new();
    let key = seed_watch(&host);
    let watch = WatchRef::parse(&key).unwrap();
    let order = submittable_order();
    let venue = MockVenue::default();
    venue.enqueue_submit(Err(VenueFault::RateLimited {
        retry_after_ms: Some(2_500),
    }));

    let tick = sample_tick();
    run(
        &host,
        &client(&venue),
        &src(move |_, _, _, _| ready_outcome(&order)),
        &tick,
    )
    .unwrap();

    let snapshot = host.store.snapshot();
    assert!(snapshot.contains_key(&key), "backoff must keep the watch");
    assert_eq!(
        snapshot.get(&watch.next_epoch_key()).unwrap(),
        &(tick.epoch_s + 3).to_le_bytes().to_vec(),
        "2500ms rounds up to a 3s backoff from the tick clock",
    );
    assert!(!snapshot.keys().any(|k| k.starts_with("submitted:")));
}

/// Restart regression: a keeper that posted, journalled, and then
/// restarted over the same persistent local store must not post the
/// same order again - one venue submit across both lives.
#[test]
fn restart_with_a_journalled_intent_does_not_repost() {
    let host = MockHost::new();
    seed_watch(&host);
    let order = submittable_order();
    let venue = MockVenue::default();
    venue.enqueue_submit(accepted());

    let source = {
        let order = order.clone();
        src(move |_, _, _, _| ready_outcome(&order))
    };
    run(&host, &client(&venue), &source, &sample_tick()).unwrap();
    assert_eq!(venue.submit_count(), 1);

    // A restarted keeper: fresh instance, the local store carried over.
    let restarted = MockHost::new();
    for (key, value) in host.store.snapshot() {
        restarted.store.set(&key, &value).unwrap();
    }
    let venue_after = MockVenue::default();
    venue_after.enqueue_submit(accepted());

    run(&restarted, &client(&venue_after), &source, &sample_tick()).unwrap();

    assert_eq!(
        venue.submit_count() + venue_after.submit_count(),
        1,
        "resubmit after restart must make no second venue submit",
    );
    assert!(
        Journal::submitted(&restarted)
            .contains(&intent_id(&order))
            .unwrap(),
    );
}

// ---- the generic seam ----

/// The seam proof: a `Post` verdict reaches the venue transport as the
/// encoded `CowIntentBody` under the CoW venue id, and the journal
/// keys on the generic submission key.
#[test]
fn ready_submits_the_encoded_intent_body_through_the_venue_seam() {
    let host = MockHost::new();
    seed_watch(&host);
    let order = submittable_order();
    let venue = MockVenue::default();
    venue.enqueue_submit(accepted());

    let source = {
        let order = order.clone();
        src(move |_, _, _, _| ready_outcome(&order))
    };
    run(&host, &client(&venue), &source, &sample_tick()).unwrap();

    let expected = intent_bytes(&order);
    let submits = venue.submits();
    assert_eq!(submits.len(), 1);
    assert_eq!(submits[0].0, CowVenue::ID.as_str());
    assert_eq!(submits[0].1, expected, "the wire carries the intent body");
    assert!(
        Journal::submitted(&host)
            .contains(&submission_key(&CowVenue::ID, &expected))
            .unwrap(),
        "the journal keys on the generic submission key",
    );
}
