//! `nexum:host/remote-store`: Swarm backend over a Bee node's HTTP API.

use crate::bindings::nexum;
use crate::bindings::nexum::host::types::Fault;
use crate::host::component::RuntimeTypes;
use crate::host::state::HostState;

impl<T: RuntimeTypes> nexum::host::remote_store::Host for HostState<T> {
    async fn upload(&mut self, data: Vec<u8>) -> Result<Vec<u8>, Fault> {
        self.remote.upload(data).await.map_err(Fault::from)
    }

    async fn download(&mut self, reference: Vec<u8>) -> Result<Vec<u8>, Fault> {
        self.remote.download(reference).await.map_err(Fault::from)
    }

    async fn read_feed(
        &mut self,
        owner: Vec<u8>,
        topic: Vec<u8>,
    ) -> Result<Option<Vec<u8>>, Fault> {
        self.remote
            .read_feed(owner, topic)
            .await
            .map_err(Fault::from)
    }

    async fn write_feed(&mut self, topic: Vec<u8>, data: Vec<u8>) -> Result<Vec<u8>, Fault> {
        self.remote
            .write_feed(topic, data)
            .await
            .map_err(Fault::from)
    }
}
