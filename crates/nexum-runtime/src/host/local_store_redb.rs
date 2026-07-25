//! `nexum:host/local-store` backend: a single redb file under
//! `EngineConfig.engine.state_dir`.
//!
//! Every key is prefixed host-side by `keccak256(module_name)`, so modules
//! sharing a key string see disjoint data and cannot forge into another's
//! range.

#![allow(clippy::result_large_err)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use alloy_primitives::keccak256;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use thiserror::Error;

const TABLE: TableDefinition<'static, &[u8], &[u8]> = TableDefinition::new("nexum:local-store");
#[cfg(test)]
const PREFIX_LEN: usize = 32;

/// Fixed per-entry overhead charged with prefix+key+value so the quota bounds
/// on-disk bytes, not logical payload.
const ENTRY_OVERHEAD: u64 = 32;

/// Cap on ops per [`ModuleStore::apply`] batch.
pub const MAX_APPLY_OPS: usize = 1024;

/// Cap on total set-value bytes per [`ModuleStore::apply`] batch.
pub const MAX_APPLY_VALUE_BYTES: u64 = 4 * 1024 * 1024;

/// One write in a [`ModuleStore::apply`] batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOp {
    /// Insert or overwrite `key` with `value`.
    Set {
        /// Module-visible key.
        key: String,
        /// Value bytes.
        value: Vec<u8>,
    },
    /// Delete `key`; a missing key is a no-op.
    Delete {
        /// Module-visible key.
        key: String,
    },
}

/// Process-wide handle to the local-store redb database; cheap to clone.
#[derive(Debug, Clone)]
pub struct LocalStore {
    db: Arc<Database>,
    /// Per-namespace live-byte counter, lazily seeded, keeping writes O(1).
    counters: Arc<Mutex<HashMap<Vec<u8>, u64>>>,
}

/// Per-module handle carrying the pre-computed keccak256 namespace prefix.
#[derive(Debug, Clone)]
pub struct ModuleStore {
    db: Arc<Database>,
    prefix: Vec<u8>,
    counters: Arc<Mutex<HashMap<Vec<u8>, u64>>>,
    /// On-disk byte quota for this namespace; `None` is unlimited.
    quota_bytes: Option<u64>,
}

impl LocalStore {
    /// Open or create the redb file at `path`, initialising the shared table.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let db = Database::create(path).map_err(StorageError::Open)?;
        {
            let txn = db.begin_write().map_err(StorageError::Txn)?;
            txn.open_table(TABLE).map_err(StorageError::Table)?;
            txn.commit().map_err(StorageError::Commit)?;
        }
        Ok(Self {
            db: Arc::new(db),
            counters: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// [`ModuleStore`] for `namespace`; rejects the empty string.
    pub fn module(&self, namespace: &str) -> Result<ModuleStore, StorageError> {
        if namespace.is_empty() {
            return Err(StorageError::InvalidNamespace(
                "module namespace must not be empty".into(),
            ));
        }
        let prefix = keccak256(namespace.as_bytes()).to_vec();
        Ok(ModuleStore {
            db: Arc::clone(&self.db),
            prefix,
            counters: Arc::clone(&self.counters),
            quota_bytes: None,
        })
    }
}

impl ModuleStore {
    /// Cap this handle's namespace at `quota_bytes` on-disk; over-cap writes
    /// return [`StorageError::QuotaExceeded`].
    pub fn with_quota(mut self, quota_bytes: u64) -> Self {
        self.quota_bytes = Some(quota_bytes);
        self
    }

    /// Value for `key`, `Ok(None)` when absent.
    pub fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
        let full = self.build_key(key);
        let txn = self.db.begin_read().map_err(StorageError::Txn)?;
        let table = txn.open_table(TABLE).map_err(StorageError::Table)?;
        let value = table
            .get(full.as_slice())
            .map_err(StorageError::Storage)?
            .map(|v| v.value().to_vec());
        Ok(value)
    }

    /// Whether `key` exists, without copying the value out.
    pub fn contains(&self, key: &str) -> Result<bool, StorageError> {
        let full = self.build_key(key);
        let txn = self.db.begin_read().map_err(StorageError::Txn)?;
        let table = txn.open_table(TABLE).map_err(StorageError::Table)?;
        Ok(table
            .get(full.as_slice())
            .map_err(StorageError::Storage)?
            .is_some())
    }

    /// Value byte length for `key`, `Ok(None)` when absent.
    pub fn len(&self, key: &str) -> Result<Option<u64>, StorageError> {
        let full = self.build_key(key);
        let txn = self.db.begin_read().map_err(StorageError::Txn)?;
        let table = txn.open_table(TABLE).map_err(StorageError::Table)?;
        Ok(table
            .get(full.as_slice())
            .map_err(StorageError::Storage)?
            .map(|v| v.value().len() as u64))
    }

    /// Number of module-visible keys starting with `prefix`.
    pub fn count(&self, prefix: &str) -> Result<u64, StorageError> {
        let full_prefix = self.build_key(prefix);
        let txn = self.db.begin_read().map_err(StorageError::Txn)?;
        let table = txn.open_table(TABLE).map_err(StorageError::Table)?;
        let mut count = 0u64;
        for entry in table
            .range(full_prefix.as_slice()..)
            .map_err(StorageError::Storage)?
        {
            let (k, _v) = entry.map_err(StorageError::Storage)?;
            if !k.value().starts_with(&full_prefix) {
                break;
            }
            count += 1;
        }
        Ok(count)
    }

    /// Insert or overwrite; fsync-durable. An over-quota write is rejected
    /// untouched.
    pub fn set(&self, key: &str, value: &[u8]) -> Result<(), StorageError> {
        let full = self.build_key(key);
        let txn = self.db.begin_write().map_err(StorageError::Txn)?;
        let mut counters = self.counters.lock().unwrap_or_else(|e| e.into_inner());
        // Track the namespace footprint when a quota applies, or when another
        // handle of this namespace already tracks it. Untracked writes skip
        // the counter (and its seeding scan) entirely.
        let track = self.quota_bytes.is_some() || counters.contains_key(&self.prefix);
        let mut projected = 0u64;
        {
            let mut table = txn.open_table(TABLE).map_err(StorageError::Table)?;
            if track {
                let entry = self.entry_cost(key.len(), value.len());
                let old = table
                    .get(full.as_slice())
                    .map_err(StorageError::Storage)?
                    .map(|v| self.entry_cost(key.len(), v.value().len()))
                    .unwrap_or(0);
                let used = match counters.get(&self.prefix) {
                    Some(&u) => u,
                    None => self.used_bytes(&table)?,
                };
                projected = used.saturating_sub(old) + entry;
                if let Some(quota) = self.quota_bytes
                    && projected > quota
                {
                    // Returning aborts the write transaction: nothing lands.
                    return Err(StorageError::QuotaExceeded {
                        needed: projected,
                        quota,
                    });
                }
            }
            table
                .insert(full.as_slice(), value)
                .map_err(StorageError::Storage)?;
        }
        txn.commit().map_err(StorageError::Commit)?;
        if track {
            counters.insert(self.prefix.clone(), projected);
        }
        Ok(())
    }

    /// Apply `ops` atomically, fsync-durable; later ops on a key win. An
    /// over-quota batch or one past [`MAX_APPLY_OPS`] /
    /// [`MAX_APPLY_VALUE_BYTES`] is rejected before commit.
    pub fn apply(&self, ops: &[WriteOp]) -> Result<(), StorageError> {
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
        let txn = self.db.begin_write().map_err(StorageError::Txn)?;
        let mut counters = self.counters.lock().unwrap_or_else(|e| e.into_inner());
        let track = self.quota_bytes.is_some() || counters.contains_key(&self.prefix);
        let mut projected = 0u64;
        {
            let mut table = txn.open_table(TABLE).map_err(StorageError::Table)?;
            if track {
                // Net whole-batch footprint: each touched key's on-disk cost
                // is released once and its post-batch cost charged once.
                let mut finals: HashMap<&str, Option<usize>> = HashMap::new();
                for op in ops {
                    match op {
                        WriteOp::Set { key, value } => finals.insert(key, Some(value.len())),
                        WriteOp::Delete { key } => finals.insert(key, None),
                    };
                }
                let used = match counters.get(&self.prefix) {
                    Some(&u) => u,
                    None => self.used_bytes(&table)?,
                };
                let mut released = 0u64;
                let mut charged = 0u64;
                for (key, value_len) in &finals {
                    let full = self.build_key(key);
                    released += table
                        .get(full.as_slice())
                        .map_err(StorageError::Storage)?
                        .map(|v| self.entry_cost(key.len(), v.value().len()))
                        .unwrap_or(0);
                    charged += value_len
                        .map(|len| self.entry_cost(key.len(), len))
                        .unwrap_or(0);
                }
                projected = used.saturating_sub(released) + charged;
                if let Some(quota) = self.quota_bytes
                    && projected > quota
                {
                    // Returning aborts the write transaction: nothing lands.
                    return Err(StorageError::QuotaExceeded {
                        needed: projected,
                        quota,
                    });
                }
            }
            for op in ops {
                match op {
                    WriteOp::Set { key, value } => {
                        let full = self.build_key(key);
                        table
                            .insert(full.as_slice(), value.as_slice())
                            .map_err(StorageError::Storage)?;
                    }
                    WriteOp::Delete { key } => {
                        let full = self.build_key(key);
                        table
                            .remove(full.as_slice())
                            .map_err(StorageError::Storage)?;
                    }
                }
            }
        }
        txn.commit().map_err(StorageError::Commit)?;
        if track {
            counters.insert(self.prefix.clone(), projected);
        }
        Ok(())
    }

    /// On-disk footprint of one entry: prefix + key + value + overhead.
    fn entry_cost(&self, key_len: usize, value_len: usize) -> u64 {
        self.prefix.len() as u64 + ENTRY_OVERHEAD + key_len as u64 + value_len as u64
    }

    /// Seed the namespace footprint by scanning its prefix range once.
    fn used_bytes(
        &self,
        table: &impl ReadableTable<&'static [u8], &'static [u8]>,
    ) -> Result<u64, StorageError> {
        let prefix = self.prefix.as_slice();
        let mut used = 0u64;
        for entry in table.range(prefix..).map_err(StorageError::Storage)? {
            let (k, v) = entry.map_err(StorageError::Storage)?;
            let kb = k.value();
            if !kb.starts_with(prefix) {
                break;
            }
            used += self.entry_cost(kb.len() - prefix.len(), v.value().len());
        }
        Ok(used)
    }

    /// Delete. Idempotent: deleting a missing key is a no-op.
    pub fn delete(&self, key: &str) -> Result<(), StorageError> {
        let full = self.build_key(key);
        let txn = self.db.begin_write().map_err(StorageError::Txn)?;
        let mut counters = self.counters.lock().unwrap_or_else(|e| e.into_inner());
        let tracked = counters.contains_key(&self.prefix);
        let mut released = 0u64;
        {
            let mut table = txn.open_table(TABLE).map_err(StorageError::Table)?;
            if tracked {
                released = table
                    .get(full.as_slice())
                    .map_err(StorageError::Storage)?
                    .map(|v| self.entry_cost(key.len(), v.value().len()))
                    .unwrap_or(0);
            }
            table
                .remove(full.as_slice())
                .map_err(StorageError::Storage)?;
        }
        txn.commit().map_err(StorageError::Commit)?;
        if tracked && let Some(u) = counters.get_mut(&self.prefix) {
            *u = u.saturating_sub(released);
        }
        Ok(())
    }

    /// Module-visible keys whose post-prefix key starts with `prefix`.
    pub fn list_keys(&self, prefix: &str) -> Result<Vec<String>, StorageError> {
        let full_prefix = self.build_key(prefix);
        let txn = self.db.begin_read().map_err(StorageError::Txn)?;
        let table = txn.open_table(TABLE).map_err(StorageError::Table)?;
        let mut out = Vec::new();
        // redb's B-tree iterates keys in sorted order, so a range
        // starting at `full_prefix` only touches matching entries (and
        // the first key past the prefix range). Breaking on the first
        // non-matching key keeps this O(matching entries) instead of
        // the O(total DB entries) `table.iter()` would do.
        for entry in table
            .range(full_prefix.as_slice()..)
            .map_err(StorageError::Storage)?
        {
            let (k, _v) = entry.map_err(StorageError::Storage)?;
            let key_bytes = k.value();
            if !key_bytes.starts_with(&full_prefix) {
                break;
            }
            if let Ok(s) = std::str::from_utf8(&key_bytes[self.prefix.len()..]) {
                out.push(s.to_owned());
            }
        }
        Ok(out)
    }

    fn build_key(&self, key: &str) -> Vec<u8> {
        let mut out = self.prefix.clone();
        out.extend_from_slice(key.as_bytes());
        out
    }
}

/// Errors surfaced by [`LocalStore`] and [`ModuleStore`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StorageError {
    #[error("open redb: {0}")]
    Open(#[source] redb::DatabaseError),
    #[error("redb txn: {0}")]
    Txn(#[source] redb::TransactionError),
    #[error("redb table: {0}")]
    Table(#[source] redb::TableError),
    #[error("redb storage: {0}")]
    Storage(#[source] redb::StorageError),
    #[error("redb commit: {0}")]
    Commit(#[source] redb::CommitError),
    #[error("invalid namespace: {0}")]
    InvalidNamespace(String),
    #[error("local-store quota exceeded: write needs {needed} B but quota is {quota} B")]
    QuotaExceeded {
        /// Footprint the write would produce.
        needed: u64,
        /// The module's byte quota.
        quota: u64,
    },
    #[error("apply batch has {ops} ops but the cap is {cap}")]
    ApplyOpsExceeded {
        /// Ops in the rejected batch.
        ops: usize,
        /// Per-batch op cap.
        cap: usize,
    },
    #[error("apply batch carries {bytes} value B but the cap is {cap} B")]
    ApplyBytesExceeded {
        /// Total set-value bytes in the rejected batch.
        bytes: u64,
        /// Per-batch value-byte cap.
        cap: u64,
    },
}

#[cfg(test)]
mod tests;
