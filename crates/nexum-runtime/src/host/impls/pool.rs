//! `nexum:intent/pool`: the strategy-facing intent import. Every method is a
//! thin delegation to the shared [`PoolRouter`](crate::host::pool_router)
//! carried in the store; the router owns the venue resolution, per-adapter
//! serialisation, guard seam, and quota. The caller identity the router meters
//! against is this store's module namespace.

use crate::bindings::pool::Host;
use crate::bindings::{IntentStatus, SubmitOutcome, VenueError};
use crate::host::component::RuntimeTypes;
use crate::host::state::HostState;

impl<T: RuntimeTypes> Host for HostState<T> {
    async fn submit(&mut self, venue: String, body: Vec<u8>) -> Result<SubmitOutcome, VenueError> {
        self.pool_router
            .submit(&self.run.module, &venue, body)
            .await
    }

    async fn status(
        &mut self,
        venue: String,
        receipt: Vec<u8>,
    ) -> Result<IntentStatus, VenueError> {
        self.pool_router.status(&venue, receipt).await
    }

    async fn cancel(&mut self, venue: String, receipt: Vec<u8>) -> Result<(), VenueError> {
        self.pool_router.cancel(&venue, receipt).await
    }
}
