//! The CoW intent body and its versioned `IntentBody` codec.
//!
//! A CoW intent is either a direct order or a composable (conditional)
//! order; [`CowIntent`] is that sum. [`CowIntentBody`] is the outer
//! per-venue version enum the venue publishes, and `#[derive(IntentBody)]`
//! gives it the borsh codec: a one-byte version tag plus the borsh
//! payload, with an unknown tag failing as a typed
//! [`BodyError`](videre_sdk::BodyError) rather than a stringly borsh
//! error. The one non-obvious invariant: the tag order is the schema, so
//! new versions append at the end and no variant is ever reordered or
//! removed.

use borsh::{BorshDeserialize, BorshSerialize};
use videre_sdk::IntentBody;

use crate::composable::ComposableBody;
use crate::order::OrderBody;

/// What the CoW venue accepts: a direct order or a conditional order.
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub enum CowIntent {
    /// A direct `GPv2Order` to place on the orderbook.
    Order(OrderBody),
    /// A ComposableCoW conditional order that mints tradeable orders.
    Composable(ComposableBody),
}

/// The outer per-venue version enum: the schema the CoW venue publishes.
/// Tag order is the schema; append new versions, never reorder.
#[derive(IntentBody, Clone, Debug, PartialEq, Eq)]
pub enum CowIntentBody {
    /// First published version: a [`CowIntent`] sum.
    V1(CowIntent),
}

#[cfg(test)]
mod tests {
    use super::*;
    use videre_sdk::BodyError;

    use crate::order::{BuyTokenDestination, OrderKind, SellTokenSource};

    fn order_body() -> OrderBody {
        OrderBody {
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
        }
    }

    fn composable_body() -> ComposableBody {
        ComposableBody {
            handler: [0xab; 20],
            salt: [0xcd; 32],
            static_input: vec![9, 8, 7],
        }
    }

    #[test]
    fn version_body_round_trips_through_the_derive() {
        for intent in [
            CowIntent::Order(order_body()),
            CowIntent::Composable(composable_body()),
        ] {
            let body = CowIntentBody::V1(intent);
            let bytes = body.to_bytes().expect("derived payload encodes");
            assert_eq!(CowIntentBody::from_bytes(&bytes).unwrap(), body);
        }
    }

    #[test]
    fn wire_tag_is_the_declaration_index() {
        let bytes = CowIntentBody::V1(CowIntent::Order(order_body()))
            .to_bytes()
            .unwrap();
        assert_eq!(bytes[0], 0);
    }

    #[test]
    fn unknown_version_fails_typedly() {
        let mut bytes = CowIntentBody::V1(CowIntent::Order(order_body()))
            .to_bytes()
            .unwrap();
        bytes[0] = 9;
        assert_eq!(
            CowIntentBody::from_bytes(&bytes),
            Err(BodyError::UnknownVersion { version: 9 })
        );
    }

    #[test]
    fn empty_and_malformed_bodies_fail_typedly() {
        assert_eq!(CowIntentBody::from_bytes(&[]), Err(BodyError::Empty));

        let mut bytes = CowIntentBody::V1(CowIntent::Order(order_body()))
            .to_bytes()
            .unwrap();
        bytes.truncate(bytes.len() - 1);
        assert!(matches!(
            CowIntentBody::from_bytes(&bytes),
            Err(BodyError::Malformed { version: 0, .. })
        ));

        let mut bytes = CowIntentBody::V1(CowIntent::Composable(composable_body()))
            .to_bytes()
            .unwrap();
        bytes.push(0);
        assert!(matches!(
            CowIntentBody::from_bytes(&bytes),
            Err(BodyError::Malformed { version: 0, .. })
        ));
    }
}
