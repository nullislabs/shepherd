//! Run acceptance tests: `run` over the generic store mocks with a
//! scripted venue transport on the `videre:venue/client` seam.

use std::cell::{Cell, RefCell};
use std::collections::{HashSet, VecDeque};

use alloy_primitives::{Address, B256, U256, address, hex, keccak256};
use composable_cow::{Verdict, run};
use cow_venue::assembly::{gpv2_to_order_data, order_data_to_body};
use cow_venue::{CowClient, CowIntent, CowIntentBody, CowVenue, SignedOrder};
use cowprotocol::{BuyTokenDestination, GPv2OrderData, OrderKind, SellTokenSource};
use nexum_sdk::host::{EntryPage, Fault, ListQuery, LocalStoreHost};
use nexum_sdk::keeper::{CommitmentRef, CommitmentSet, Gates, Journal, Mark, Poller, Tick};
use nexum_sdk_test::{MockHost, MockLocalStore, capture_tracing};
use videre_sdk::client::sealed::SealedTransport;
use videre_sdk::keeper::submission_key;
use videre_sdk::{
    IntentBody as _, IntentStatus, Quotation, SubmitOutcome, UnsignedTx, Venue as _, VenueFault,
    VenueId, VenueTransport,
};

const SEPOLIA: u64 = 11_155_111;

/// Scripted venue transport: one submit outcome per queued entry,
/// every submit recorded. Quote, status, and cancel are off the run
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

impl<H, F> Poller<H> for FnSource<F>
where
    F: Fn(&H, CommitmentRef<'_>, &[u8], &Tick) -> Verdict,
{
    type Outcome = Verdict;

    fn poll(&self, host: &H, commitment: CommitmentRef<'_>, params: &[u8], tick: &Tick) -> Verdict {
        (self.0)(host, commitment, params, tick)
    }
}

/// Pin the closure to the source signature so inference keeps the
/// higher-ranked lifetime.
fn src<F>(f: F) -> FnSource<F>
where
    F: Fn(&MockHost, CommitmentRef<'_>, &[u8], &Tick) -> Verdict,
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
        next_poll: None,
    }
}

fn seed_commitment(host: &MockHost) -> String {
    CommitmentSet::new(host)
        .put(&sample_owner(), &sample_hash(), b"params")
        .unwrap()
}

/// The encoded intent body the run submits for `order`.
fn intent_bytes(order: &GPv2OrderData) -> Vec<u8> {
    let order_data = gpv2_to_order_data(order).expect("known markers");
    CowIntentBody::V1(CowIntent::Signed(SignedOrder {
        order: order_data_to_body(&order_data),
        owner: sample_owner(),
        signature: hex!("c0ffeec0ffeec0ffee").to_vec(),
    }))
    .to_bytes()
    .expect("body encodes")
}

/// The intent-id the run journals for `order`.
fn intent_id(order: &GPv2OrderData) -> String {
    submission_key(&CowVenue::ID, &intent_bytes(order))
}

fn accepted() -> Result<SubmitOutcome, VenueFault> {
    Ok(SubmitOutcome::Accepted(vec![0xAA]))
}

#[test]
fn try_next_block_leaves_the_store_untouched() {
    let host = MockHost::new();
    seed_commitment(&host);
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
    let key = seed_commitment(&host);
    let commitment = CommitmentRef::parse(&key).unwrap();
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
        host.store
            .snapshot()
            .get(&commitment.next_block_key())
            .unwrap(),
        &2_000_u64.to_le_bytes().to_vec(),
    );
}

#[test]
fn wait_timestamp_sets_the_epoch_gate() {
    let host = MockHost::new();
    let key = seed_commitment(&host);
    let commitment = CommitmentRef::parse(&key).unwrap();
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
        host.store
            .snapshot()
            .get(&commitment.next_epoch_key())
            .unwrap(),
        &1_800_000_000_u64.to_le_bytes().to_vec(),
    );
}

#[test]
fn invalid_removes_the_commitment_and_its_gates() {
    let host = MockHost::new();
    let key = seed_commitment(&host);
    let commitment = CommitmentRef::parse(&key).unwrap();
    Gates::new(&host).set_next_block(commitment, 1).unwrap();
    let venue = MockVenue::default();

    run(
        &host,
        &client(&venue),
        &src(|_, _, _, _| Verdict::Invalid { reason: [0; 4] }),
        &sample_tick(),
    )
    .unwrap();

    assert!(host.store.is_empty(), "commitment and gates must go");
}

#[test]
fn gated_commitment_is_not_polled() {
    let host = MockHost::new();
    let key = seed_commitment(&host);
    Gates::new(&host)
        .set_next_block(CommitmentRef::parse(&key).unwrap(), 5_000)
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

    assert_eq!(
        polls.get(),
        0,
        "a gated commitment must not reach the source"
    );
}

#[test]
fn malformed_commitment_rows_are_skipped() {
    let host = MockHost::new();
    host.store.set("commitment:no-separator", b"junk").unwrap();
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

#[test]
fn ready_submits_once_and_journals_the_intent_id() {
    let host = MockHost::new();
    seed_commitment(&host);
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
    seed_commitment(&host);
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
    seed_commitment(&host);
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
fn ready_with_unknown_marker_skips_submit_and_keeps_the_commitment() {
    let host = MockHost::new();
    let key = seed_commitment(&host);
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

/// A run cannot sign: `requires-signing` is surfaced, not journalled.
#[test]
fn requires_signing_is_surfaced_and_not_journalled() {
    let host = MockHost::new();
    let key = seed_commitment(&host);
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
    assert!(snapshot.contains_key(&key), "the commitment survives");
    assert!(!snapshot.keys().any(|k| k.starts_with("submitted:")));
    assert!(logs.any(|e| e.message.contains("requires signing")));
}

#[test]
fn transient_fault_keeps_the_commitment_ungated() {
    let host = MockHost::new();
    let key = seed_commitment(&host);
    let commitment = CommitmentRef::parse(&key).unwrap();
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
    assert!(!snapshot.contains_key(&commitment.next_block_key()));
    assert!(!snapshot.contains_key(&commitment.next_epoch_key()));
    assert!(!snapshot.keys().any(|k| k.starts_with("submitted:")));
}

#[test]
fn denied_fault_drops_the_commitment_through_the_ledger() {
    let host = MockHost::new();
    let key = seed_commitment(&host);
    Gates::new(&host)
        .set_next_block(CommitmentRef::parse(&key).unwrap(), 1)
        .unwrap();
    let order = submittable_order();
    let venue = MockVenue::default();
    venue.enqueue_submit(Err(VenueFault::Denied("InvalidSignature: bad sig".into())));

    let source = src(move |_, _, _, _| ready_outcome(&order));
    let (result, logs) = capture_tracing(|| run(&host, &client(&venue), &source, &sample_tick()));
    result.unwrap();

    assert!(
        host.store.is_empty(),
        "a permanent refusal must drop the commitment and its gates",
    );
    assert!(logs.any(|e| e.message.contains("submit dropped commitment")));
}

/// A rate-limit fault backs the commitment off on the epoch clock.
#[test]
fn rate_limited_submit_backs_off_through_the_epoch_gate() {
    let host = MockHost::new();
    let key = seed_commitment(&host);
    let commitment = CommitmentRef::parse(&key).unwrap();
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
    assert!(
        snapshot.contains_key(&key),
        "backoff must keep the commitment"
    );
    assert_eq!(
        snapshot.get(&commitment.next_epoch_key()).unwrap(),
        &(tick.epoch_s + 3).to_le_bytes().to_vec(),
        "2500ms rounds up to a 3s backoff from the tick clock",
    );
    assert!(!snapshot.keys().any(|k| k.starts_with("submitted:")));
}

/// The same-block wiring+create race: the orderbook rejects the first
/// submission against its own head.
fn eip1271_rejection() -> Result<SubmitOutcome, VenueFault> {
    Err(VenueFault::Denied(
        "InvalidEip1271Signature: signature for computed order hash 0x7ee5 is not valid".into(),
    ))
}

/// First EIP-1271 rejection gates to the next block; the retry one
/// block later lands.
#[test]
fn first_eip1271_rejection_retries_on_the_next_block() {
    let host = MockHost::new();
    let key = seed_commitment(&host);
    let commitment = CommitmentRef::parse(&key).unwrap();
    let order = submittable_order();
    let venue = MockVenue::default();
    venue.enqueue_submit(eip1271_rejection());
    venue.enqueue_submit(accepted());

    let source = {
        let order = order.clone();
        src(move |_, _, _, _| ready_outcome(&order))
    };
    let tick = sample_tick();
    let (result, logs) = capture_tracing(|| run(&host, &client(&venue), &source, &tick));
    result.unwrap();

    let snapshot = host.store.snapshot();
    assert!(
        snapshot.contains_key(&key),
        "first rejection keeps the commitment"
    );
    assert_eq!(
        snapshot.get(&commitment.next_block_key()).unwrap(),
        &(tick.block + 1).to_le_bytes().to_vec(),
        "the commitment gates to the next block",
    );
    assert!(logs.any(|e| e.message.contains("drop-on-repeat")));

    // Sub-block re-polls stay gated: the race is not hammered.
    run(&host, &client(&venue), &source, &tick).unwrap();
    assert_eq!(venue.submit_count(), 1);

    // One block later the wiring is visible and the retry lands.
    let next = Tick {
        block: tick.block + 1,
        ..tick
    };
    run(&host, &client(&venue), &source, &next).unwrap();
    assert_eq!(venue.submit_count(), 2);
    assert!(
        Journal::submitted(&host)
            .contains(&intent_id(&order))
            .unwrap(),
    );
}

/// A rejection repeating on a later block drops the commitment and its keys.
#[test]
fn repeated_eip1271_rejection_on_a_later_block_drops_the_commitment() {
    let host = MockHost::new();
    seed_commitment(&host);
    let order = submittable_order();
    let venue = MockVenue::default();
    venue.enqueue_submit(eip1271_rejection());
    venue.enqueue_submit(eip1271_rejection());

    let source = src(move |_, _, _, _| ready_outcome(&order));
    let tick = sample_tick();
    run(&host, &client(&venue), &source, &tick).unwrap();

    let next = Tick {
        block: tick.block + 1,
        ..tick
    };
    run(&host, &client(&venue), &source, &next).unwrap();

    assert_eq!(venue.submit_count(), 2);
    assert!(
        host.store.is_empty(),
        "a repeated rejection must drop the commitment, its gates, and the marker",
    );
}

/// An acceptance ends the refusal episode; a later tranche's first
/// rejection earns a fresh one-block grace.
#[test]
fn acceptance_resets_the_one_block_grace_for_later_tranches() {
    let host = MockHost::new();
    let key = seed_commitment(&host);
    let commitment = CommitmentRef::parse(&key).unwrap();
    let tranche_one = submittable_order();
    let mut tranche_two = submittable_order();
    tranche_two.buyAmount = U256::from(1_001_u64);

    let venue = MockVenue::default();
    venue.enqueue_submit(eip1271_rejection());
    venue.enqueue_submit(accepted());
    venue.enqueue_submit(eip1271_rejection());

    let tick = sample_tick();
    let boundary = tick.block + 5;
    let source = src(move |_, _, _, t: &Tick| {
        if t.block < boundary {
            ready_outcome(&tranche_one)
        } else {
            ready_outcome(&tranche_two)
        }
    });

    // Tranche one: refused at the tick block, accepted one block later.
    run(&host, &client(&venue), &source, &tick).unwrap();
    let next = Tick {
        block: tick.block + 1,
        ..tick
    };
    run(&host, &client(&venue), &source, &next).unwrap();
    assert_eq!(venue.submit_count(), 2);
    assert!(
        !host
            .store
            .snapshot()
            .contains_key(&commitment.refused_key()),
        "acceptance must clear the first-refusal marker",
    );

    // Tranche two: its own first rejection at a later block keeps the
    // commitment and gates it to the next block.
    let later = Tick {
        block: boundary,
        ..tick
    };
    run(&host, &client(&venue), &source, &later).unwrap();
    let snapshot = host.store.snapshot();
    assert!(
        snapshot.contains_key(&key),
        "a fresh refusal after an acceptance must keep the commitment",
    );
    assert_eq!(
        snapshot.get(&commitment.refused_key()).unwrap(),
        &later.block.to_le_bytes().to_vec(),
    );
    assert_eq!(
        snapshot.get(&commitment.next_block_key()).unwrap(),
        &(later.block + 1).to_le_bytes().to_vec(),
    );
}

/// Restart regression: a journalled intent is not re-posted after
/// restart, one venue submit across both lives.
#[test]
fn restart_with_a_journalled_intent_does_not_repost() {
    let host = MockHost::new();
    seed_commitment(&host);
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

/// The seam proof: a `Post` reaches the transport as the encoded
/// `CowIntentBody`, keyed on the generic submission key.
#[test]
fn ready_submits_the_encoded_intent_body_through_the_venue_seam() {
    let host = MockHost::new();
    seed_commitment(&host);
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

/// Models the CoW re-POST floor: a held body re-accepts, so a reconcile
/// resubmit is always safe. A fresh body gets the programmed outcome, an
/// accepted body joins the held set. Every POST is recorded.
struct HoldingVenue {
    outcome: RefCell<Result<SubmitOutcome, VenueFault>>,
    posts: RefCell<Vec<Vec<u8>>>,
    held: RefCell<HashSet<Vec<u8>>>,
}

impl HoldingVenue {
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

    fn posts(&self) -> Vec<Vec<u8>> {
        self.posts.borrow().clone()
    }

    fn post_count(&self) -> usize {
        self.posts.borrow().len()
    }

    fn held_count(&self) -> usize {
        self.held.borrow().len()
    }

    /// Pre-seed a held body: a POST the venue received before the caller
    /// lost its outcome.
    fn preload(&self, body: &[u8]) {
        self.held.borrow_mut().insert(body.to_vec());
    }
}

impl SealedTransport for &HoldingVenue {}

impl VenueTransport for &HoldingVenue {
    async fn quote(&self, _venue: &VenueId, _body: Vec<u8>) -> Result<Quotation, VenueFault> {
        unreachable!("quote not exercised")
    }

    async fn submit(&self, _venue: &VenueId, body: Vec<u8>) -> Result<SubmitOutcome, VenueFault> {
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

    async fn status(&self, _venue: &VenueId, _receipt: &[u8]) -> Result<IntentStatus, VenueFault> {
        unreachable!("status not exercised")
    }

    async fn cancel(&self, _venue: &VenueId, _receipt: &[u8]) -> Result<(), VenueFault> {
        unreachable!("cancel not exercised")
    }
}

fn holding_client(venue: &HoldingVenue) -> CowClient<&HoldingVenue> {
    CowClient::with_transport(venue)
}

/// Faults the first `COMMITTED` write to `submitted:` once: models a
/// commit write that faults, leaving the marker RESERVED with no release.
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

impl LocalStoreHost for FlakyCommit {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Fault> {
        self.inner.get(key)
    }

    fn set(&self, key: &str, value: &[u8]) -> Result<(), Fault> {
        // 0x02 is the journal COMMITTED tag.
        if self.arm.get() && key.starts_with("submitted:") && value.first() == Some(&0x02) {
            self.arm.set(false);
            return Err(Fault::unavailable("commit write faulted"));
        }
        self.inner.set(key, value)
    }

    fn delete(&self, key: &str) -> Result<(), Fault> {
        self.inner.delete(key)
    }

    fn list_keys(&self, prefix: &str) -> Result<Vec<String>, Fault> {
        self.inner.list_keys(prefix)
    }

    // Delegated, not defaulted: the default re-derives entries from
    // `list_keys` + `get` and loses the inner store's paging.
    fn list_entries(&self, query: &ListQuery<'_>) -> Result<EntryPage, Fault> {
        self.inner.list_entries(query)
    }

    fn contains(&self, key: &str) -> Result<bool, Fault> {
        self.inner.contains(key)
    }

    fn len(&self, key: &str) -> Result<Option<u64>, Fault> {
        LocalStoreHost::len(&self.inner, key)
    }

    fn count(&self, prefix: &str) -> Result<u64, Fault> {
        self.inner.count(prefix)
    }
}

/// A source that never submits: exercises the reconcile pass alone.
struct Idle;

impl<H> Poller<H> for Idle {
    type Outcome = Verdict;

    fn poll(
        &self,
        _host: &H,
        _commitment: CommitmentRef<'_>,
        _params: &[u8],
        _tick: &Tick,
    ) -> Verdict {
        Verdict::TryNextBlock { reason: [0; 4] }
    }
}

/// A source that posts one fixed order on every poll.
struct PostOnce(GPv2OrderData);

impl<H> Poller<H> for PostOnce {
    type Outcome = Verdict;

    fn poll(
        &self,
        _host: &H,
        _commitment: CommitmentRef<'_>,
        _params: &[u8],
        _tick: &Tick,
    ) -> Verdict {
        ready_outcome(&self.0)
    }
}

/// Seed a stranded `RESERVED` marker, as a prior tick's reserve whose
/// outcome never landed.
fn seed_reserved(host: &impl LocalStoreHost, order: &GPv2OrderData) {
    Journal::submitted(host)
        .reserve(&intent_id(order), &intent_bytes(order))
        .unwrap();
}

fn cow_mark(host: &impl LocalStoreHost, order: &GPv2OrderData) -> Option<Mark> {
    Journal::submitted(host).mark(&intent_id(order)).unwrap()
}

/// W1: reserved, but the venue never saw the POST. The next tick's
/// reconcile resubmits to exactly one held order.
#[test]
fn w1_reserved_but_venue_never_saw_the_post_reconciles() {
    let host = MockLocalStore::default();
    let order = submittable_order();
    seed_reserved(&host, &order);
    let venue = HoldingVenue::accepting();

    run(&host, &holding_client(&venue), &Idle, &sample_tick()).unwrap();

    assert_eq!(
        venue.post_count(),
        1,
        "reconcile resubmits the stranded body"
    );
    assert_eq!(venue.held_count(), 1, "exactly one held order");
    assert_eq!(
        venue.posts()[0],
        intent_bytes(&order),
        "the reserved body round-trips",
    );
    assert_eq!(cow_mark(&host, &order), Some(Mark::Committed));
}

/// W2: accepted, then the commit faults, leaving the marker RESERVED.
/// The next tick's reconcile resubmits, the venue dedups
/// (AlreadyHeld -> Accepted), the commit lands: two POSTs, one held.
#[test]
fn w2_accepted_then_commit_faults_reconciles_without_double_holding() {
    let host = FlakyCommit::new();
    CommitmentSet::new(&host)
        .put(&sample_owner(), &sample_hash(), b"params")
        .unwrap();
    let order = submittable_order();
    let venue = HoldingVenue::accepting();

    // Tick A: reserve, venue accepts (POST #1), the commit write faults;
    // the RESERVED marker persists, no release runs.
    run(
        &host,
        &holding_client(&venue),
        &PostOnce(order.clone()),
        &sample_tick(),
    )
    .unwrap();
    assert_eq!(venue.post_count(), 1);
    assert_eq!(
        cow_mark(&host, &order),
        Some(Mark::Reserved),
        "a commit fault leaves the marker RESERVED",
    );

    // Tick B: reconcile re-POSTs (POST #2), the venue dedups, the commit
    // lands. The fresh loop then sees COMMITTED and never re-posts.
    run(
        &host,
        &holding_client(&venue),
        &PostOnce(order.clone()),
        &sample_tick(),
    )
    .unwrap();
    assert_eq!(
        venue.post_count(),
        2,
        "reconcile re-POSTs the reserved body"
    );
    assert_eq!(venue.held_count(), 1, "one held order despite two POSTs");
    assert_eq!(cow_mark(&host, &order), Some(Mark::Committed));
}

/// W3: the run was abandoned after the venue received the POST. The next
/// tick's reconcile re-POSTs, the AlreadyHeld backstop accepts, one held.
#[test]
fn w3_abandoned_after_the_post_reconciles_to_one_held() {
    let host = MockLocalStore::default();
    let order = submittable_order();
    seed_reserved(&host, &order);
    let venue = HoldingVenue::accepting();
    venue.preload(&intent_bytes(&order));

    run(&host, &holding_client(&venue), &Idle, &sample_tick()).unwrap();

    assert_eq!(venue.post_count(), 1);
    assert_eq!(venue.held_count(), 1, "the already-held order stays single");
    assert_eq!(cow_mark(&host, &order), Some(Mark::Committed));
    // The venue-never-saw-it sub-case is W1 above.
}

/// Anti-#572: a RESERVED marker drives a reconcile POST through
/// `venue.submit`, where the AlreadyHeld backstop catches the duplicate.
#[test]
fn anti_572_reserved_marker_drives_a_reconcile_post_through_the_venue() {
    let host = MockLocalStore::default();
    let order = submittable_order();
    seed_reserved(&host, &order);
    let venue = HoldingVenue::accepting();
    // The venue already holds it: the reconcile POST hits the
    // AlreadyHeld -> Accepted path, not a fresh accept.
    venue.preload(&intent_bytes(&order));

    run(&host, &holding_client(&venue), &Idle, &sample_tick()).unwrap();

    assert_eq!(
        venue.post_count(),
        1,
        "the reserved marker POSTs, never a silent skip",
    );
    assert_eq!(
        venue.posts()[0],
        intent_bytes(&order),
        "the exact reserved bytes re-POST",
    );
    assert_eq!(venue.held_count(), 1, "no duplicate order");
    assert_eq!(
        cow_mark(&host, &order),
        Some(Mark::Committed),
        "the backstop-accepted resubmit commits",
    );
}
