//! `videre:venue/client`: the keeper-facing venue import. Every method
//! resolves the shared [`VenueRegistry`] from the store's service map under
//! the videre namespace and delegates; the registry owns the venue
//! resolution, per-adapter serialisation, guard seam (advisory-only for
//! now), and quota. The caller identity the registry meters against is this
//! store's module namespace. No registry service means no venues, so every
//! call resolves to `unknown-venue`.

use std::sync::Arc;

use crate::bindings::client::Host;
use crate::bindings::{IntentStatus, Quotation, SubmitOutcome, VenueError};
use crate::host::component::RuntimeTypes;
use crate::host::state::HostState;
use crate::host::venue_registry::{VenueId, VenueRegistry};

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
