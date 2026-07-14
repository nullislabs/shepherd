//! Keeper-store acceptance tests against the composed CoW
//! `shepherd_sdk_test::MockHost` - the same host the flagship modules
//! test with. These live as an integration test (not `#[cfg(test)]`)
//! because the mock crate links `nexum-sdk` externally, and the
//! external and unit-test copies of the host traits are distinct types.

use alloy_primitives::{Address, B256, address, b256};
use nexum_sdk::host::{Fault, LocalStoreHost as _};
use nexum_sdk::keeper::{
    Gates, Journal, NEXT_BLOCK_PREFIX, NEXT_EPOCH_PREFIX, WATCH_PREFIX, WatchRef, WatchSet,
};
use shepherd_sdk_test::MockHost;

fn sample_owner() -> Address {
    address!("00112233445566778899aabbccddeeff00112233")
}

fn sample_hash() -> B256 {
    b256!("0202020202020202020202020202020202020202020202020202020202020202")
}

// ---- watch keys ----

#[test]
fn watch_key_is_lowercase_prefixed_hex() {
    let key = WatchSet::<MockHost>::key(&sample_owner(), &sample_hash());
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
    let key = WatchSet::<MockHost>::key(&sample_owner(), &sample_hash());
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
    let key = WatchSet::<MockHost>::key(&sample_owner(), &sample_hash());
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
