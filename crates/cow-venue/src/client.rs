//! The typed CoW intent client.
//!
//! [`CowClient`] binds the strategy-facing [`IntentClient`] to the CoW
//! venue id and speaks the venue's own [`CowIntentBody`] over it, so
//! strategy code submits a typed CoW body without naming the venue on
//! every call or handling wire bytes. The classification API
//! ([`classify`](crate::classification::classify)) travels in the same
//! slice so the client that submits an order and the table that
//! classifies its rejection version together.

use nexum_venue_sdk::client::{ClientError, IntentClient, IntentPool};
use nexum_venue_sdk::{IntentStatus, SubmitOutcome};

use crate::body::CowIntentBody;

/// The venue id the CoW adapter registers under and the router resolves.
/// Every [`CowClient`] call routes here.
pub const VENUE: &str = "cow";

/// A typed intent client pre-bound to the CoW venue. A thin newtype over
/// [`IntentClient`] that fixes the venue id and the body type so callers
/// cannot mis-route or submit a foreign body.
#[derive(Clone, Debug)]
pub struct CowClient<P> {
    inner: IntentClient<P>,
}

impl<P: IntentPool> CowClient<P> {
    /// Bind a pool handle to the CoW venue.
    pub fn new(pool: P) -> Self {
        Self {
            inner: IntentClient::new(pool, VENUE),
        }
    }

    /// The venue id every call routes to (always [`VENUE`]).
    pub fn venue(&self) -> &str {
        self.inner.venue()
    }

    /// Encode a typed CoW body and submit it to the venue.
    pub fn submit(&self, body: &CowIntentBody) -> Result<SubmitOutcome, ClientError> {
        self.inner.submit(body)
    }

    /// Report where a previously submitted intent is in its life.
    pub fn status(&self, receipt: &[u8]) -> Result<IntentStatus, ClientError> {
        self.inner.status(receipt)
    }

    /// Ask the venue to withdraw an intent.
    pub fn cancel(&self, receipt: &[u8]) -> Result<(), ClientError> {
        self.inner.cancel(receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexum_venue_sdk::VenueError;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// One recorded submit: the venue it routed to and the wire bytes.
    type SubmitLog = Rc<RefCell<Vec<(String, Vec<u8>)>>>;

    /// Records the venue every call routed to and the bytes submitted.
    /// Cloneable over a shared log so the test can inspect it after the
    /// pool moves into the client.
    #[derive(Clone, Default)]
    struct SpyPool {
        submitted: SubmitLog,
    }

    impl IntentPool for SpyPool {
        fn submit(&self, venue: &str, body: Vec<u8>) -> Result<SubmitOutcome, VenueError> {
            self.submitted
                .borrow_mut()
                .push((venue.to_string(), body.clone()));
            Ok(SubmitOutcome::Accepted(body))
        }

        fn status(&self, _venue: &str, _receipt: &[u8]) -> Result<IntentStatus, VenueError> {
            unreachable!("status not exercised")
        }

        fn cancel(&self, _venue: &str, _receipt: &[u8]) -> Result<(), VenueError> {
            unreachable!("cancel not exercised")
        }
    }

    fn sample_body() -> CowIntentBody {
        use crate::body::CowIntent;
        use crate::order::{BuyTokenDestination, OrderBody, OrderKind, SellTokenSource};
        CowIntentBody::V1(CowIntent::Order(OrderBody {
            sell_token: [0x11; 20],
            buy_token: [0x22; 20],
            receiver: None,
            sell_amount: [0x01; 32],
            buy_amount: [0x02; 32],
            valid_to: 1_700_000_000,
            app_data: [0x44; 32],
            fee_amount: [0u8; 32],
            kind: OrderKind::Sell,
            partially_fillable: true,
            sell_token_balance: SellTokenSource::Erc20,
            buy_token_balance: BuyTokenDestination::Erc20,
        }))
    }

    #[test]
    fn submit_routes_to_the_cow_venue_with_encoded_body() {
        use nexum_venue_sdk::IntentBody;

        let pool = SpyPool::default();
        let body = sample_body();
        let expected = body.to_bytes().expect("body encodes");

        let client = CowClient::new(pool.clone());
        assert_eq!(client.venue(), VENUE);
        client.submit(&body).expect("submit succeeds");

        let calls = pool.submitted.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, VENUE);
        assert_eq!(calls[0].1, expected);
    }
}
