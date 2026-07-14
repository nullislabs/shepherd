//! MockVenue acceptance tests: the scripted venue driving the keeper
//! run (multi-tick retry, backoff, and outage scenarios the
//! single-replayed-response mock cannot express) and module-shaped
//! strategy code polling the venue directly.

use alloy_primitives::{Address, B256, U256, address, hex, keccak256};
use cowprotocol::{BuyTokenDestination, GPv2OrderData, OrderKind, SellTokenSource};
use nexum_sdk::host::{Fault, LocalStoreHost as _, RateLimit};
use nexum_sdk::keeper::{ConditionalSource, Journal, Tick, WatchRef, WatchSet, watch_key};
use shepherd_sdk::cow::{CowApiError, CowHost, OrderRejection, PollOutcome, order_uid_hex, run};
use shepherd_sdk_test::{MockHost, MockVenue};

const SEPOLIA: u64 = 11_155_111;

type VenueHost = MockHost<MockVenue>;

/// Closure-backed source so each test scripts its own outcome.
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
    F: Fn(&VenueHost, WatchRef<'_>, &[u8], &Tick) -> PollOutcome,
{
    FnSource(f)
}

fn sample_owner() -> Address {
    address!("00112233445566778899aabbccddeeff00112233")
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

fn ready_source(
    order: &GPv2OrderData,
) -> FnSource<impl Fn(&VenueHost, WatchRef<'_>, &[u8], &Tick) -> PollOutcome> {
    let order = order.clone();
    src(move |_, _, _, _| ready_outcome(&order))
}

fn seed_watch(host: &VenueHost) -> String {
    WatchSet::new(host)
        .put(
            &sample_owner(),
            &keccak256(b"conditional order params"),
            b"params",
        )
        .unwrap()
}

fn client_uid(order: &GPv2OrderData) -> String {
    order_uid_hex(SEPOLIA, order, sample_owner()).expect("supported chain, known markers")
}

fn rejection(error_type: &str) -> CowApiError {
    CowApiError::Rejected(OrderRejection {
        status: 400,
        error_type: error_type.into(),
        description: "test".into(),
        data: None,
    })
}

// ---- keeper use ----

/// A transient rejection on the first tick keeps the watch alive; the
/// next tick's scripted success is journalled. Per-call scripting is
/// the point: one venue plays a different outcome on each tick.
#[test]
fn keeper_retries_a_transient_rejection_then_submits() {
    let host = MockHost::with_venue();
    let key = seed_watch(&host);
    let order = submittable_order();
    host.cow_api
        .enqueue_submit(Err(rejection("InsufficientFee")));
    host.cow_api.enqueue_submit(Ok(client_uid(&order)));

    let source = ready_source(&order);
    run(&host, &source, &sample_tick()).unwrap();
    assert_eq!(host.cow_api.call_count(), 1);
    assert!(host.store.snapshot().contains_key(&key), "watch survives");
    assert!(
        !Journal::submitted(&host)
            .contains(&client_uid(&order))
            .unwrap()
    );

    run(&host, &source, &sample_tick()).unwrap();
    assert_eq!(host.cow_api.call_count(), 2);
    assert!(
        Journal::submitted(&host)
            .contains(&client_uid(&order))
            .unwrap()
    );
    assert_eq!(
        host.cow_api.pending_submits(),
        0,
        "scenario played out in full"
    );
}

/// A rate-limit with server guidance gates the watch on the epoch
/// clock; the venue is only reached again once the gate clears, and
/// the queued success then lands.
#[test]
fn keeper_backs_off_on_rate_limit_and_submits_after_the_gate() {
    let host = MockHost::with_venue();
    seed_watch(&host);
    let order = submittable_order();
    host.cow_api
        .enqueue_submit(Err(CowApiError::Fault(Fault::RateLimited(RateLimit {
            retry_after_ms: Some(2_500),
        }))));
    host.cow_api.enqueue_submit(Ok(client_uid(&order)));

    let t0 = sample_tick();
    let source = ready_source(&order);
    run(&host, &source, &t0).unwrap();
    assert_eq!(host.cow_api.call_count(), 1);

    // 2500ms rounds up to a 3s epoch gate: a tick inside it never
    // reaches the venue.
    let gated = Tick {
        epoch_s: t0.epoch_s + 2,
        ..t0
    };
    run(&host, &source, &gated).unwrap();
    assert_eq!(host.cow_api.call_count(), 1, "gated tick must not submit");

    let clear = Tick {
        epoch_s: t0.epoch_s + 3,
        ..t0
    };
    run(&host, &source, &clear).unwrap();
    assert_eq!(host.cow_api.call_count(), 2);
    assert!(
        Journal::submitted(&host)
            .contains(&client_uid(&order))
            .unwrap()
    );
}

/// A venue outage is transient: the watch stays, nothing is gated, and
/// the first tick after recovery submits the queued outcome.
#[test]
fn keeper_survives_a_venue_outage_and_submits_on_recovery() {
    let host = MockHost::with_venue();
    let key = seed_watch(&host);
    let watch_key = WatchRef::parse(&key).unwrap();
    let order = submittable_order();
    host.cow_api
        .inject_fault(CowApiError::Fault(Fault::Unavailable("venue down".into())));

    let source = ready_source(&order);
    run(&host, &source, &sample_tick()).unwrap();
    let snapshot = host.store.snapshot();
    assert!(snapshot.contains_key(&key));
    assert!(!snapshot.contains_key(&watch_key.next_block_key()));
    assert!(!snapshot.contains_key(&watch_key.next_epoch_key()));
    assert_eq!(host.cow_api.call_count(), 1);

    host.cow_api.clear_fault();
    host.cow_api.enqueue_submit(Ok(client_uid(&order)));
    run(&host, &source, &sample_tick()).unwrap();
    assert_eq!(host.cow_api.call_count(), 2);
    assert!(
        Journal::submitted(&host)
            .contains(&client_uid(&order))
            .unwrap()
    );
}

/// A scripted permanent rejection drops the watch through the ledger.
#[test]
fn keeper_drops_the_watch_on_a_scripted_permanent_rejection() {
    let host = MockHost::with_venue();
    seed_watch(&host);
    let order = submittable_order();
    host.cow_api
        .enqueue_submit(Err(rejection("InvalidSignature")));

    run(&host, &ready_source(&order), &sample_tick()).unwrap();

    assert!(host.store.is_empty(), "watch and gates must go");
    assert_eq!(host.cow_api.call_count(), 1);
}

/// Keeper rows written through the composed host stay invisible to a
/// sibling store namespace, and a decoy watch planted there never
/// reaches the sweep - the store-fidelity seam under the venue tests.
#[test]
fn keeper_sweep_ignores_sibling_namespace_watches() {
    let host = MockHost::with_venue();
    seed_watch(&host);
    let sibling = host.store.namespaced("other-module");
    assert!(sibling.is_empty(), "keeper rows must not leak across");
    sibling
        .set(
            "watch:0x00112233445566778899aabbccddeeff00112233:0xdead",
            b"decoy",
        )
        .unwrap();

    let order = submittable_order();
    host.cow_api.enqueue_submit(Ok(client_uid(&order)));
    let polls = std::cell::Cell::new(0_u32);
    run(
        &host,
        &src(|_, _, _, _| {
            polls.set(polls.get() + 1);
            ready_outcome(&order)
        }),
        &sample_tick(),
    )
    .unwrap();

    assert_eq!(polls.get(), 1, "only this module's watch is swept");
    assert_eq!(host.cow_api.call_count(), 1);
}

// ---- module use ----

/// Module-shaped fill tracker: probe the orderbook status route and
/// journal an `observed:` receipt once the order reports fulfilled.
/// Generic over [`CowHost`] exactly like production strategy code.
fn record_fill<H: CowHost>(host: &H, chain_id: u64, uid: &str) -> Result<bool, Fault> {
    let path = format!("/api/v1/orders/{uid}");
    let Ok(body) = host.cow_api_request(chain_id, "GET", &path, None) else {
        return Ok(false);
    };
    let fulfilled = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("status").and_then(|s| s.as_str().map(str::to_owned)))
        .is_some_and(|status| status == "fulfilled");
    if fulfilled {
        Journal::observed(host).record(uid)?;
    }
    Ok(fulfilled)
}

/// The status sequence advances one entry per module poll and its
/// terminal entry persists across any number of re-polls.
#[test]
fn module_tracks_a_fill_through_a_status_sequence() {
    let host = MockHost::with_venue();
    for body in [
        r#"{"status":"open"}"#,
        r#"{"status":"open"}"#,
        r#"{"status":"fulfilled"}"#,
    ] {
        host.cow_api.enqueue_order_status("0xuid", Ok(body.into()));
    }

    assert!(!record_fill(&host, SEPOLIA, "0xuid").unwrap());
    assert!(!record_fill(&host, SEPOLIA, "0xuid").unwrap());
    assert!(record_fill(&host, SEPOLIA, "0xuid").unwrap());
    // Terminal status sticks: an over-eager re-poll sees it again.
    assert!(record_fill(&host, SEPOLIA, "0xuid").unwrap());

    assert!(Journal::observed(&host).contains("0xuid").unwrap());
    assert_eq!(host.cow_api.request_calls().len(), 4);
    assert_eq!(host.cow_api.request_calls()[0].path, "/api/v1/orders/0xuid");
}

/// An outage mid-sequence surfaces to the module as a failed probe and
/// consumes nothing: the sequence resumes where it left off.
#[test]
fn module_probe_rides_out_an_injected_outage() {
    let host = MockHost::with_venue();
    host.cow_api
        .enqueue_order_status("0xuid", Ok(r#"{"status":"open"}"#.into()));
    host.cow_api
        .enqueue_order_status("0xuid", Ok(r#"{"status":"fulfilled"}"#.into()));

    assert!(!record_fill(&host, SEPOLIA, "0xuid").unwrap());

    host.cow_api
        .inject_fault(CowApiError::Fault(Fault::Timeout));
    assert!(!record_fill(&host, SEPOLIA, "0xuid").unwrap());

    host.cow_api.clear_fault();
    assert!(record_fill(&host, SEPOLIA, "0xuid").unwrap());
    assert!(Journal::observed(&host).contains("0xuid").unwrap());
}

/// The free `watch_key` helper produces exactly the key
/// `WatchSet::put` writes through the venue host, so a test can seed
/// or assert rows without a host turbofish.
#[test]
fn watch_key_helper_unifies_with_the_venue_host() {
    let host = MockHost::with_venue();
    let hash: B256 = keccak256(b"conditional order params");
    let written = WatchSet::new(&host)
        .put(&sample_owner(), &hash, b"params")
        .unwrap();
    assert_eq!(watch_key(&sample_owner(), &hash), written);
}
