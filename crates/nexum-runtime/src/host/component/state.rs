//! Local-store seam: process-wide store vending per-module namespaced
//! handles, mirroring `LocalStore::module` and the `ModuleStore` API.

// StorageError embeds redb error types; same allowance as
// local_store_redb.rs.
#![allow(clippy::result_large_err)]

use crate::host::local_store_redb::{LocalStore, ModuleStore, StorageError};

/// Process-wide state store that vends per-module handles.
pub trait StateStore {
    /// Per-module namespaced handle type.
    type Handle: StateHandle;

    /// Return a handle scoped to `namespace`.
    fn module(&self, namespace: &str) -> Result<Self::Handle, StorageError>;
}

/// Per-module key-value handle; mirrors the inherent `ModuleStore` API.
pub trait StateHandle {
    /// Fetch a value; `Ok(None)` when absent.
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError>;
    /// Insert or overwrite.
    fn set(&self, key: &str, value: &[u8]) -> Result<(), StorageError>;
    /// Delete; idempotent.
    fn delete(&self, key: &str) -> Result<(), StorageError>;
    /// Enumerate module-visible keys starting with `prefix`.
    fn list_keys(&self, prefix: &str) -> Result<Vec<String>, StorageError>;
}

impl StateStore for LocalStore {
    type Handle = ModuleStore;

    fn module(&self, namespace: &str) -> Result<ModuleStore, StorageError> {
        LocalStore::module(self, namespace)
    }
}

impl StateHandle for ModuleStore {
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
}
