//! `videre:venue/client`: the keeper-facing venue import. Every method is a
//! thin delegation to the shared
//! [`VenueRegistry`](crate::host::venue_registry) carried in the store; the
//! registry owns the venue resolution, per-adapter serialisation, guard
//! seam (advisory-only for now), and quota. The caller identity the registry
//! meters against is this store's module namespace.

use crate::bindings::client::Host;
use crate::bindings::{IntentStatus, Quotation, SubmitOutcome, VenueError};
use crate::host::component::RuntimeTypes;
use crate::host::state::HostState;
use crate::host::venue_registry::VenueId;

impl<T: RuntimeTypes> Host for HostState<T> {
    async fn quote(&mut self, venue: String, body: Vec<u8>) -> Result<Quotation, VenueError> {
        self.venue_registry
            .quote(&self.run.module, &VenueId::from(venue), body)
            .await
    }

    async fn submit(&mut self, venue: String, body: Vec<u8>) -> Result<SubmitOutcome, VenueError> {
        self.venue_registry
            .submit(&self.run.module, &VenueId::from(venue), body)
            .await
    }

    async fn status(
        &mut self,
        venue: String,
        receipt: Vec<u8>,
    ) -> Result<IntentStatus, VenueError> {
        self.venue_registry
            .status(&VenueId::from(venue), receipt)
            .await
    }

    async fn cancel(&mut self, venue: String, receipt: Vec<u8>) -> Result<(), VenueError> {
        self.venue_registry
            .cancel(&VenueId::from(venue), receipt)
            .await
    }
}
