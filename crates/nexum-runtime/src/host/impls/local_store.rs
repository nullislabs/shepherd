//! `nexum:host/local-store`: redb backend with host-side namespacing.

use crate::bindings::nexum;
use crate::bindings::nexum::host::types::Fault;
use crate::host::component::{RuntimeTypes, StateHandle};
use crate::host::state::HostState;

impl<T: RuntimeTypes> nexum::host::local_store::Host for HostState<T> {
    async fn get(&mut self, key: String) -> Result<Option<Vec<u8>>, Fault> {
        self.store.get(&key).map_err(Fault::from)
    }

    async fn set(&mut self, key: String, value: Vec<u8>) -> Result<(), Fault> {
        self.store.set(&key, &value).map_err(Fault::from)
    }

    async fn delete(&mut self, key: String) -> Result<(), Fault> {
        self.store.delete(&key).map_err(Fault::from)
    }

    async fn list_keys(&mut self, prefix: String) -> Result<Vec<String>, Fault> {
        self.store.list_keys(&prefix).map_err(Fault::from)
    }
}
