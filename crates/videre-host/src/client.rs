//! `videre:venue/client`: the keeper-facing venue import. Every method
//! resolves the shared [`VenueRegistry`] from the store's service map and
//! delegates, metering against this store's module namespace. No registry
//! service resolves every call to `unknown-venue`.

use std::sync::Arc;

use nexum_runtime::host::component::RuntimeTypes;
use nexum_runtime::host::state::HostState;

use crate::bindings::client::Host;
use crate::bindings::{IntentStatus, Quotation, SubmitOutcome, VenueError};
use crate::registry::{VenueId, VenueRegistry};

/// The registry published under the videre service namespace.
fn registry<T: RuntimeTypes>(state: &HostState<T>) -> Result<Arc<VenueRegistry>, VenueError> {
    state
        .services
        .get::<VenueRegistry>(VenueRegistry::NAMESPACE)
        .ok_or(VenueError::UnknownVenue)
}

impl<T: RuntimeTypes> Host for HostState<T> {
    async fn quote(&mut self, venue: String, body: Vec<u8>) -> Result<Quotation, VenueError> {
        registry(self)?
            .quote(&self.run.module, &VenueId::from(venue), body)
            .await
    }

    async fn submit(&mut self, venue: String, body: Vec<u8>) -> Result<SubmitOutcome, VenueError> {
        registry(self)?
            .submit(&self.run.module, &VenueId::from(venue), body)
            .await
    }

    async fn observe(&mut self, venue: String, receipt: Vec<u8>) -> Result<(), VenueError> {
        registry(self)?.observe(&VenueId::from(venue), receipt)
    }

    async fn status(
        &mut self,
        venue: String,
        receipt: Vec<u8>,
    ) -> Result<IntentStatus, VenueError> {
        registry(self)?.status(&VenueId::from(venue), receipt).await
    }

    async fn cancel(&mut self, venue: String, receipt: Vec<u8>) -> Result<(), VenueError> {
        registry(self)?.cancel(&VenueId::from(venue), receipt).await
    }
}
