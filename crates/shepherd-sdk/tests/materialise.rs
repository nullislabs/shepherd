//! Materialiser acceptance tests against the composed
//! `shepherd_sdk_test::MockHost`. These live as an integration test
//! (not `#[cfg(test)]`) because the mock crate links `shepherd-sdk`
//! externally, and the external and unit-test copies of the traits
//! are distinct types.

use std::cell::Cell;

use alloy_primitives::{Address, B256, U256, address, hex, keccak256};
use cowprotocol::{BuyTokenDestination, GPv2OrderData, OrderKind, SellTokenSource};
use nexum_sdk::chassis::{ConditionalSource, Gates, Journal, Tick, WatchRef, WatchSet};
use nexum_sdk::host::{Fault, LocalStoreHost as _, RateLimit};
use nexum_sdk_test::capture_tracing;
use shepherd_sdk::cow::{CowApiError, OrderRejection, PollOutcome, materialise, order_uid_hex};
use shepherd_sdk_test::MockHost;

const SEPOLIA: u64 = 11_155_111;

/// Closure-backed source so each test scripts its own outcome and
/// observes its own poll calls.
struct FnSource<F>(F);

impl<H, F> ConditionalSource<H> for FnSource<F>
where
    F: Fn(&H, WatchRef<'_>, &[u8], &Tick) -> PollOutcome,
{
    type Outcome = PollOutcome;

    fn poll(&self, host: &H, watch: WatchRef<'_>, params: &[u8], tick: &Tick) -> PollOutcome {
        (self.0)(host, watch, params, tick)
    }
}

/// Pin the closure to the higher-ranked source signature at the
/// construction site so inference never guesses a too-narrow lifetime.
fn src<F>(f: F) -> FnSource<F>
where
    F: Fn(&MockHost, WatchRef<'_>, &[u8], &Tick) -> PollOutcome,
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

/// `validTo` a given number of seconds from now. The `OrderCreation`
/// constructor's client-side max-horizon policy reads the wall clock
/// (not the block clock), so test orders must expire relative to it.
fn valid_to_in(seconds: u64) -> u32 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is after the epoch")
        .as_secs();
    u32::try_from(now + seconds).expect("test validTo fits u32")
}

fn submittable_order() -> GPv2OrderData {
    GPv2OrderData {
        sellToken: address!("6810e776880C02933D47DB1b9fc05908e5386b96"),
        buyToken: address!("DAE5F1590db13E3B40423B5b5c5fbf175515910b"),
        receiver: Address::ZERO,
        sellAmount: U256::from(1_000_000_u64),
        buyAmount: U256::from(999_u64),
        validTo: valid_to_in(3_600),
        appData: cowprotocol::EMPTY_APP_DATA_HASH,
        feeAmount: U256::ZERO,
        kind: OrderKind::SELL,
        partiallyFillable: false,
        sellTokenBalance: SellTokenSource::ERC20,
        buyTokenBalance: BuyTokenDestination::ERC20,
    }
}

fn ready_outcome(order: &GPv2OrderData) -> PollOutcome {
    PollOutcome::Ready {
        order: Box::new(order.clone()),
        signature: hex!("c0ffeec0ffeec0ffee").to_vec().into(),
    }
}

fn seed_watch(host: &MockHost) -> String {
    WatchSet::new(host)
        .put(&sample_owner(), &sample_hash(), b"params")
        .unwrap()
}

fn client_uid(order: &GPv2OrderData) -> String {
    order_uid_hex(SEPOLIA, order, sample_owner()).expect("supported chain, known markers")
}

// ---- lifecycle outcomes ----

#[test]
fn try_next_block_leaves_the_store_untouched() {
    let host = MockHost::new();
    seed_watch(&host);
    let before = host.store.snapshot();

    materialise(
        &host,
        &src(|_, _, _, _| PollOutcome::TryNextBlock),
        &sample_tick(),
    )
    .unwrap();

    assert_eq!(host.store.snapshot(), before);
    assert_eq!(host.cow_api.call_count(), 0);
}

#[test]
fn try_on_block_sets_the_block_gate() {
    let host = MockHost::new();
    let key = seed_watch(&host);
    let watch = WatchRef::parse(&key).unwrap();

    materialise(
        &host,
        &src(|_, _, _, _| PollOutcome::TryOnBlock(2_000)),
        &sample_tick(),
    )
    .unwrap();

    assert_eq!(
        host.store.snapshot().get(&watch.next_block_key()).unwrap(),
        &2_000_u64.to_le_bytes().to_vec(),
    );
}

#[test]
fn try_at_epoch_sets_the_epoch_gate() {
    let host = MockHost::new();
    let key = seed_watch(&host);
    let watch = WatchRef::parse(&key).unwrap();

    materialise(
        &host,
        &src(|_, _, _, _| PollOutcome::TryAtEpoch(1_800_000_000)),
        &sample_tick(),
    )
    .unwrap();

    assert_eq!(
        host.store.snapshot().get(&watch.next_epoch_key()).unwrap(),
        &1_800_000_000_u64.to_le_bytes().to_vec(),
    );
}

#[test]
fn dont_try_again_removes_the_watch_and_its_gates() {
    let host = MockHost::new();
    let key = seed_watch(&host);
    let watch = WatchRef::parse(&key).unwrap();
    Gates::new(&host).set_next_block(watch, 1).unwrap();

    materialise(
        &host,
        &src(|_, _, _, _| PollOutcome::DontTryAgain),
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

    materialise(
        &host,
        &src(|_, _, _, _| {
            polls.set(polls.get() + 1);
            PollOutcome::TryNextBlock
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

    materialise(
        &host,
        &src(|_, _, _, _| {
            polls.set(polls.get() + 1);
            PollOutcome::TryNextBlock
        }),
        &sample_tick(),
    )
    .unwrap();

    assert_eq!(polls.get(), 0);
}

// ---- ready -> submission ----

#[test]
fn ready_submits_once_and_journals_the_client_uid() {
    let host = MockHost::new();
    seed_watch(&host);
    let order = submittable_order();
    host.cow_api.respond(Ok(client_uid(&order)));

    let source = {
        let order = order.clone();
        src(move |_, _, _, _| ready_outcome(&order))
    };
    materialise(&host, &source, &sample_tick()).unwrap();

    assert_eq!(host.cow_api.call_count(), 1);
    assert!(
        Journal::submitted(&host)
            .contains(&client_uid(&order))
            .unwrap(),
        "submitted:{{client_uid}} receipt must be recorded",
    );
    assert_eq!(host.cow_api.last_call().unwrap().chain_id, SEPOLIA);
}

#[test]
fn ready_marker_keys_on_the_client_uid_when_the_server_diverges() {
    let host = MockHost::new();
    seed_watch(&host);
    let order = submittable_order();
    host.cow_api.respond(Ok("0xfeedface".to_string()));

    let source = {
        let order = order.clone();
        src(move |_, _, _, _| ready_outcome(&order))
    };
    let (result, logs) = capture_tracing(|| materialise(&host, &source, &sample_tick()));
    result.unwrap();

    let snapshot = host.store.snapshot();
    assert!(snapshot.contains_key(&format!("submitted:{}", client_uid(&order))));
    assert!(
        !snapshot.contains_key("submitted:0xfeedface"),
        "marker must key on the client UID, not the divergent server UID",
    );
    assert!(logs.any(|e| e.message.contains("UID divergence")));
}

#[test]
fn ready_skips_the_orderbook_when_the_receipt_is_journalled() {
    let host = MockHost::new();
    seed_watch(&host);
    let order = submittable_order();
    Journal::submitted(&host)
        .record(&client_uid(&order))
        .unwrap();
    let polls = Cell::new(0_u32);

    materialise(
        &host,
        &src(|_, _, _, _| {
            polls.set(polls.get() + 1);
            ready_outcome(&order)
        }),
        &sample_tick(),
    )
    .unwrap();

    assert_eq!(polls.get(), 1, "the source is still consulted");
    assert_eq!(
        host.cow_api.call_count(),
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

    materialise(
        &host,
        &src(move |_, _, _, _| ready_outcome(&order)),
        &sample_tick(),
    )
    .unwrap();

    assert_eq!(host.cow_api.call_count(), 0);
    assert!(host.store.snapshot().contains_key(&key));
}

// ---- submission failure dispatch ----

fn rejection(error_type: &str) -> CowApiError {
    CowApiError::Rejected(OrderRejection {
        status: 400,
        error_type: error_type.into(),
        description: "test".into(),
        data: None,
    })
}

#[test]
fn transient_rejection_keeps_the_watch_ungated() {
    let host = MockHost::new();
    let key = seed_watch(&host);
    let watch_key = WatchRef::parse(&key).unwrap();
    let order = submittable_order();
    host.cow_api.respond(Err(rejection("InsufficientFee")));

    materialise(
        &host,
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
fn permanent_rejection_drops_the_watch_through_the_ledger() {
    let host = MockHost::new();
    let key = seed_watch(&host);
    Gates::new(&host)
        .set_next_block(WatchRef::parse(&key).unwrap(), 1)
        .unwrap();
    let order = submittable_order();
    host.cow_api.respond(Err(rejection("InvalidSignature")));

    materialise(
        &host,
        &src(move |_, _, _, _| ready_outcome(&order)),
        &sample_tick(),
    )
    .unwrap();

    assert!(
        host.store.is_empty(),
        "a permanent rejection must drop the watch and its gates",
    );
}

/// The orderbook already holds the order: the receipt is recorded, the
/// watch survives, and the next tick short-circuits on the journal
/// instead of re-posting.
#[test]
fn duplicated_order_records_the_receipt_and_keeps_the_watch() {
    let host = MockHost::new();
    let key = seed_watch(&host);
    let order = submittable_order();
    host.cow_api.respond(Err(rejection("DuplicatedOrder")));

    let source = {
        let order = order.clone();
        src(move |_, _, _, _| ready_outcome(&order))
    };
    materialise(&host, &source, &sample_tick()).unwrap();

    assert!(host.store.snapshot().contains_key(&key));
    assert!(
        Journal::submitted(&host)
            .contains(&client_uid(&order))
            .unwrap(),
        "already-submitted must record the receipt",
    );

    // The next tick must not touch the orderbook again.
    materialise(&host, &source, &sample_tick()).unwrap();
    assert_eq!(host.cow_api.call_count(), 1);
}

/// A rate-limit fault with server guidance backs the watch off on the
/// epoch clock - `RetryAction::Backoff` reached through the ledger.
#[test]
fn rate_limited_submit_backs_off_through_the_epoch_gate() {
    let host = MockHost::new();
    let key = seed_watch(&host);
    let watch = WatchRef::parse(&key).unwrap();
    let order = submittable_order();
    host.cow_api
        .respond(Err(CowApiError::Fault(Fault::RateLimited(RateLimit {
            retry_after_ms: Some(2_500),
        }))));

    let tick = sample_tick();
    materialise(&host, &src(move |_, _, _, _| ready_outcome(&order)), &tick).unwrap();

    let snapshot = host.store.snapshot();
    assert!(snapshot.contains_key(&key), "backoff must keep the watch");
    assert_eq!(
        snapshot.get(&watch.next_epoch_key()).unwrap(),
        &(tick.epoch_s + 3).to_le_bytes().to_vec(),
        "2500ms rounds up to a 3s backoff from the tick clock",
    );
    assert!(!snapshot.keys().any(|k| k.starts_with("submitted:")));
}
