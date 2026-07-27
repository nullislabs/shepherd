//! `nexum:host/local-store`: redb backend with host-side namespacing.

use crate::bindings::nexum;
use crate::bindings::nexum::host::local_store::{KeyValue, WriteOp};
use crate::bindings::nexum::host::types::Fault;
use crate::host::component::{RuntimeTypes, StateHandle};
use crate::host::local_store_redb;
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

    async fn contains(&mut self, key: String) -> Result<bool, Fault> {
        self.store.contains(&key).map_err(Fault::from)
    }

    async fn len(&mut self, key: String) -> Result<Option<u64>, Fault> {
        self.store.len(&key).map_err(Fault::from)
    }

    async fn count(&mut self, prefix: String) -> Result<u64, Fault> {
        self.store.count(&prefix).map_err(Fault::from)
    }

    async fn apply(&mut self, ops: Vec<WriteOp>) -> Result<(), Fault> {
        let ops: Vec<local_store_redb::WriteOp> = ops
            .into_iter()
            .map(|op| match op {
                WriteOp::Set(KeyValue { key, value }) => {
                    local_store_redb::WriteOp::Set { key, value }
                }
                WriteOp::Delete(key) => local_store_redb::WriteOp::Delete { key },
            })
            .collect();
        self.store.apply(&ops).map_err(Fault::from)
    }
}
