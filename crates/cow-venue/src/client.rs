//! The CoW venue as a keeper types it.
//!
//! [`CowVenue`] names the venue once - the id its adapter registers
//! under and the [`CowIntentBody`] schema it decodes - so keeper code
//! drives it through [`VenueClient`] with typed bodies, never wire
//! bytes. The classification API
//! ([`classify`](crate::classification::classify)) travels in the same
//! slice so the client that submits an order and the table that
//! classifies its rejection version together.

use alloc::string::String;

use videre_sdk::client::{HostVenues, Venue, VenueClient, VenueId};
use videre_sdk::keeper::submission_key;
use videre_sdk::{BodyError, IntentBody as _};

use crate::body::CowIntentBody;

/// The CoW venue marker: every [`CowClient`] call routes to
/// [`Venue::ID`] and encodes a [`CowIntentBody`]. An accepted submit's
/// receipt is the canonical [`OrderUid`](crate::OrderUid) in wire form.
#[derive(Clone, Copy, Debug)]
pub struct CowVenue;

impl Venue for CowVenue {
    const ID: VenueId = VenueId::from_static("cow");
    type Body = CowIntentBody;
}

/// A typed client pre-bound to the CoW venue: callers cannot mis-route
/// or submit a foreign body.
pub type CowClient<T = HostVenues> = VenueClient<CowVenue, T>;

/// Deterministic intent-id for `body`: the sweep's
/// [`submission_key`] bound to [`CowVenue::ID`]. Derivable before any
/// network work, so a keeper journals the same key whether it submits
/// through the sweep or directly.
pub fn intent_id(body: &CowIntentBody) -> Result<String, BodyError> {
    Ok(submission_key(&CowVenue::ID, &body.to_bytes()?))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use videre_sdk::client::VenueTransport;
    use videre_sdk::{IntentStatus, Quotation, SubmitOutcome, VenueFault};

    use super::*;

    /// One recorded submit: the venue it routed to and the wire bytes.
    type SubmitLog = Rc<RefCell<Vec<(String, Vec<u8>)>>>;

    /// Records the venue every call routed to and the bytes submitted.
    /// Cloneable over a shared log so the test can inspect it after the
    /// handle moves into the client.
    #[derive(Clone, Default)]
    struct SpyClient {
        submitted: SubmitLog,
    }

    impl videre_sdk::client::sealed::SealedTransport for SpyClient {}

    impl VenueTransport for SpyClient {
        async fn quote(&self, _venue: &VenueId, _body: Vec<u8>) -> Result<Quotation, VenueFault> {
            unreachable!("quote not exercised")
        }

        async fn submit(
            &self,
            venue: &VenueId,
            body: Vec<u8>,
        ) -> Result<SubmitOutcome, VenueFault> {
            self.submitted
                .borrow_mut()
                .push((venue.to_string(), body.clone()));
            Ok(SubmitOutcome::Accepted(body))
        }

        async fn status(
            &self,
            _venue: &VenueId,
            _receipt: &[u8],
        ) -> Result<IntentStatus, VenueFault> {
            unreachable!("status not exercised")
        }

        async fn cancel(&self, _venue: &VenueId, _receipt: &[u8]) -> Result<(), VenueFault> {
            unreachable!("cancel not exercised")
        }
    }

    fn sample_body() -> CowIntentBody {
        use crate::body::CowIntent;
        use crate::order::{BuyToken, OrderBody, SellToken};
        CowIntentBody::V1(CowIntent::Order(
            OrderBody::sell(SellToken([0x11; 20]), [0x01; 32])
                .for_at_least(BuyToken([0x22; 20]), [0x02; 32])
                .valid_to(1_700_000_000)
                .app_data([0x44; 32])
                .partially_fillable()
                .build(),
        ))
    }

    #[test]
    fn intent_id_is_deterministic_and_body_scoped() {
        use videre_sdk::IntentBody;

        use crate::body::CowIntent;
        use crate::order::{BuyToken, OrderBody, SellToken, SignedOrder};

        let body = sample_body();
        let id = intent_id(&body).expect("body encodes");
        assert_eq!(id, intent_id(&body.clone()).expect("body encodes"));
        assert_eq!(
            id,
            submission_key(&CowVenue::ID, &body.to_bytes().expect("body encodes")),
            "the id must be exactly the key the generic sweep journals",
        );
        assert!(id.starts_with("cow:0x"));

        let other = CowIntentBody::V1(CowIntent::Signed(SignedOrder {
            order: OrderBody::sell(SellToken([0x11; 20]), [0x01; 32])
                .for_at_least(BuyToken([0x22; 20]), [0x02; 32])
                .valid_to(1_700_000_000)
                .build(),
            owner: [0x55; 20],
            signature: vec![0xC0],
        }));
        assert_ne!(id, intent_id(&other).expect("body encodes"));
    }

    #[test]
    fn submit_routes_to_the_cow_venue_with_encoded_body() {
        use videre_sdk::IntentBody;

        let spy = SpyClient::default();
        let body = sample_body();
        let expected = body.to_bytes().expect("body encodes");

        let client = CowClient::with_transport(spy.clone());
        assert_eq!(client.venue(), CowVenue::ID);
        videre_sdk::rt::complete(client.submit(&body))
            .expect("guest futures complete in one poll")
            .expect("submit succeeds");

        let calls = spy.submitted.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, CowVenue::ID.as_str());
        assert_eq!(calls[0].1, expected);
    }
}
