//! The typed venue client: [`VenueClient`] binds one [`Venue`] over the
//! byte-level [`VenueTransport`] seam.
//!
//! The wire carries opaque bodies and a stringly venue selector; typing
//! is recovered here. A venue is named once, as a [`Venue`] marker
//! carrying its [`VenueId`] and body schema, and every call encodes
//! through [`IntentBody`] before the seam, so keeper code never handles
//! wire bytes. [`HostVenues`] is the seam bound to the module's own
//! `videre:venue/client` import; tests and in-process adapters
//! implement [`VenueTransport`] directly. The transport methods are
//! native AFIT, so dispatch is static and nothing on the call path
//! boxes.

use std::borrow::Cow;
use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use strum::IntoStaticStr;

use crate::bindings::videre::venue::client as shims;
use crate::{BodyError, IntentBody, IntentStatus, Quotation, SubmitOutcome, VenueFault};

/// Venue identifier: the id an adapter registers under and every client
/// call routes to. Opaque beyond equality.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct VenueId(Cow<'static, str>);

impl VenueId {
    /// Wrap a static id without allocating: the [`Venue::ID`] spelling.
    #[must_use]
    pub const fn from_static(id: &'static str) -> Self {
        Self(Cow::Borrowed(id))
    }

    /// The id at its wire spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for VenueId {
    fn from(id: String) -> Self {
        Self(Cow::Owned(id))
    }
}

impl From<&str> for VenueId {
    fn from(id: &str) -> Self {
        Self(Cow::Owned(id.to_owned()))
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

/// One venue as a keeper types it: the id its adapter registers under
/// and the body schema it decodes. Implement on a unit marker
/// (`struct CowVenue;`) and drive it through [`VenueClient`].
pub trait Venue {
    /// The id the venue's adapter registers under.
    const ID: VenueId;

    /// The versioned body schema the venue decodes.
    type Body: IntentBody;
}

/// Sealing markers: a transport opts into [`VenueTransport`], and an
/// adapter into [`VenueReconcile`], by also implementing the respective
/// marker.
#[doc(hidden)]
pub mod sealed {
    pub trait SealedTransport {}
    pub trait SealedReconcile {}
}

/// The byte-level seam under the typed client: `videre:venue/client`
/// with the venue named per call. Native AFIT, so a [`VenueClient`]
/// over any transport dispatches statically. [`HostVenues`] binds it to
/// the module's own import; tests implement it in memory.
///
/// Sealed: a transport opts in by also implementing the sealing marker.
pub trait VenueTransport: sealed::SealedTransport {
    /// Price an opaque intent body at the named venue.
    fn quote(
        &self,
        venue: &VenueId,
        body: Vec<u8>,
    ) -> impl Future<Output = Result<Quotation, VenueFault>>;

    /// Submit an opaque intent body to the named venue.
    fn submit(
        &self,
        venue: &VenueId,
        body: Vec<u8>,
    ) -> impl Future<Output = Result<SubmitOutcome, VenueFault>>;

    /// Put an externally-obtained receipt under the host's status
    /// watch; an accepted submit is watched implicitly. Defaults to
    /// `unsupported`: a transport that can watch foreign receipts opts
    /// in.
    fn observe(
        &self,
        venue: &VenueId,
        receipt: &[u8],
    ) -> impl Future<Output = Result<(), VenueFault>> {
        let _ = (venue, receipt);
        async { Err(VenueFault::Unsupported) }
    }

    /// Report where a previously submitted intent is in its life.
    fn status(
        &self,
        venue: &VenueId,
        receipt: &[u8],
    ) -> impl Future<Output = Result<IntentStatus, VenueFault>>;

    /// Ask the venue to withdraw an intent. Success means the venue
    /// accepted the cancellation, not that an in-flight settlement can
    /// no longer win the race.
    fn cancel(
        &self,
        venue: &VenueId,
        receipt: &[u8],
    ) -> impl Future<Output = Result<(), VenueFault>>;
}

/// The reconcile contract a venue adapter honours so a keeper can
/// recover a stranded reservation without double-placing. A marker: it
/// adds no methods, it names the guarantees the adapter's
/// [`submit`](crate::VenueAdapter::submit) and
/// [`status`](crate::VenueAdapter::status) already give. Exactly-once
/// across the external POST is unreachable from the host alone (the call
/// is not inside the reserve transaction), so the floor is venue-side and
/// per-adapter.
///
/// An adapter opts in by implementing it (and the sealing marker), and
/// proves it with `videre_test::venue_reconcile_compliance!`.
///
/// # Contract
///
/// 1. Mandatory re-POST idempotency. A [`submit`](crate::VenueAdapter::submit)
///    of a body the venue already holds resolves to the SAME outcome as
///    the first submit: a signed order folds to
///    [`SubmitOutcome::Accepted`] with the same receipt, a pre-sign order
///    to the same [`SubmitOutcome::RequiresSigning`] call. A held body
///    NEVER surfaces as a terminal [`VenueFault`]: it folds to the accept
///    outcome, never to a classified `denied`. This floor is what makes a
///    reconcile resubmit safe.
/// 2. Optional status fast-path. An adapter MAY derive a receipt from the
///    body, so a reconcile can [`status`](crate::VenueAdapter::status) it
///    first and commit without a redundant POST.
///    [`observe`](VenueTransport::observe) (defaulting `unsupported`) is
///    not the reconcile primitive; `submit` is.
///
/// # Validity
///
/// Reconcile trusts venue-side order validity (its `validTo` and limit
/// price); it does not re-poll the watch source. The contract therefore
/// requires adapters to carry self-describing order validity. (Maintainer
/// decision, 2026-07-24.)
pub trait VenueReconcile: crate::VenueAdapter + sealed::SealedReconcile {}

/// Poll a future once and return its state. `videre:venue/client@0.1.0`
/// declares plain funcs, so a [`VenueTransport`] over the host import
/// resolves on the first poll. [`Poll::Pending`] means a foreign
/// [`VenueTransport`] impl suspended, which the keeper macro folds to
/// `Fault::Internal`.
pub fn poll_once<F: Future>(future: F) -> Poll<F::Output> {
    let mut future = pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    future.as_mut().poll(&mut cx)
}

/// The module's `videre:venue/client` import behind the
/// [`VenueTransport`] seam: the transport every guest-side
/// [`VenueClient`] defaults to.
#[derive(Clone, Copy, Debug, Default)]
pub struct HostVenues;

impl sealed::SealedTransport for HostVenues {}

impl VenueTransport for HostVenues {
    async fn quote(&self, venue: &VenueId, body: Vec<u8>) -> Result<Quotation, VenueFault> {
        shims::quote(venue.as_str(), &body).map_err(VenueFault::from)
    }

    async fn submit(&self, venue: &VenueId, body: Vec<u8>) -> Result<SubmitOutcome, VenueFault> {
        shims::submit(venue.as_str(), &body).map_err(VenueFault::from)
    }

    async fn observe(&self, venue: &VenueId, receipt: &[u8]) -> Result<(), VenueFault> {
        shims::observe(venue.as_str(), receipt).map_err(VenueFault::from)
    }

    async fn status(&self, venue: &VenueId, receipt: &[u8]) -> Result<IntentStatus, VenueFault> {
        shims::status(venue.as_str(), receipt).map_err(VenueFault::from)
    }

    async fn cancel(&self, venue: &VenueId, receipt: &[u8]) -> Result<(), VenueFault> {
        shims::cancel(venue.as_str(), receipt).map_err(VenueFault::from)
    }
}

/// A typed client bound to one [`Venue`]: encodes the venue's
/// [`IntentBody`] to wire bytes and forwards through the
/// [`VenueTransport`] seam under [`Venue::ID`]. Zero-sized over the
/// default [`HostVenues`] transport.
pub struct VenueClient<V: Venue, T: VenueTransport = HostVenues> {
    transport: T,
    venue: PhantomData<V>,
}

impl<V: Venue> VenueClient<V> {
    /// Bind the venue over the module's own `videre:venue/client`
    /// import.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            transport: HostVenues,
            venue: PhantomData,
        }
    }
}

impl<V: Venue> Default for VenueClient<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: Venue, T: VenueTransport> VenueClient<V, T> {
    /// Bind the venue over a caller-supplied transport (tests,
    /// in-process adapters).
    pub const fn with_transport(transport: T) -> Self {
        Self {
            transport,
            venue: PhantomData,
        }
    }

    /// The venue id every call on this client routes to.
    #[must_use]
    pub fn venue(&self) -> VenueId {
        V::ID
    }

    /// The bound transport, so a keeper reconcile pass resubmits reserved
    /// bodies through the same seam this client submits on.
    #[must_use]
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Encode the typed body and price it at the bound venue. The
    /// returned [`Quoted`] carries the encoded bytes, so `submit` sends
    /// exactly the body the venue priced.
    pub async fn quote(&self, body: &V::Body) -> Result<Quoted<'_, V, T>, ClientError> {
        let bytes = body.to_bytes()?;
        let quotation = self.transport.quote(&V::ID, bytes.clone()).await?;
        Ok(Quoted {
            client: self,
            bytes,
            quotation,
        })
    }

    /// Encode the typed body and submit it to the bound venue.
    pub async fn submit(&self, body: &V::Body) -> Result<SubmitOutcome, ClientError> {
        let bytes = body.to_bytes()?;
        Ok(self.transport.submit(&V::ID, bytes).await?)
    }

    /// Put an externally-obtained receipt under the host's status
    /// watch at the bound venue.
    pub async fn observe(&self, receipt: &[u8]) -> Result<(), ClientError> {
        Ok(self.transport.observe(&V::ID, receipt).await?)
    }

    /// Report where a previously submitted intent is in its life.
    /// Rejects an empty receipt as `invalid-body` before the wire.
    pub async fn status(&self, receipt: &[u8]) -> Result<IntentStatus, ClientError> {
        crate::adapter::guard_receipt(receipt).map_err(VenueFault::from)?;
        Ok(self.transport.status(&V::ID, receipt).await?)
    }

    /// Ask the bound venue to withdraw an intent. Rejects an empty
    /// receipt as `invalid-body` before the wire.
    pub async fn cancel(&self, receipt: &[u8]) -> Result<(), ClientError> {
        crate::adapter::guard_receipt(receipt).map_err(VenueFault::from)?;
        Ok(self.transport.cancel(&V::ID, receipt).await?)
    }
}

impl<V: Venue, T: VenueTransport + Clone> Clone for VenueClient<V, T> {
    fn clone(&self) -> Self {
        Self {
            transport: self.transport.clone(),
            venue: PhantomData,
        }
    }
}

impl<V: Venue, T: VenueTransport> fmt::Debug for VenueClient<V, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VenueClient")
            .field("venue", &V::ID)
            .finish_non_exhaustive()
    }
}

/// A priced intent: the quotation plus the exact bytes it prices, bound
/// to the client that fetched it. Consuming it with
/// [`submit`](Self::submit) is the only way from a quote to a
/// submission, so a keeper cannot submit a body other than the one
/// quoted.
pub struct Quoted<'a, V: Venue, T: VenueTransport> {
    client: &'a VenueClient<V, T>,
    bytes: Vec<u8>,
    quotation: Quotation,
}

impl<V: Venue, T: VenueTransport> Quoted<'_, V, T> {
    /// The venue's indicative quotation for the body.
    #[must_use]
    pub fn quotation(&self) -> &Quotation {
        &self.quotation
    }

    /// Submit the quoted body to the venue that priced it.
    pub async fn submit(self) -> Result<SubmitOutcome, ClientError> {
        Ok(self.client.transport.submit(&V::ID, self.bytes).await?)
    }
}

impl<V: Venue, T: VenueTransport> fmt::Debug for Quoted<'_, V, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Quoted")
            .field("venue", &V::ID)
            .field("quotation", &self.quotation)
            .finish_non_exhaustive()
    }
}

/// Why a typed client call failed: before the wire (the body failed to
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

#[cfg(test)]
mod tests {
    use std::task::Poll;

    use super::poll_once;

    #[test]
    fn ready_chain_completes_in_one_poll() {
        async fn two() -> u8 {
            let one = async { 1u8 }.await;
            one + async { 1u8 }.await
        }
        assert_eq!(poll_once(two()), Poll::Ready(2));
    }

    #[test]
    fn suspending_future_reports_pending() {
        assert_eq!(poll_once(std::future::pending::<()>()), Poll::Pending);
    }
}
