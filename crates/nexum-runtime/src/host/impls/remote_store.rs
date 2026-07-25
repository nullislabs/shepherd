//! `nexum:host/remote-store`: unimplemented stub; every call returns the
//! unsupported fault.

use crate::bindings::nexum;
use crate::bindings::nexum::host::types::Fault;
use crate::host::component::RuntimeTypes;
use crate::host::state::HostState;

const DEFERRED: &str = "Swarm backend deferred to 0.3";

impl<T: RuntimeTypes> nexum::host::remote_store::Host for HostState<T> {
    async fn upload(&mut self, _data: Vec<u8>) -> Result<Vec<u8>, Fault> {
        Err(Fault::Unsupported(DEFERRED.into()))
    }

    async fn download(&mut self, _reference: Vec<u8>) -> Result<Vec<u8>, Fault> {
        Err(Fault::Unsupported(DEFERRED.into()))
    }

    async fn read_feed(
        &mut self,
        _owner: Vec<u8>,
        _topic: Vec<u8>,
    ) -> Result<Option<Vec<u8>>, Fault> {
        Err(Fault::Unsupported(DEFERRED.into()))
    }

    async fn write_feed(&mut self, _topic: Vec<u8>, _data: Vec<u8>) -> Result<Vec<u8>, Fault> {
        Err(Fault::Unsupported(DEFERRED.into()))
    }
}
