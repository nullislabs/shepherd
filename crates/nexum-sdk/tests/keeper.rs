//! Keeper-store acceptance tests against the composed
//! `nexum_sdk_test::MockHost` - the keeper touches only the local
//! store, so the world-neutral mock is the whole seam. These live as
//! an integration test (not `#[cfg(test)]`) because the mock crate
//! links `nexum-sdk` externally, and the external and unit-test
//! copies of the host traits are distinct types.

use alloy_primitives::{Address, B256, address, b256};
use nexum_sdk::host::{Fault, LocalStoreHost as _};
use nexum_sdk::keeper::{
    ConditionalSource, Gates, Journal, NEXT_BLOCK_PREFIX, NEXT_EPOCH_PREFIX, REFUSED_PREFIX,
    Retrier, RetryAction, Tick, WATCH_PREFIX, WatchRef, WatchSet, watch_key,
};
use nexum_sdk_test::MockHost;

fn sample_owner() -> Address {
    address!("00112233445566778899aabbccddeeff00112233")
}

fn sample_hash() -> B256 {
    b256!("0202020202020202020202020202020202020202020202020202020202020202")
}

// ---- watch keys ----

#[test]
fn watch_key_is_lowercase_prefixed_hex() {
    let key = watch_key(&sample_owner(), &sample_hash());
    assert_eq!(
        key,
        concat!(
            "watch:0x00112233445566778899aabbccddeeff00112233:",
            "0x0202020202020202020202020202020202020202020202020202020202020202",
        ),
    );
}

#[test]
fn watch_key_round_trips_via_parse() {
    let key = watch_key(&sample_owner(), &sample_hash());
    let watch = WatchRef::parse(&key).expect("parse");
    assert_eq!(
        watch.owner_hex().parse::<Address>().unwrap(),
        sample_owner()
    );
    assert_eq!(watch.hash_hex().parse::<B256>().unwrap(), sample_hash());
    assert_eq!(watch.key(), key);
}

#[test]
fn parse_rejects_missing_prefix_or_separator() {
    assert_eq!(WatchRef::parse("gate:0xaa:0xbb"), None);
    assert_eq!(WatchRef::parse("watch:0xaa0xbb"), None);
    assert_eq!(WatchRef::parse(""), None);
}

#[test]
fn parse_rejects_empty_halves() {
    // `watch::` splits into two empty halves, which would derive
    // degenerate gate keys like `next_block::`; reject it outright.
    assert_eq!(WatchRef::parse("watch::"), None);
    assert_eq!(WatchRef::parse("watch:0xaa:"), None);
    assert_eq!(WatchRef::parse("watch::0xbb"), None);
    // A well-formed key with both halves still parses.
    assert!(WatchRef::parse("watch:0xaa:0xbb").is_some());
}

#[test]
fn parse_preserves_key_substrings_verbatim() {
    // A foreign writer may have cased the hex differently; gate keys
    // must derive from the stored substrings, not from a re-rendered
    // canonical form.
    let watch = WatchRef::parse("watch:0xAABB:0xCCDD").expect("parse");
    assert_eq!(watch.owner_hex(), "0xAABB");
    assert_eq!(watch.hash_hex(), "0xCCDD");
    assert_eq!(watch.next_block_key(), "next_block:0xAABB:0xCCDD");
    assert_eq!(watch.next_epoch_key(), "next_epoch:0xAABB:0xCCDD");
}

// ---- watch-set registry ----

#[test]
fn put_get_list_round_trip() {
    let host = MockHost::new();
    let watches = WatchSet::new(&host);

    let key = watches
        .put(&sample_owner(), &sample_hash(), b"params")
        .unwrap();
    assert_eq!(watches.list().unwrap(), vec![key.clone()]);

    let watch = WatchRef::parse(&key).unwrap();
    assert_eq!(watches.get(watch).unwrap().as_deref(), Some(&b"params"[..]));
}

#[test]
fn put_overwrites_in_place() {
    let host = MockHost::new();
    let watches = WatchSet::new(&host);

    watches
        .put(&sample_owner(), &sample_hash(), b"one")
        .unwrap();
    let key = watches
        .put(&sample_owner(), &sample_hash(), b"two")
        .unwrap();

    assert_eq!(host.store.len(), 1, "re-put must not duplicate the row");
    let watch = WatchRef::parse(&key).unwrap();
    assert_eq!(watches.get(watch).unwrap().as_deref(), Some(&b"two"[..]));
}

#[test]
fn get_absent_watch_is_none() {
    let host = MockHost::new();
    let watches = WatchSet::new(&host);
    let key = watch_key(&sample_owner(), &sample_hash());
    let watch = WatchRef::parse(&key).unwrap();
    assert_eq!(watches.get(watch).unwrap(), None);
}

#[test]
fn list_scans_only_the_watch_prefix() {
    let host = MockHost::new();
    let watches = WatchSet::new(&host);
    let key = watches
        .put(&sample_owner(), &sample_hash(), b"params")
        .unwrap();
    Journal::submitted(&host).record("0xuid").unwrap();

    assert_eq!(watches.list().unwrap(), vec![key]);
}

// ---- atomic delete ----

#[test]
fn remove_drops_watch_and_all_gate_keys() {
    let host = MockHost::new();
    let watches = WatchSet::new(&host);
    let gates = Gates::new(&host);

    let key = watches
        .put(&sample_owner(), &sample_hash(), b"params")
        .unwrap();
    let watch = WatchRef::parse(&key).unwrap();
    gates.set_next_block(watch, 500).unwrap();
    gates.set_next_epoch(watch, 1_700_000_000).unwrap();
    assert_eq!(host.store.len(), 3);

    watches.remove(watch).unwrap();

    assert!(host.store.is_empty(), "watch and both gates must go");
}

#[test]
fn remove_without_gates_is_clean() {
    let host = MockHost::new();
    let watches = WatchSet::new(&host);
    let key = watches
        .put(&sample_owner(), &sample_hash(), b"params")
        .unwrap();
    watches.remove(WatchRef::parse(&key).unwrap()).unwrap();
    assert!(host.store.is_empty());
}

#[test]
fn remove_clears_gates_before_the_watch_row() {
    // A fault on the watch delete must still find the gates gone: the
    // retryable leftover is the watch row, never an orphaned gate.
    let host = MockHost::new();
    let watches = WatchSet::new(&host);
    let gates = Gates::new(&host);
    let key = watches
        .put(&sample_owner(), &sample_hash(), b"params")
        .unwrap();
    let watch = WatchRef::parse(&key).unwrap();
    gates.set_next_block(watch, 500).unwrap();
    gates.set_next_epoch(watch, 1_700_000_000).unwrap();

    host.store
        .fail_on(WATCH_PREFIX, Fault::Unavailable("injected".into()));

    watches.remove(watch).unwrap_err();

    let snapshot = host.store.snapshot();
    assert!(
        !snapshot
            .keys()
            .any(|k| k.starts_with(NEXT_BLOCK_PREFIX) || k.starts_with(NEXT_EPOCH_PREFIX)),
        "gates must already be gone when the watch delete faults",
    );
    assert!(
        snapshot.contains_key(&key),
        "the watch row stays behind so a retry can re-drop it",
    );
}

#[test]
fn remove_propagates_a_gate_delete_fault_and_keeps_the_watch() {
    let host = MockHost::new();
    let watches = WatchSet::new(&host);
    let key = watches
        .put(&sample_owner(), &sample_hash(), b"params")
        .unwrap();
    let watch = WatchRef::parse(&key).unwrap();

    host.store
        .fail_on(NEXT_BLOCK_PREFIX, Fault::Unavailable("injected".into()));

    watches.remove(watch).unwrap_err();

    assert!(
        host.store.snapshot().contains_key(&key),
        "a gate-delete fault must leave the watch for a retry",
    );
}

// ---- gates ----

#[test]
fn ready_with_no_gates_set() {
    let host = MockHost::new();
    let watch = WatchRef::parse("watch:0xaa:0xbb").unwrap();
    assert!(Gates::new(&host).is_ready(watch, 0, 0).unwrap());
}

#[test]
fn next_block_gate_is_inclusive_at_threshold() {
    let host = MockHost::new();
    let gates = Gates::new(&host);
    let watch = WatchRef::parse("watch:0xaa:0xbb").unwrap();
    gates.set_next_block(watch, 500).unwrap();

    assert!(!gates.is_ready(watch, 499, u64::MAX).unwrap());
    assert!(gates.is_ready(watch, 500, u64::MAX).unwrap());
    assert!(gates.is_ready(watch, 501, u64::MAX).unwrap());
}

#[test]
fn next_epoch_gate_is_inclusive_at_threshold() {
    let host = MockHost::new();
    let gates = Gates::new(&host);
    let watch = WatchRef::parse("watch:0xaa:0xbb").unwrap();
    gates.set_next_epoch(watch, 1_700_000_000).unwrap();

    assert!(!gates.is_ready(watch, u64::MAX, 1_699_999_999).unwrap());
    assert!(gates.is_ready(watch, u64::MAX, 1_700_000_000).unwrap());
}

#[test]
fn both_gates_must_pass() {
    let host = MockHost::new();
    let gates = Gates::new(&host);
    let watch = WatchRef::parse("watch:0xaa:0xbb").unwrap();
    gates.set_next_block(watch, 100).unwrap();
    gates.set_next_epoch(watch, 2_000).unwrap();

    assert!(!gates.is_ready(watch, 100, 1_999).unwrap());
    assert!(!gates.is_ready(watch, 99, 2_000).unwrap());
    assert!(gates.is_ready(watch, 100, 2_000).unwrap());
}

#[test]
fn gate_values_are_u64_le() {
    let host = MockHost::new();
    let gates = Gates::new(&host);
    let watch = WatchRef::parse("watch:0xaa:0xbb").unwrap();
    gates.set_next_block(watch, 0x0102_0304_0506_0708).unwrap();

    assert_eq!(
        host.store.snapshot().get("next_block:0xaa:0xbb").unwrap(),
        &0x0102_0304_0506_0708_u64.to_le_bytes().to_vec(),
    );
}

#[test]
fn malformed_gate_value_reads_as_no_gate() {
    let host = MockHost::new();
    let gates = Gates::new(&host);
    let watch = WatchRef::parse("watch:0xaa:0xbb").unwrap();
    host.store.set("next_block:0xaa:0xbb", b"not8b").unwrap();

    assert!(
        gates.is_ready(watch, 0, 0).unwrap(),
        "a corrupt gate can only make the watch poll sooner",
    );
}

#[test]
fn clear_removes_both_gate_keys() {
    let host = MockHost::new();
    let gates = Gates::new(&host);
    let watch = WatchRef::parse("watch:0xaa:0xbb").unwrap();
    gates.set_next_block(watch, 1).unwrap();
    gates.set_next_epoch(watch, 2).unwrap();

    gates.clear(watch).unwrap();

    assert!(host.store.is_empty());
    // And clearing again stays a no-op.
    gates.clear(watch).unwrap();
}

#[test]
fn gate_fault_propagates_from_is_ready() {
    let host = MockHost::new();
    let gates = Gates::new(&host);
    let watch = WatchRef::parse("watch:0xaa:0xbb").unwrap();
    host.store
        .fail_on(NEXT_EPOCH_PREFIX, Fault::Unavailable("injected".into()));

    gates.is_ready(watch, 0, 0).unwrap_err();
}

// ---- journal ----

#[test]
fn journal_round_trips_a_receipt() {
    let host = MockHost::new();
    let journal = Journal::submitted(&host);

    assert!(!journal.contains("0xuid").unwrap());
    journal.record("0xuid").unwrap();
    assert!(journal.contains("0xuid").unwrap());
}

#[test]
fn journal_marker_is_an_empty_presence_row() {
    let host = MockHost::new();
    Journal::submitted(&host).record("0xuid").unwrap();
    assert_eq!(
        host.store.snapshot().get("submitted:0xuid").unwrap(),
        &Vec::<u8>::new(),
    );
}

#[test]
fn journal_record_is_idempotent() {
    let host = MockHost::new();
    let journal = Journal::observed(&host);
    journal.record("0xuid").unwrap();
    journal.record("0xuid").unwrap();
    assert_eq!(host.store.len(), 1);
}

#[test]
fn submitted_and_observed_keyspaces_are_disjoint() {
    let host = MockHost::new();
    Journal::submitted(&host).record("0xuid").unwrap();

    assert!(!Journal::observed(&host).contains("0xuid").unwrap());
    let snapshot = host.store.snapshot();
    assert!(snapshot.contains_key("submitted:0xuid"));
    assert!(!snapshot.contains_key("observed:0xuid"));
}

// ---- retry ledger ----

fn seeded_watch(host: &MockHost) -> String {
    WatchSet::new(host)
        .put(&sample_owner(), &sample_hash(), b"params")
        .unwrap()
}

fn tick_at(block: u64, epoch_s: u64) -> Tick {
    Tick {
        chain_id: 1,
        block,
        epoch_s,
    }
}

#[test]
fn ledger_try_next_block_leaves_the_store_untouched() {
    let host = MockHost::new();
    let key = seeded_watch(&host);
    let before = host.store.snapshot();

    Retrier::new(&host)
        .apply(
            WatchRef::parse(&key).unwrap(),
            RetryAction::TryNextBlock,
            &tick_at(100, 1_000),
        )
        .unwrap();

    assert_eq!(host.store.snapshot(), before);
}

#[test]
fn ledger_backoff_gates_the_watch_on_the_epoch_clock() {
    let host = MockHost::new();
    let key = seeded_watch(&host);
    let watch = WatchRef::parse(&key).unwrap();
    let ledger = Retrier::new(&host);

    ledger
        .apply(
            watch,
            RetryAction::Backoff { seconds: 30 },
            &tick_at(100, 1_000),
        )
        .unwrap();

    let gates = Gates::new(&host);
    assert!(!gates.is_ready(watch, u64::MAX, 1_029).unwrap());
    assert!(gates.is_ready(watch, u64::MAX, 1_030).unwrap());
    assert_eq!(
        host.store.snapshot().get(&watch.next_epoch_key()).unwrap(),
        &1_030_u64.to_le_bytes().to_vec(),
    );
    assert!(
        host.store.snapshot().contains_key(&key),
        "backoff must keep the watch",
    );
}

#[test]
fn ledger_backoff_saturates_on_the_epoch_clock() {
    let host = MockHost::new();
    let key = seeded_watch(&host);
    let watch = WatchRef::parse(&key).unwrap();

    Retrier::new(&host)
        .apply(
            watch,
            RetryAction::Backoff { seconds: u64::MAX },
            &tick_at(100, 1_000),
        )
        .unwrap();

    assert_eq!(
        host.store.snapshot().get(&watch.next_epoch_key()).unwrap(),
        &u64::MAX.to_le_bytes().to_vec(),
    );
}

#[test]
fn ledger_drop_removes_the_watch_and_its_gates() {
    let host = MockHost::new();
    let key = seeded_watch(&host);
    let watch = WatchRef::parse(&key).unwrap();
    Gates::new(&host).set_next_block(watch, 500).unwrap();

    Retrier::new(&host)
        .apply(watch, RetryAction::Drop, &tick_at(100, 1_000))
        .unwrap();

    assert!(host.store.is_empty(), "watch and gates must go");
}

#[test]
fn ledger_drop_on_repeat_grants_one_next_block_retry() {
    let host = MockHost::new();
    let key = seeded_watch(&host);
    let watch = WatchRef::parse(&key).unwrap();
    let ledger = Retrier::new(&host);

    // First refusal: the block is recorded and the watch gates to the
    // next block instead of dropping.
    ledger
        .apply(watch, RetryAction::DropOnRepeat, &tick_at(100, 1_000))
        .unwrap();
    let snapshot = host.store.snapshot();
    assert!(snapshot.contains_key(&key), "first refusal keeps the watch");
    assert_eq!(
        snapshot.get(&watch.refused_key()).unwrap(),
        &100_u64.to_le_bytes().to_vec(),
    );
    assert_eq!(
        snapshot.get(&watch.next_block_key()).unwrap(),
        &101_u64.to_le_bytes().to_vec(),
    );

    // A repeat at the same block leaves the store untouched.
    let before = host.store.snapshot();
    ledger
        .apply(watch, RetryAction::DropOnRepeat, &tick_at(100, 1_000))
        .unwrap();
    assert_eq!(host.store.snapshot(), before);

    // A repeat on a later block removes the watch and every derived key.
    ledger
        .apply(watch, RetryAction::DropOnRepeat, &tick_at(101, 1_012))
        .unwrap();
    assert!(host.store.is_empty(), "watch, gates, and marker must go");
}

#[test]
fn ledger_clear_refusal_resets_the_one_block_grace() {
    let host = MockHost::new();
    let key = seeded_watch(&host);
    let watch = WatchRef::parse(&key).unwrap();
    let ledger = Retrier::new(&host);

    ledger
        .apply(watch, RetryAction::DropOnRepeat, &tick_at(100, 1_000))
        .unwrap();
    ledger.clear_refusal(watch).unwrap();
    assert!(!host.store.snapshot().contains_key(&watch.refused_key()));

    // A refusal at a later block after the clear is a fresh first
    // refusal: the watch survives and the marker records the new block.
    ledger
        .apply(watch, RetryAction::DropOnRepeat, &tick_at(105, 1_060))
        .unwrap();
    let snapshot = host.store.snapshot();
    assert!(snapshot.contains_key(&key), "the watch must survive");
    assert_eq!(
        snapshot.get(&watch.refused_key()).unwrap(),
        &105_u64.to_le_bytes().to_vec(),
    );
    assert_eq!(
        snapshot.get(&watch.next_block_key()).unwrap(),
        &106_u64.to_le_bytes().to_vec(),
    );
}

#[test]
fn ledger_drop_removes_the_refused_marker() {
    let host = MockHost::new();
    let key = seeded_watch(&host);
    let watch = WatchRef::parse(&key).unwrap();

    let ledger = Retrier::new(&host);
    ledger
        .apply(watch, RetryAction::DropOnRepeat, &tick_at(100, 1_000))
        .unwrap();
    ledger
        .apply(watch, RetryAction::Drop, &tick_at(100, 1_000))
        .unwrap();

    assert!(
        !host
            .store
            .snapshot()
            .keys()
            .any(|k| k.starts_with(REFUSED_PREFIX)),
        "drop must not orphan the refusal marker",
    );
}

#[test]
fn retry_action_labels_are_stable_snake_case() {
    let cases: [(RetryAction, &str); 4] = [
        (RetryAction::TryNextBlock, "try_next_block"),
        (RetryAction::Backoff { seconds: 1 }, "backoff"),
        (RetryAction::DropOnRepeat, "drop_on_repeat"),
        (RetryAction::Drop, "drop"),
    ];
    for (action, label) in cases {
        assert_eq!(<&'static str>::from(action), label);
    }
}

// ---- conditional source ----

/// A source is generic over the host and owns its outcome shape; the
/// keeper passes the stored params verbatim and the tick it judged
/// the gates by.
#[test]
fn conditional_source_sees_params_and_tick_verbatim() {
    struct EchoSource;
    impl<H> ConditionalSource<H> for EchoSource {
        type Outcome = (usize, u64, u64, u64, String);
        fn poll(
            &self,
            _host: &H,
            watch: WatchRef<'_>,
            params: &[u8],
            tick: &Tick,
        ) -> Self::Outcome {
            (
                params.len(),
                tick.chain_id,
                tick.block,
                tick.epoch_s,
                watch.key(),
            )
        }
    }

    let host = MockHost::new();
    let key = seeded_watch(&host);
    let watch = WatchRef::parse(&key).unwrap();
    let tick = Tick {
        chain_id: 1,
        block: 42,
        epoch_s: 1_700_000_000,
    };

    let (len, chain_id, block, epoch_s, echoed) = EchoSource.poll(&host, watch, b"params", &tick);
    assert_eq!(len, b"params".len());
    assert_eq!(chain_id, 1);
    assert_eq!(block, 42);
    assert_eq!(epoch_s, 1_700_000_000);
    assert_eq!(echoed, key);
}
