//! Local-store seam: process-wide store vending per-module namespaced
//! handles, mirroring `LocalStore::module` and the `ModuleStore` API.

// StorageError embeds redb error types; same allowance as
// local_store_redb.rs.
#![allow(clippy::result_large_err)]

use crate::host::local_store_redb::{LocalStore, ModuleStore, StorageError, WriteOp};

/// Process-wide state store that vends per-module handles.
pub trait StateStore {
    /// Per-module namespaced handle type.
    type Handle: StateHandle;

    /// Return a handle scoped to `namespace`.
    fn module(&self, namespace: &str) -> Result<Self::Handle, StorageError>;
}

/// Per-module key-value handle; mirrors the inherent `ModuleStore` API.
pub trait StateHandle {
    /// Cap this handle at `quota_bytes` (key + value bytes); writes past it
    /// are rejected with [`StorageError::QuotaExceeded`].
    fn with_quota(self, quota_bytes: u64) -> Self;
    /// Fetch a value; `Ok(None)` when absent.
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError>;
    /// Insert or overwrite.
    fn set(&self, key: &str, value: &[u8]) -> Result<(), StorageError>;
    /// Delete; idempotent.
    fn delete(&self, key: &str) -> Result<(), StorageError>;
    /// Enumerate module-visible keys starting with `prefix`.
    fn list_keys(&self, prefix: &str) -> Result<Vec<String>, StorageError>;
    /// Whether `key` exists. Default fetches the value; a backend
    /// overrides when it can answer without.
    fn contains(&self, key: &str) -> Result<bool, StorageError> {
        Ok(self.get(key)?.is_some())
    }
    /// Value byte length, `Ok(None)` when absent. Default fetches the
    /// value; on some backends this may be a scan.
    fn len(&self, key: &str) -> Result<Option<u64>, StorageError> {
        Ok(self.get(key)?.map(|v| v.len() as u64))
    }
    /// Number of keys starting with `prefix`. Default materialises the
    /// key list; on some backends this may be a scan.
    fn count(&self, prefix: &str) -> Result<u64, StorageError> {
        Ok(self.list_keys(prefix)?.len() as u64)
    }
    /// Apply `ops` as one atomic batch: every op lands or none does.
    /// Quota is charged on the net whole-batch footprint; the backend
    /// caps op count and total value bytes per batch.
    fn apply(&self, ops: &[WriteOp]) -> Result<(), StorageError>;
}

impl StateStore for LocalStore {
    type Handle = ModuleStore;

    fn module(&self, namespace: &str) -> Result<ModuleStore, StorageError> {
        LocalStore::module(self, namespace)
    }
}

impl StateHandle for ModuleStore {
    fn with_quota(self, quota_bytes: u64) -> Self {
        ModuleStore::with_quota(self, quota_bytes)
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
        ModuleStore::get(self, key)
    }

    fn set(&self, key: &str, value: &[u8]) -> Result<(), StorageError> {
        ModuleStore::set(self, key, value)
    }

    fn delete(&self, key: &str) -> Result<(), StorageError> {
        ModuleStore::delete(self, key)
    }

    fn list_keys(&self, prefix: &str) -> Result<Vec<String>, StorageError> {
        ModuleStore::list_keys(self, prefix)
    }

    fn contains(&self, key: &str) -> Result<bool, StorageError> {
        ModuleStore::contains(self, key)
    }

    fn len(&self, key: &str) -> Result<Option<u64>, StorageError> {
        ModuleStore::len(self, key)
    }

    fn count(&self, prefix: &str) -> Result<u64, StorageError> {
        ModuleStore::count(self, prefix)
    }

    fn apply(&self, ops: &[WriteOp]) -> Result<(), StorageError> {
        ModuleStore::apply(self, ops)
    }
}
