//! `nexum:host/remote-store`: deferred to 0.3 (Swarm backend).

use crate::bindings::HostError;
use crate::bindings::nexum;
use crate::host::component::{ChainProvider, CowApi, HttpClient, StateHandle};
use crate::host::error::unimplemented;
use crate::host::state::HostState;

impl<C, W, S, H> nexum::host::remote_store::Host for HostState<C, W, S, H>
where
    C: ChainProvider + Send + Sync,
    W: CowApi + Send + Sync,
    S: StateHandle + Send + Sync,
    H: HttpClient + Send + Sync,
{
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
