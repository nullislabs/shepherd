//! In-memory [`StateStore`] fake: per-namespace `HashMap`, no redb, no disk.

// StorageError embeds redb error types; same allowance as the seam it mirrors.
#![allow(clippy::result_large_err)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::host::component::{StateHandle, StateStore};
use crate::host::local_store_redb::StorageError;

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
}
