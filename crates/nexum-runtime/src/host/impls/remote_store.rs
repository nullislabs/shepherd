//! `nexum:host/remote-store`: deferred to 0.3 (Swarm backend).

use crate::bindings::HostError;
use crate::bindings::nexum;
use crate::host::error::unimplemented;
use crate::host::state::HostState;

impl nexum::host::remote_store::Host for HostState {
    async fn upload(&mut self, _data: Vec<u8>) -> Result<Vec<u8>, HostError> {
        Err(unimplemented(
            "remote-store",
            "Swarm backend deferred to 0.3",
        ))
    }

    async fn download(&mut self, _reference: Vec<u8>) -> Result<Vec<u8>, HostError> {
        Err(unimplemented(
            "remote-store",
            "Swarm backend deferred to 0.3",
        ))
    }

    async fn read_feed(
        &mut self,
        _owner: Vec<u8>,
        _topic: Vec<u8>,
    ) -> Result<Option<Vec<u8>>, HostError> {
        Err(unimplemented(
            "remote-store",
            "Swarm backend deferred to 0.3",
        ))
    }

    async fn write_feed(&mut self, _topic: Vec<u8>, _data: Vec<u8>) -> Result<Vec<u8>, HostError> {
        Err(unimplemented(
            "remote-store",
            "Swarm backend deferred to 0.3",
        ))
    }
}
