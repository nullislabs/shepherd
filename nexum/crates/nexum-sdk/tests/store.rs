//! Store-helper acceptance tests over the world-neutral mock local
//! store, which exercises the trait's default per-op `apply` fallback.

use borsh::{BorshDeserialize, BorshSerialize};
use nexum_sdk::host::{Fault, LocalStoreHost};
use nexum_sdk::store::{Counter, TypedCell, TypedMap, WriteBatch, clear_prefix};
use nexum_sdk_test::MockLocalStore;

#[derive(BorshSerialize, BorshDeserialize, Debug, Eq, PartialEq)]
struct Row {
    label: String,
    size: u64,
}

fn row(label: &str, size: u64) -> Row {
    Row {
        label: label.to_owned(),
        size,
    }
}

#[test]
fn write_batch_flushes_staged_sets_and_deletes() {
    let store = MockLocalStore::default();
    store.set("stale", b"x").unwrap();

    let mut batch = WriteBatch::new(&store);
    batch.set("a", b"1".to_vec()).set("b", b"2".to_vec());
    batch.delete("stale");
    assert_eq!(batch.len(), 3);
    assert!(!batch.is_empty());
    batch.flush().unwrap();

    assert_eq!(store.get("a").unwrap(), Some(b"1".to_vec()));
    assert_eq!(store.get("b").unwrap(), Some(b"2".to_vec()));
    assert_eq!(store.get("stale").unwrap(), None);
}

#[test]
fn write_batch_dropped_before_flush_writes_nothing() {
    let store = MockLocalStore::default();
    let mut batch = WriteBatch::new(&store);
    batch.set("a", b"1".to_vec());
    drop(batch);
    assert!(store.is_empty());
}

#[test]
fn write_batch_empty_flush_is_a_no_op() {
    let store = MockLocalStore::default();
    WriteBatch::new(&store).flush().unwrap();
    assert!(store.is_empty());
}

#[test]
fn default_apply_fallback_is_per_op_so_a_mid_batch_fault_leaves_earlier_ops() {
    let store = MockLocalStore::default();
    store.fail_on("poison", Fault::Internal("injected".into()));

    let mut batch = WriteBatch::new(&store);
    batch.set("a", b"1".to_vec());
    batch.set("poison", b"2".to_vec());
    batch.set("z", b"3".to_vec());
    batch.flush().unwrap_err();

    // The mock exercises the trait default, so the pre-fault op landed
    // and the post-fault op did not.
    assert_eq!(store.get("a").unwrap(), Some(b"1".to_vec()));
    assert_eq!(store.get("z").unwrap(), None);
}

#[test]
fn clear_prefix_deletes_only_the_prefix_and_counts() {
    let store = MockLocalStore::default();
    store.set("watch:a", b"1").unwrap();
    store.set("watch:b", b"2").unwrap();
    store.set("gate:a", b"3").unwrap();

    assert_eq!(clear_prefix(&store, "watch:").unwrap(), 2);
    assert_eq!(store.len(), 1);
    assert_eq!(store.get("gate:a").unwrap(), Some(b"3".to_vec()));
    assert_eq!(clear_prefix(&store, "watch:").unwrap(), 0);
}

#[test]
fn typed_cell_round_trips_and_clears() {
    let store = MockLocalStore::default();
    let cell: TypedCell<'_, _, Row> = TypedCell::new(&store, "cursor");

    assert_eq!(cell.get().unwrap(), None);
    cell.set(&row("head", 7)).unwrap();
    assert_eq!(cell.get().unwrap(), Some(row("head", 7)));
    cell.clear().unwrap();
    assert_eq!(cell.get().unwrap(), None);
}

#[test]
fn typed_cell_folds_a_corrupt_value_to_internal() {
    let store = MockLocalStore::default();
    store.set("cursor", b"not borsh").unwrap();
    let cell: TypedCell<'_, _, Row> = TypedCell::new(&store, "cursor");
    assert!(matches!(cell.get().unwrap_err(), Fault::Internal(_)));
}

#[test]
fn typed_map_inserts_gets_removes_and_strips_prefix_from_keys() {
    let store = MockLocalStore::default();
    store.set("other", b"x").unwrap();
    let map: TypedMap<'_, _, Row> = TypedMap::new(&store, "order:");

    assert_eq!(map.keys().unwrap(), Vec::<String>::new());
    map.insert("a", &row("first", 1)).unwrap();
    map.insert("b", &row("second", 2)).unwrap();
    assert_eq!(map.get("a").unwrap(), Some(row("first", 1)));
    assert_eq!(map.get("missing").unwrap(), None);
    assert_eq!(map.keys().unwrap(), vec!["a".to_owned(), "b".to_owned()]);

    map.remove("a").unwrap();
    assert_eq!(map.get("a").unwrap(), None);
    assert_eq!(map.clear().unwrap(), 1);
    assert!(map.keys().unwrap().is_empty());
    assert_eq!(store.get("other").unwrap(), Some(b"x".to_vec()));
}

#[test]
fn counter_defaults_to_zero_adds_and_saturates() {
    let store = MockLocalStore::default();
    let counter = Counter::new(&store, "submitted");

    assert_eq!(counter.get().unwrap(), 0);
    assert_eq!(counter.add(2).unwrap(), 2);
    assert_eq!(counter.add(3).unwrap(), 5);
    counter.set(u64::MAX).unwrap();
    assert_eq!(counter.add(1).unwrap(), u64::MAX);
}
