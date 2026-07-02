//! `nexum:host/local-store`: redb backend with host-side namespacing.

use crate::bindings::HostError;
use crate::bindings::nexum;
use crate::host::error::internal_error;
use crate::host::local_store_redb::StorageError;
use crate::host::state::HostState;

/// Shared `StorageError` -> `HostError` conversion used by every
/// `local-store` host endpoint. Centralised so the `("local-store",
/// err.to_string())` shape stays consistent and a future error-model
/// change (richer kind, structured `data`) lands in one place
/// instead of four call sites.
fn local_store_err(err: StorageError) -> HostError {
    internal_error("local-store", err.to_string())
}

impl nexum::host::local_store::Host for HostState {
    async fn get(&mut self, key: String) -> Result<Option<Vec<u8>>, HostError> {
        self.store.get(&key).map_err(local_store_err)
    }

    async fn set(&mut self, key: String, value: Vec<u8>) -> Result<(), HostError> {
        self.store.set(&key, &value).map_err(local_store_err)
    }

    async fn delete(&mut self, key: String) -> Result<(), HostError> {
        self.store.delete(&key).map_err(local_store_err)
    }

    async fn list_keys(&mut self, prefix: String) -> Result<Vec<String>, HostError> {
        self.store.list_keys(&prefix).map_err(local_store_err)
    }
}
