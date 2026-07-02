//! `nexum:host/messaging`: deferred to 0.3 (Waku backend). `query`
//! returns an empty result, same posture as `identity::accounts`.

use crate::bindings::HostError;
use crate::bindings::nexum;
use crate::host::error::unimplemented;
use crate::host::state::HostState;

impl nexum::host::messaging::Host for HostState {
    async fn publish(
        &mut self,
        _content_topic: String,
        _payload: Vec<u8>,
    ) -> Result<(), HostError> {
        Err(unimplemented("messaging", "Waku backend deferred to 0.3"))
    }

    async fn query(
        &mut self,
        _content_topic: String,
        _start_time: Option<u64>,
        _end_time: Option<u64>,
        _limit: Option<u32>,
    ) -> Result<Vec<nexum::host::types::Message>, HostError> {
        Ok(vec![])
    }
}
