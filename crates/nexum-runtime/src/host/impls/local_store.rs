//! `nexum:host/local-store`: redb backend with host-side namespacing.

use crate::bindings::HostError;
use crate::bindings::nexum;
use crate::host::component::{RuntimeTypes, StateHandle};
use crate::host::state::HostState;

impl<T: RuntimeTypes> nexum::host::local_store::Host for HostState<T> {
    async fn get(&mut self, key: String) -> Result<Option<Vec<u8>>, HostError> {
        self.store.get(&key).map_err(HostError::from)
    }

    async fn set(&mut self, key: String, value: Vec<u8>) -> Result<(), HostError> {
        self.store.set(&key, &value).map_err(HostError::from)
    }

    async fn delete(&mut self, key: String) -> Result<(), HostError> {
        self.store.delete(&key).map_err(HostError::from)
    }

    async fn list_keys(&mut self, prefix: String) -> Result<Vec<String>, HostError> {
        self.store.list_keys(&prefix).map_err(HostError::from)
    }
}
