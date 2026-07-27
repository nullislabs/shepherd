//! The CoW venue as a keeper types it.
//!
//! [`CowVenue`] names the venue: the id its adapter registers under and
//! the [`CowIntentBody`] schema, so keeper code drives it through
//! [`VenueClient`] with typed bodies. The retry
//! [`classify`](crate::classification::classify) API ships in the same
//! slice so client and classification version together.

use videre_sdk::client::{HostVenues, Venue, VenueClient};
use videre_sdk::keeper::submission_key;
use videre_sdk::{BodyError, IntentBody as _};

use crate::body::CowIntentBody;

/// The CoW venue marker: `CowClient` calls route to [`Venue::ID`] and
/// encode a [`CowIntentBody`]; a receipt is the canonical
/// [`OrderUid`](crate::OrderUid) in wire form.
#[derive(Clone, Copy, Debug)]
pub struct CowVenue;

// The id is held to `module.toml`'s `[module] name` at expansion.
#[videre_sdk::venue(id = "cow", body = CowIntentBody)]
impl Venue for CowVenue {}

/// A typed client pre-bound to the CoW venue.
pub type CowClient<T = HostVenues> = VenueClient<CowVenue, T>;

/// Deterministic intent-id for `body`: [`submission_key`] bound to
/// [`CowVenue::ID`], derivable before any network work. It covers the
/// encoded body, so a signed payload
/// ([`CowIntent::Signed`](crate::CowIntent::Signed)) keys on its
/// signature, not the economic order.
pub fn intent_id(body: &CowIntentBody) -> Result<String, BodyError> {
    Ok(submission_key(&CowVenue::ID, &body.to_bytes()?))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use videre_sdk::client::{VenueId, VenueTransport};
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
        use alloy_primitives::{Address, U256};

        use crate::body::CowIntent;
        use crate::order::{BuyToken, OrderBody, SellToken};
        CowIntentBody::V1(CowIntent::Order(
            OrderBody::sell(
                SellToken(Address::repeat_byte(0x11)),
                U256::from(1u64),
                BuyToken(Address::repeat_byte(0x22)),
                U256::from(2u64),
                1_700_000_000,
            )
            .app_data([0x44; 32])
            .partially_fillable()
            .build(),
        ))
    }

    #[test]
    fn intent_id_is_deterministic_and_body_scoped() {
        use alloy_primitives::{Address, U256};
        use videre_sdk::IntentBody;

        use crate::body::CowIntent;
        use crate::order::{BuyToken, OrderBody, SellToken, SignedOrder};

        let body = sample_body();
        let id = intent_id(&body).expect("body encodes");
        assert_eq!(id, intent_id(&body.clone()).expect("body encodes"));
        assert_eq!(
            id,
            submission_key(&CowVenue::ID, &body.to_bytes().expect("body encodes")),
            "the id must be exactly the key the generic run journals",
        );
        assert!(id.starts_with("cow:0x"));

        let other = CowIntentBody::V1(CowIntent::Signed(SignedOrder {
            order: OrderBody::sell(
                SellToken(Address::repeat_byte(0x11)),
                U256::from(1u64),
                BuyToken(Address::repeat_byte(0x22)),
                U256::from(2u64),
                1_700_000_000,
            )
            .build(),
            owner: Address::repeat_byte(0x55),
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
        let std::task::Poll::Ready(result) = videre_sdk::client::poll_once(client.submit(&body))
        else {
            panic!("guest futures complete in one poll");
        };
        result.expect("submit succeeds");

        let calls = spy.submitted.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, CowVenue::ID.as_str());
        assert_eq!(calls[0].1, expected);
    }
}
