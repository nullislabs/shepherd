//! In-memory [`StateStore`] fake: per-namespace `HashMap`, no redb, no disk.

// StorageError embeds redb error types; same allowance as the seam it mirrors.
#![allow(clippy::result_large_err)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::host::component::{StateHandle, StateStore};
use crate::host::local_store_redb::{MAX_APPLY_OPS, MAX_APPLY_VALUE_BYTES, StorageError, WriteOp};

type Namespaces = HashMap<String, HashMap<String, Vec<u8>>>;

/// Process-lifetime in-memory store keyed by namespace then key. Cheap `Arc`
/// clone shares one backing map, so a test keeps a clone to assert on what a
/// module wrote.
#[derive(Clone, Default)]
pub struct MockStateStore {
    namespaces: Arc<Mutex<Namespaces>>,
}

impl MockStateStore {
    /// Fresh empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Per-module handle over the shared map, scoped to one namespace.
#[derive(Clone)]
pub struct MockStateHandle {
    namespaces: Arc<Mutex<Namespaces>>,
    namespace: String,
    quota_bytes: Option<u64>,
}

impl StateStore for MockStateStore {
    type Handle = MockStateHandle;

    fn module(&self, namespace: &str) -> Result<MockStateHandle, StorageError> {
        // Reject the empty namespace so the handle always has a real prefix,
        // matching the redb-backed store.
        if namespace.is_empty() {
            return Err(StorageError::InvalidNamespace(
                "module namespace must not be empty".into(),
            ));
        }
        Ok(MockStateHandle {
            namespaces: Arc::clone(&self.namespaces),
            namespace: namespace.to_owned(),
            quota_bytes: None,
        })
    }
}

impl MockStateHandle {
    fn lock(&self) -> std::sync::MutexGuard<'_, Namespaces> {
        self.namespaces.lock().expect("mock store mutex poisoned")
    }
}

impl StateHandle for MockStateHandle {
    fn with_quota(mut self, quota_bytes: u64) -> Self {
        self.quota_bytes = Some(quota_bytes);
        self
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
        Ok(self
            .lock()
            .get(&self.namespace)
            .and_then(|m| m.get(key))
            .cloned())
    }

    fn set(&self, key: &str, value: &[u8]) -> Result<(), StorageError> {
        let mut map = self.lock();
        let ns = map.entry(self.namespace.clone()).or_default();
        if let Some(quota) = self.quota_bytes {
            let entry = (key.len() + value.len()) as u64;
            let old = ns
                .get(key)
                .map(|v| (key.len() + v.len()) as u64)
                .unwrap_or(0);
            let used: u64 = ns.iter().map(|(k, v)| (k.len() + v.len()) as u64).sum();
            let projected = used.saturating_sub(old) + entry;
            if projected > quota {
                return Err(StorageError::QuotaExceeded {
                    needed: projected,
                    quota,
                });
            }
        }
        ns.insert(key.to_owned(), value.to_vec());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), StorageError> {
        if let Some(m) = self.lock().get_mut(&self.namespace) {
            m.remove(key);
        }
        Ok(())
    }

    fn list_keys(&self, prefix: &str) -> Result<Vec<String>, StorageError> {
        let map = self.lock();
        let mut keys: Vec<String> = map
            .get(&self.namespace)
            .into_iter()
            .flat_map(|m| m.keys())
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect();
        // Sorted for deterministic enumeration, matching the redb B-tree order.
        keys.sort();
        Ok(keys)
    }

    fn apply(&self, ops: &[WriteOp]) -> Result<(), StorageError> {
        if ops.len() > MAX_APPLY_OPS {
            return Err(StorageError::ApplyOpsExceeded {
                ops: ops.len(),
                cap: MAX_APPLY_OPS,
            });
        }
        let value_bytes: u64 = ops
            .iter()
            .map(|op| match op {
                WriteOp::Set { value, .. } => value.len() as u64,
                WriteOp::Delete { .. } => 0,
            })
            .sum();
        if value_bytes > MAX_APPLY_VALUE_BYTES {
            return Err(StorageError::ApplyBytesExceeded {
                bytes: value_bytes,
                cap: MAX_APPLY_VALUE_BYTES,
            });
        }
        let mut map = self.lock();
        let ns = map.entry(self.namespace.clone()).or_default();
        // Net whole-batch projection, checked once before any mutation so
        // an over-quota batch lands nothing (the map mirrors one txn).
        if let Some(quota) = self.quota_bytes {
            let mut finals: HashMap<&str, Option<usize>> = HashMap::new();
            for op in ops {
                match op {
                    WriteOp::Set { key, value } => finals.insert(key, Some(value.len())),
                    WriteOp::Delete { key } => finals.insert(key, None),
                };
            }
            let used: u64 = ns.iter().map(|(k, v)| (k.len() + v.len()) as u64).sum();
            let mut released = 0u64;
            let mut charged = 0u64;
            for (key, value_len) in &finals {
                released += ns
                    .get(*key)
                    .map(|v| (key.len() + v.len()) as u64)
                    .unwrap_or(0);
                charged += value_len.map(|len| (key.len() + len) as u64).unwrap_or(0);
            }
            let projected = used.saturating_sub(released) + charged;
            if projected > quota {
                return Err(StorageError::QuotaExceeded {
                    needed: projected,
                    quota,
                });
            }
        }
        for op in ops {
            match op {
                WriteOp::Set { key, value } => {
                    ns.insert(key.clone(), value.clone());
                }
                WriteOp::Delete { key } => {
                    ns.remove(key);
                }
            }
        }
        Ok(())
    }
}
