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

// ---------------------------------------------------------------------------
// Concurrent access tests
// ---------------------------------------------------------------------------

#[test]
fn concurrent_writes_from_different_namespaces() {
    let (_dir, store) = fresh();

    let handles: Vec<_> = (0..8)
        .map(|i| {
            let s = store.clone();
            std::thread::spawn(move || {
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
        h.join().expect("thread panicked");
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

#[test]
fn concurrent_reads_during_writes() {
    let (_dir, store) = fresh();
    let ms = store.module("rw").unwrap();

    // Pre-populate namespace "rw" with 50 keys.
    for j in 0..50 {
        ms.set(&format!("k-{j}"), b"old").unwrap();
    }

    let writer_ms = ms.clone();
    let writer = std::thread::spawn(move || {
        for j in 0..50 {
            writer_ms.set(&format!("k-{j}"), b"new").unwrap();
        }
    });

    let readers: Vec<_> = (0..4)
        .map(|_| {
            let reader_ms = ms.clone();
            std::thread::spawn(move || {
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

    writer.join().expect("writer panicked");
    for r in readers {
        r.join().expect("reader panicked");
    }

    // Final state: all keys must be "new".
    for j in 0..50 {
        assert_eq!(
            ms.get(&format!("k-{j}")).unwrap().as_deref(),
            Some(&b"new"[..]),
        );
    }
}

#[test]
fn list_keys_races_with_delete() {
    let (_dir, store) = fresh();
    let ms = store.module("race").unwrap();

    // Pre-populate namespace "race" with 100 keys.
    for i in 0..100 {
        ms.set(&format!("k:{i}"), b"x").unwrap();
    }

    let deleter_ms = ms.clone();
    let deleter = std::thread::spawn(move || {
        for i in 0..100 {
            deleter_ms.delete(&format!("k:{i}")).unwrap();
        }
    });

    let lister_ms = ms.clone();
    let lister = std::thread::spawn(move || {
        for _ in 0..50 {
            let keys = lister_ms.list_keys("k:").unwrap();
            assert!(
                keys.len() <= 100,
                "list_keys returned more keys than expected: {}",
                keys.len(),
            );
        }
    });

    deleter.join().expect("deleter panicked");
    lister.join().expect("lister panicked");
}

#[test]
fn stress_many_writers_one_namespace() {
    let (_dir, store) = fresh();
    let ms = store.module("shared").unwrap();

    let handles: Vec<_> = (0..8)
        .map(|i| {
            let ms = ms.clone();
            std::thread::spawn(move || {
                for j in 0..100 {
                    let key = format!("t{i}-k{j}");
                    let val = format!("v-{i}-{j}").into_bytes();
                    ms.set(&key, &val).unwrap();
                }
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread panicked");
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
