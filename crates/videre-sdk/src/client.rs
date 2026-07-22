//! The typed intent client core: [`IntentClient`] over the byte-level
//! [`VenueClient`] seam.
//!
//! The client boundary carries opaque bodies; this module is where a
//! typed body meets it. [`IntentClient`] binds one [`VenueId`] and
//! encodes through [`IntentBody`] before submission, so keeper code
//! never handles wire bytes. The seam is byte-level on purpose: the
//! strategy-module SDK implements [`VenueClient`] over its own
//! `videre:venue/client` import shims, tests implement it in memory
//! (an in-process adapter works directly), and the typed layer above is
//! shared by both.

use std::fmt;

use strum::IntoStaticStr;

use crate::{BodyError, IntentBody, IntentStatus, Quotation, SubmitOutcome, VenueFault};

/// Venue identifier: the id an adapter registers under and every client
/// call routes to. Opaque beyond equality.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct VenueId(String);

impl VenueId {
    /// The id at its wire spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for VenueId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

impl From<&str> for VenueId {
    fn from(id: &str) -> Self {
        Self(id.to_owned())
    }
}

impl AsRef<str> for VenueId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for VenueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Byte-level access to the keeper-facing `videre:venue/client`
/// interface, the venue named per call.
pub trait VenueClient {
    /// Price an opaque intent body at the named venue.
    fn quote(&self, venue: &VenueId, body: Vec<u8>) -> Result<Quotation, VenueFault>;

    /// Submit an opaque intent body to the named venue.
    fn submit(&self, venue: &VenueId, body: Vec<u8>) -> Result<SubmitOutcome, VenueFault>;

    /// Report where a previously submitted intent is in its life.
    fn status(&self, venue: &VenueId, receipt: &[u8]) -> Result<IntentStatus, VenueFault>;

    /// Ask the venue to withdraw an intent. Success means the venue
    /// accepted the cancellation, not that an in-flight settlement can
    /// no longer win the race.
    fn cancel(&self, venue: &VenueId, receipt: &[u8]) -> Result<(), VenueFault>;
}

/// A typed intent client bound to one venue: encodes an [`IntentBody`]
/// to wire bytes and forwards through the [`VenueClient`] seam.
#[derive(Clone, Debug)]
pub struct IntentClient<P> {
    venues: P,
    venue: VenueId,
}

impl<P: VenueClient> IntentClient<P> {
    /// Bind a client handle to the venue id the registry resolves.
    pub fn new(venues: P, venue: impl Into<VenueId>) -> Self {
        Self {
            venues,
            venue: venue.into(),
        }
    }

    /// The venue every call on this client routes to.
    pub fn venue(&self) -> &VenueId {
        &self.venue
    }

    /// Encode a typed body and price it at the bound venue. The returned
    /// [`Quoted`] carries the encoded bytes, so `submit` sends exactly
    /// the body the venue priced.
    pub fn quote<B: IntentBody>(&self, body: &B) -> Result<Quoted<'_, P>, ClientError> {
        let bytes = body.to_bytes()?;
        let quotation = self.venues.quote(&self.venue, bytes.clone())?;
        Ok(Quoted {
            client: self,
            bytes,
            quotation,
        })
    }

    /// Encode a typed body and submit it to the bound venue.
    pub fn submit<B: IntentBody>(&self, body: &B) -> Result<SubmitOutcome, ClientError> {
        let bytes = body.to_bytes()?;
        Ok(self.venues.submit(&self.venue, bytes)?)
    }

    /// Report where a previously submitted intent is in its life.
    pub fn status(&self, receipt: &[u8]) -> Result<IntentStatus, ClientError> {
        Ok(self.venues.status(&self.venue, receipt)?)
    }

    /// Ask the bound venue to withdraw an intent.
    pub fn cancel(&self, receipt: &[u8]) -> Result<(), ClientError> {
        Ok(self.venues.cancel(&self.venue, receipt)?)
    }
}

/// A priced intent: the quotation plus the exact bytes it prices, bound
/// to the client that fetched it. Consuming it with [`submit`](Self::submit)
/// is the only way from a quote to a submission, so a keeper cannot
/// submit a body other than the one quoted.
#[derive(Debug)]
pub struct Quoted<'a, P> {
    client: &'a IntentClient<P>,
    bytes: Vec<u8>,
    quotation: Quotation,
}

impl<P: VenueClient> Quoted<'_, P> {
    /// The venue's indicative quotation for the body.
    pub fn quotation(&self) -> &Quotation {
        &self.quotation
    }

    /// Submit the quoted body to the venue that priced it.
    pub fn submit(self) -> Result<SubmitOutcome, ClientError> {
        Ok(self.client.venues.submit(&self.client.venue, self.bytes)?)
    }
}

/// Why a typed intent call failed: before the wire (the body failed to
/// encode) or beyond it (the registry or venue refused).
///
/// `IntoStaticStr` yields a snake_case label per case for log and
/// metric fields.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum ClientError {
    /// The typed body failed to encode; nothing crossed the wire.
    #[error(transparent)]
    Body(#[from] BodyError),
    /// The registry or the venue behind it refused the call.
    #[error(transparent)]
    Venue(#[from] VenueFault),
}
