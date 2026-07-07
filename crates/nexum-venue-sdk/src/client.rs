//! The typed intent client core: [`IntentClient`] over the byte-level
//! [`IntentPool`] seam.
//!
//! The pool boundary carries opaque bodies; this module is where a
//! typed body meets it. [`IntentClient`] binds one venue and encodes
//! through [`IntentBody`] before submission, so strategy code never
//! handles wire bytes. The seam is byte-level on purpose: the
//! strategy-module SDK implements [`IntentPool`] over its own
//! `nexum:intent/pool` import shims, tests implement it in memory
//! (an in-process adapter works directly), and the typed layer above is
//! shared by both.

use strum::IntoStaticStr;

use crate::{BodyError, IntentBody, IntentStatus, SubmitOutcome, VenueError};

/// Byte-level access to the strategy-facing `nexum:intent/pool`
/// interface, venue named per call as on the wire.
pub trait IntentPool {
    /// Submit an opaque intent body to the named venue.
    fn submit(&self, venue: &str, body: Vec<u8>) -> Result<SubmitOutcome, VenueError>;

    /// Report where a previously submitted intent is in its life.
    fn status(&self, venue: &str, receipt: &[u8]) -> Result<IntentStatus, VenueError>;

    /// Ask the venue to withdraw an intent. Success means the venue
    /// accepted the cancellation, not that an in-flight settlement can
    /// no longer win the race.
    fn cancel(&self, venue: &str, receipt: &[u8]) -> Result<(), VenueError>;
}

/// A typed intent client bound to one venue: encodes an [`IntentBody`]
/// to wire bytes and forwards through the [`IntentPool`] seam.
#[derive(Clone, Debug)]
pub struct IntentClient<P> {
    pool: P,
    venue: String,
}

impl<P: IntentPool> IntentClient<P> {
    /// Bind a pool handle to the venue id the router resolves.
    pub fn new(pool: P, venue: impl Into<String>) -> Self {
        Self {
            pool,
            venue: venue.into(),
        }
    }

    /// The venue id every call on this client routes to.
    pub fn venue(&self) -> &str {
        &self.venue
    }

    /// Encode a typed body and submit it to the bound venue.
    pub fn submit<B: IntentBody>(&self, body: &B) -> Result<SubmitOutcome, ClientError> {
        let bytes = body.to_bytes()?;
        self.pool
            .submit(&self.venue, bytes)
            .map_err(ClientError::Venue)
    }

    /// Report where a previously submitted intent is in its life.
    pub fn status(&self, receipt: &[u8]) -> Result<IntentStatus, ClientError> {
        self.pool
            .status(&self.venue, receipt)
            .map_err(ClientError::Venue)
    }

    /// Ask the bound venue to withdraw an intent.
    pub fn cancel(&self, receipt: &[u8]) -> Result<(), ClientError> {
        self.pool
            .cancel(&self.venue, receipt)
            .map_err(ClientError::Venue)
    }
}

/// Why a typed intent call failed: before the wire (the body failed to
/// encode) or beyond it (the pool or venue refused).
///
/// `IntoStaticStr` yields a snake_case label per case for log and
/// metric fields.
#[derive(Clone, Debug, PartialEq, thiserror::Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum ClientError {
    /// The typed body failed to encode; nothing reached the pool.
    #[error(transparent)]
    Body(#[from] BodyError),
    /// The pool or the venue behind it failed the call. The payload is
    /// the wire `venue-error`, which carries no `Display`; format via
    /// `Debug`.
    #[error("venue error: {0:?}")]
    Venue(VenueError),
}
