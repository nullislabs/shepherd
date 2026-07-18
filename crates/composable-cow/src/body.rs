//! The composable (conditional) order body.
//!
//! ComposableCoW expresses a conditional order as the
//! `ConditionalOrderParams` tuple: the handler contract that mints the
//! tradeable order, a salt that distinguishes otherwise-identical
//! conditional orders, and the opaque handler-specific static input.
//! This body type is that tuple in wire form. The one non-obvious
//! invariant: `static_input` is opaque; only the named handler parses
//! it, so this crate never inspects its bytes.

use borsh::{BorshDeserialize, BorshSerialize};

/// The conditional order body: `ConditionalOrderParams` in wire form.
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct ComposableBody {
    /// The `IConditionalOrder` handler that mints the tradeable order.
    pub handler: [u8; 20],
    /// Salt distinguishing otherwise-identical conditional orders.
    pub salt: [u8; 32],
    /// Handler-specific static input; opaque to everything but the
    /// named handler.
    pub static_input: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ComposableBody {
        ComposableBody {
            handler: [0xab; 20],
            salt: [0xcd; 32],
            static_input: vec![1, 2, 3, 4, 5],
        }
    }

    #[test]
    fn composable_body_borsh_round_trips() {
        let body = sample();
        let bytes = borsh::to_vec(&body).expect("encode");
        assert_eq!(
            ComposableBody::try_from_slice(&bytes).expect("decode"),
            body
        );
    }

    #[test]
    fn empty_static_input_round_trips() {
        let mut body = sample();
        body.static_input = Vec::new();
        let bytes = borsh::to_vec(&body).expect("encode");
        assert_eq!(
            ComposableBody::try_from_slice(&bytes).expect("decode"),
            body
        );
    }
}
