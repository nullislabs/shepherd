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
    use videre_test::{CodecVectors, Expectation};

    use crate::order::{BuyToken, SellToken};

    fn order_body() -> OrderBody {
        OrderBody::sell(SellToken([0x11; 20]), [0x01; 32])
            .for_at_least(BuyToken([0x22; 20]), [0x02; 32])
            .valid_to(1_700_000_000)
            .app_data([0x44; 32])
            .partially_fillable()
            .build()
    }

    fn composable_body() -> ComposableBody {
        ComposableBody {
            handler: [0xab; 20],
            salt: [0xcd; 32],
            static_input: vec![9, 8, 7],
        }
    }

    /// The codec conformance set: both v1 intents as round-trip vectors
    /// plus the typed failure contract, in the kit's published form.
    fn vectors() -> CodecVectors {
        let mut vectors = CodecVectors::new("cow-venue/cow-intent-body");
        vectors
            .push_round_trip(
                "v1-order",
                &CowIntentBody::V1(CowIntent::Order(order_body())),
            )
            .expect("order body encodes");
        vectors
            .push_round_trip(
                "v1-composable",
                &CowIntentBody::V1(CowIntent::Composable(composable_body())),
            )
            .expect("composable body encodes");

        let bytes = |intent: CowIntent| CowIntentBody::V1(intent).to_bytes().expect("body encodes");
        let mut unknown = bytes(CowIntent::Order(order_body()));
        unknown[0] = 9;
        vectors.push_failure(
            "unknown-version",
            unknown,
            Expectation::UnknownVersion { version: 9 },
        );
        vectors.push_failure("empty", Vec::new(), Expectation::Empty);
        let mut truncated = bytes(CowIntent::Order(order_body()));
        truncated.truncate(truncated.len() - 1);
        vectors.push_failure(
            "truncated-payload",
            truncated,
            Expectation::Malformed { version: 0 },
        );
        let mut trailing = bytes(CowIntent::Composable(composable_body()));
        trailing.push(0);
        vectors.push_failure(
            "trailing-bytes",
            trailing,
            Expectation::Malformed { version: 0 },
        );
        vectors
    }

    #[test]
    fn codec_conforms_to_its_vectors() {
        vectors().assert_conforms::<CowIntentBody>();
    }

    #[test]
    fn wire_tag_is_the_declaration_index() {
        let bytes = CowIntentBody::V1(CowIntent::Order(order_body()))
            .to_bytes()
            .unwrap();
        assert_eq!(bytes[0], 0);
    }

    #[test]
    fn divergent_codec_is_caught_by_the_vectors() {
        // A vector claiming a different typed failure must fail the
        // check, proving it has teeth on this schema.
        let mut vectors = CodecVectors::new("cow-venue/cow-intent-body");
        vectors.push_failure(
            "empty",
            Vec::new(),
            Expectation::UnknownVersion { version: 1 },
        );
        assert!(vectors.check::<CowIntentBody>().is_err());
    }
}
