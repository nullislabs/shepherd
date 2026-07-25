use super::*;

fn fresh() -> (tempfile::TempDir, LocalStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LocalStore::open(dir.path().join("ls.redb")).expect("open");
    (dir, store)
}

#[test]
fn set_get_roundtrip() {
    let (_dir, store) = fresh();
    let ms = store.module("twap").unwrap();
    ms.set("k", b"v").unwrap();
    assert_eq!(ms.get("k").unwrap().as_deref(), Some(&b"v"[..]));
}

// A committed write survives dropping every handle and reopening the file:
// each `set` is its own fsync-durable txn, so a shutdown after it returns
// cannot lose it.
#[test]
fn committed_write_survives_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ls.redb");
    {
        let store = LocalStore::open(&path).expect("open");
        let ms = store.module("twap").unwrap();
        ms.set("cursor", b"42").unwrap();
        ms.delete("stale").unwrap();
        // Drop every handle (the `Arc<Database>` flushes on close).
    }
    let store = LocalStore::open(&path).expect("reopen");
    let ms = store.module("twap").unwrap();
    assert_eq!(ms.get("cursor").unwrap().as_deref(), Some(&b"42"[..]));
    assert!(ms.get("stale").unwrap().is_none());
}

#[test]
fn namespaces_isolate_modules() {
    let (_dir, store) = fresh();
    let a = store.module("a").unwrap();
    let b = store.module("b").unwrap();
    a.set("k", b"from-a").unwrap();
    b.set("k", b"from-b").unwrap();
    assert_eq!(a.get("k").unwrap().as_deref(), Some(&b"from-a"[..]));
    assert_eq!(b.get("k").unwrap().as_deref(), Some(&b"from-b"[..]));
}

#[test]
fn delete_then_get_is_none() {
    let (_dir, store) = fresh();
    let ms = store.module("twap").unwrap();
    ms.set("k", b"v").unwrap();
    ms.delete("k").unwrap();
    assert!(ms.get("k").unwrap().is_none());
}

#[test]
fn list_keys_strips_namespace_prefix() {
    let (_dir, store) = fresh();
    let ms = store.module("twap").unwrap();
    ms.set("posted:1", b"x").unwrap();
    ms.set("posted:2", b"y").unwrap();
    ms.set("other", b"z").unwrap();
    let keys = ms.list_keys("posted:").unwrap();
    assert_eq!(keys.len(), 2);
    assert!(keys.iter().all(|k| k.starts_with("posted:")));
}

#[test]
fn contains_answers_without_the_value() {
    let (_dir, store) = fresh();
    let ms = store.module("twap").unwrap();
    ms.set("k", b"v").unwrap();
    assert!(ms.contains("k").unwrap());
    assert!(!ms.contains("missing").unwrap());
    ms.delete("k").unwrap();
    assert!(!ms.contains("k").unwrap());
}

#[test]
fn len_reports_value_bytes_or_none() {
    let (_dir, store) = fresh();
    let ms = store.module("twap").unwrap();
    ms.set("empty", b"").unwrap();
    ms.set("k", b"abcde").unwrap();
    assert_eq!(ms.len("empty").unwrap(), Some(0));
    assert_eq!(ms.len("k").unwrap(), Some(5));
    assert_eq!(ms.len("missing").unwrap(), None);
}

#[test]
fn count_matches_list_keys_and_respects_namespaces() {
    let (_dir, store) = fresh();
    let a = store.module("a").unwrap();
    let b = store.module("b").unwrap();
    a.set("posted:1", b"x").unwrap();
    a.set("posted:2", b"y").unwrap();
    a.set("other", b"z").unwrap();
    b.set("posted:9", b"w").unwrap();
    assert_eq!(a.count("posted:").unwrap(), 2);
    assert_eq!(a.count("").unwrap(), 3);
    assert_eq!(a.count("nope:").unwrap(), 0);
    assert_eq!(b.count("posted:").unwrap(), 1);
    assert_eq!(
        a.count("posted:").unwrap(),
        a.list_keys("posted:").unwrap().len() as u64
    );
}

#[test]
fn rejects_empty_namespace() {
    let (_dir, store) = fresh();
    let err = store.module("").unwrap_err();
    assert!(matches!(err, StorageError::InvalidNamespace(_)));
}

#[test]
fn prefix_is_fixed_32_bytes() {
    let short = store_prefix("a");
    let long = store_prefix(&"a".repeat(300));
    assert_eq!(short.len(), PREFIX_LEN);
    assert_eq!(long.len(), PREFIX_LEN);
    // Different inputs produce different prefixes.
    assert_ne!(short, long);
}

#[test]
fn prefix_is_deterministic() {
    let p1 = store_prefix("twap-monitor");
    let p2 = store_prefix("twap-monitor");
    assert_eq!(p1, p2);
}

#[test]
fn similar_names_differ() {
    // Verify that names that share a common prefix don't collide.
    let pa = store_prefix("module-a");
    let pb = store_prefix("module-b");
    assert_ne!(pa, pb);
}

#[test]
fn module_handles_share_underlying_data() {
    let (_dir, store) = fresh();
    let ms1 = store.module("twap").unwrap();
    let ms2 = ms1.clone();
    ms1.set("k", b"v").unwrap();
    assert_eq!(ms2.get("k").unwrap().as_deref(), Some(&b"v"[..]));
}

/// Helper: compute the prefix a ModuleStore would use for `name`.
fn store_prefix(name: &str) -> Vec<u8> {
    keccak256(name.as_bytes()).to_vec()
}

/// On-disk cost the quota charges for one entry: prefix + overhead + key +
/// value.
fn cost(key: &str, val: &[u8]) -> u64 {
    (PREFIX_LEN + key.len() + val.len()) as u64 + ENTRY_OVERHEAD
}

#[test]
fn default_handle_has_no_quota() {
    let (_dir, store) = fresh();
    let ms = store.module("m").unwrap();
    // Comfortably larger than any per-module quota; the default is unlimited.
    ms.set("k", &vec![0u8; 4096]).unwrap();
    assert_eq!(ms.get("k").unwrap().map(|v| v.len()), Some(4096));
}

#[test]
fn quota_rejects_over_budget_write_leaving_store_unchanged() {
    let (_dir, store) = fresh();
    // Quota sized so the first entry exactly fits its on-disk cost.
    let quota = cost("key", b"fits!");
    let ms = store.module("m").unwrap().with_quota(quota);
    ms.set("key", b"fits!").unwrap();

    // A second, distinct key pushes the footprint past the quota.
    let expected = quota + cost("k2", b"nope");
    let err = ms.set("k2", b"nope").unwrap_err();
    match err {
        StorageError::QuotaExceeded { needed, quota: q } => {
            assert_eq!(needed, expected);
            assert_eq!(q, quota);
        }
        other => panic!("expected QuotaExceeded, got {other:?}"),
    }
    // The rejected write must not have landed.
    assert!(ms.get("k2").unwrap().is_none());
    assert_eq!(ms.get("key").unwrap().as_deref(), Some(&b"fits!"[..]));
}

#[test]
fn quota_rejects_single_oversize_value() {
    let (_dir, store) = fresh();
    let ms = store.module("m").unwrap().with_quota(4);
    let err = ms.set("k", b"toolong").unwrap_err();
    assert!(matches!(err, StorageError::QuotaExceeded { .. }));
    assert!(ms.get("k").unwrap().is_none());
}

#[test]
fn quota_overwrite_releases_previous_bytes() {
    let (_dir, store) = fresh();
    // Room for two small entries; a large "k" plus "j" would not fit unless
    // the overwrite releases the old value first.
    let quota = cost("k", b"bb") + cost("j", b"cc");
    let ms = store.module("m").unwrap().with_quota(quota);
    ms.set("k", b"aaaaa").unwrap();
    // Overwriting releases the old bytes first, so a smaller value fits.
    ms.set("k", b"bb").unwrap();
    assert_eq!(ms.get("k").unwrap().as_deref(), Some(&b"bb"[..]));
    // A fresh key now fits in the freed budget.
    ms.set("j", b"cc").unwrap();
    assert_eq!(ms.get("j").unwrap().as_deref(), Some(&b"cc"[..]));
}

#[test]
fn quota_is_released_by_delete() {
    let (_dir, store) = fresh();
    let ms = store.module("m").unwrap().with_quota(cost("key", b"fits!"));
    ms.set("key", b"fits!").unwrap();
    assert!(ms.set("k2", b"nope").is_err());
    ms.delete("key").unwrap();
    // With the namespace emptied, the previously rejected write fits.
    ms.set("k2", b"nope").unwrap();
    assert_eq!(ms.get("k2").unwrap().as_deref(), Some(&b"nope"[..]));
}

#[test]
fn quota_counts_across_short_lived_handles_of_one_namespace() {
    let (_dir, store) = fresh();
    // Distinct handles for the same namespace share the footprint: a write
    // through a second quota handle sees the first handle's bytes.
    store
        .module("m")
        .unwrap()
        .with_quota(cost("a", b"1234") + cost("b", b"5678"))
        .set("a", b"1234")
        .unwrap();
    let err = store
        .module("m")
        .unwrap()
        .with_quota(8)
        .set("b", b"5678")
        .unwrap_err();
    assert!(matches!(err, StorageError::QuotaExceeded { .. }));
}

// ---------------------------------------------------------------------------
// Atomic apply batches (#609).
// ---------------------------------------------------------------------------

fn set_op(key: &str, value: &[u8]) -> WriteOp {
    WriteOp::Set {
        key: key.into(),
        value: value.to_vec(),
    }
}

fn delete_op(key: &str) -> WriteOp {
    WriteOp::Delete { key: key.into() }
}

#[test]
fn apply_mixed_batch_commits_atomically() {
    let (_dir, store) = fresh();
    let ms = store.module("m").unwrap();
    ms.set("stale", b"old").unwrap();
    ms.set("keep", b"as-is").unwrap();
    ms.apply(&[
        set_op("fresh", b"new"),
        set_op("stale", b"overwritten"),
        delete_op("keep"),
        delete_op("missing"),
    ])
    .unwrap();
    assert_eq!(ms.get("fresh").unwrap().as_deref(), Some(&b"new"[..]));
    assert_eq!(
        ms.get("stale").unwrap().as_deref(),
        Some(&b"overwritten"[..])
    );
    assert!(ms.get("keep").unwrap().is_none());
    assert!(ms.get("missing").unwrap().is_none());
}

#[test]
fn apply_over_quota_batch_lands_nothing() {
    let (_dir, store) = fresh();
    // Room for the seeded entry plus one small one, but not the batch's two.
    let quota = cost("seed", b"v") + cost("a", b"1");
    let ms = store.module("m").unwrap().with_quota(quota);
    ms.set("seed", b"v").unwrap();
    let err = ms
        .apply(&[set_op("a", b"1"), set_op("b", b"2")])
        .unwrap_err();
    match err {
        StorageError::QuotaExceeded { needed, quota: q } => {
            assert_eq!(needed, quota + cost("b", b"2"));
            assert_eq!(q, quota);
        }
        other => panic!("expected QuotaExceeded, got {other:?}"),
    }
    // All-or-nothing: even the op that fit on its own must not have landed.
    assert!(ms.get("a").unwrap().is_none());
    assert!(ms.get("b").unwrap().is_none());
    assert_eq!(ms.get("seed").unwrap().as_deref(), Some(&b"v"[..]));
}

#[test]
fn apply_over_op_count_batch_rejected_untouched() {
    let (_dir, store) = fresh();
    let ms = store.module("m").unwrap();
    let ops: Vec<WriteOp> = (0..=MAX_APPLY_OPS)
        .map(|i| set_op(&format!("k{i}"), b"v"))
        .collect();
    let err = ms.apply(&ops).unwrap_err();
    match err {
        StorageError::ApplyOpsExceeded { ops: n, cap } => {
            assert_eq!(n, MAX_APPLY_OPS + 1);
            assert_eq!(cap, MAX_APPLY_OPS);
        }
        other => panic!("expected ApplyOpsExceeded, got {other:?}"),
    }
    assert!(ms.get("k0").unwrap().is_none());
    assert_eq!(ms.count("").unwrap(), 0);
}

#[test]
fn apply_over_value_bytes_batch_rejected_untouched() {
    let (_dir, store) = fresh();
    let ms = store.module("m").unwrap();
    let big = vec![0u8; MAX_APPLY_VALUE_BYTES as usize];
    let err = ms
        .apply(&[set_op("a", &big), set_op("b", b"1")])
        .unwrap_err();
    match err {
        StorageError::ApplyBytesExceeded { bytes, cap } => {
            assert_eq!(bytes, MAX_APPLY_VALUE_BYTES + 1);
            assert_eq!(cap, MAX_APPLY_VALUE_BYTES);
        }
        other => panic!("expected ApplyBytesExceeded, got {other:?}"),
    }
    assert!(ms.get("a").unwrap().is_none());
    assert!(ms.get("b").unwrap().is_none());
}

#[test]
fn apply_quota_charges_net_batch_footprint() {
    let (_dir, store) = fresh();
    // Quota holds exactly one entry: the set alone would bust it, but the
    // batch's delete releases the seeded bytes first, so the net fits.
    let quota = cost("old", b"12345");
    let ms = store.module("m").unwrap().with_quota(quota);
    ms.set("old", b"12345").unwrap();
    assert!(ms.set("new", b"12345").is_err());
    ms.apply(&[delete_op("old"), set_op("new", b"12345")])
        .unwrap();
    assert!(ms.get("old").unwrap().is_none());
    assert_eq!(ms.get("new").unwrap().as_deref(), Some(&b"12345"[..]));
    // The counter carried the net footprint: a refill of the freed slot fits.
    ms.apply(&[delete_op("new"), set_op("old", b"12345")])
        .unwrap();
}

#[test]
fn apply_quota_projects_the_last_op_per_key() {
    let (_dir, store) = fresh();
    // The oversized first write on "k" is superseded within the batch; only
    // the final small value is charged, so the batch fits a tight quota.
    let quota = cost("k", b"ok");
    let ms = store.module("m").unwrap().with_quota(quota);
    ms.apply(&[set_op("k", &vec![0u8; 1024]), set_op("k", b"ok")])
        .unwrap();
    assert_eq!(ms.get("k").unwrap().as_deref(), Some(&b"ok"[..]));
}

// ---------------------------------------------------------------------------
// Concurrent access tests: real parallelism via the blocking pool.
// ---------------------------------------------------------------------------

fn blocking_executor() -> nexum_tasks::TaskExecutor {
    nexum_tasks::TaskManager::new().executor()
}

#[tokio::test]
async fn concurrent_writes_from_different_namespaces() {
    let (_dir, store) = fresh();
    let executor = blocking_executor();

    let handles: Vec<_> = (0..8)
        .map(|i| {
            let s = store.clone();
            executor.spawn_blocking(move || {
                let ms = s.module(&format!("ns-{i}")).unwrap();
                for j in 0..100 {
                    let key = format!("key-{j}");
                    let val = format!("val-{i}-{j}").into_bytes();
                    ms.set(&key, &val).unwrap();
                }
            })
        })
        .collect();

    for h in handles {
        h.join().await.expect("writer task panicked");
    }

    for i in 0..8 {
        let ms = store.module(&format!("ns-{i}")).unwrap();
        for j in 0..100 {
            let key = format!("key-{j}");
            let expected = format!("val-{i}-{j}").into_bytes();
            assert_eq!(ms.get(&key).unwrap().as_deref(), Some(expected.as_slice()),);
        }
    }
}

#[tokio::test]
async fn concurrent_reads_during_writes() {
    let (_dir, store) = fresh();
    let ms = store.module("rw").unwrap();
    let executor = blocking_executor();

    // Pre-populate namespace "rw" with 50 keys.
    for j in 0..50 {
        ms.set(&format!("k-{j}"), b"old").unwrap();
    }

    let writer_ms = ms.clone();
    let writer = executor.spawn_blocking(move || {
        for j in 0..50 {
            writer_ms.set(&format!("k-{j}"), b"new").unwrap();
        }
    });

    let readers: Vec<_> = (0..4)
        .map(|_| {
            let reader_ms = ms.clone();
            executor.spawn_blocking(move || {
                for _ in 0..100 {
                    for j in 0..50 {
                        let val = reader_ms.get(&format!("k-{j}")).unwrap();
                        let val = val.expect("key must exist");
                        assert!(
                            val == b"old" || val == b"new",
                            "unexpected value: {:?}",
                            val,
                        );
                    }
                }
            })
        })
        .collect();

    writer.join().await.expect("writer panicked");
    for r in readers {
        r.join().await.expect("reader panicked");
    }

    // Final state: all keys must be "new".
    for j in 0..50 {
        assert_eq!(
            ms.get(&format!("k-{j}")).unwrap().as_deref(),
            Some(&b"new"[..]),
        );
    }
}

#[tokio::test]
async fn list_keys_races_with_delete() {
    let (_dir, store) = fresh();
    let ms = store.module("race").unwrap();
    let executor = blocking_executor();

    // Pre-populate namespace "race" with 100 keys.
    for i in 0..100 {
        ms.set(&format!("k:{i}"), b"x").unwrap();
    }

    let deleter_ms = ms.clone();
    let deleter = executor.spawn_blocking(move || {
        for i in 0..100 {
            deleter_ms.delete(&format!("k:{i}")).unwrap();
        }
    });

    let lister_ms = ms.clone();
    let lister = executor.spawn_blocking(move || {
        for _ in 0..50 {
            let keys = lister_ms.list_keys("k:").unwrap();
            assert!(
                keys.len() <= 100,
                "list_keys returned more keys than expected: {}",
                keys.len(),
            );
        }
    });

    deleter.join().await.expect("deleter panicked");
    lister.join().await.expect("lister panicked");
}

#[tokio::test]
async fn stress_many_writers_one_namespace() {
    let (_dir, store) = fresh();
    let ms = store.module("shared").unwrap();
    let executor = blocking_executor();

    let handles: Vec<_> = (0..8)
        .map(|i| {
            let ms = ms.clone();
            executor.spawn_blocking(move || {
                for j in 0..100 {
                    let key = format!("t{i}-k{j}");
                    let val = format!("v-{i}-{j}").into_bytes();
                    ms.set(&key, &val).unwrap();
                }
            })
        })
        .collect();

    for h in handles {
        h.join().await.expect("writer task panicked");
    }

    // Verify all 800 keys are present with correct values.
    for i in 0..8 {
        for j in 0..100 {
            let key = format!("t{i}-k{j}");
            let expected = format!("v-{i}-{j}").into_bytes();
            assert_eq!(ms.get(&key).unwrap().as_deref(), Some(expected.as_slice()),);
        }
    }
}
