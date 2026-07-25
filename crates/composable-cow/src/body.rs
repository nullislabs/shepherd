//! The composable (conditional) order body.
//!
//! ComposableCoW's `ConditionalOrderParams` tuple in wire form: the
//! handler that mints the tradeable order, a salt, and the opaque
//! handler-specific static input. `static_input` is opaque; only the
//! named handler parses it.

use alloy_primitives::{Address, B256, Bytes};
use borsh::{BorshDeserialize, BorshSerialize};

/// The conditional order body: `ConditionalOrderParams` in wire form.
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct ComposableBody {
    /// The `IConditionalOrder` handler that mints the tradeable order.
    pub handler: Address,
    /// Salt distinguishing otherwise-identical conditional orders.
    pub salt: B256,
    /// Handler-specific static input; opaque to everything but the
    /// named handler.
    pub static_input: Bytes,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ComposableBody {
        ComposableBody {
            handler: Address::repeat_byte(0xab),
            salt: B256::repeat_byte(0xcd),
            static_input: Bytes::from_static(&[1, 2, 3, 4, 5]),
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
        body.static_input = Bytes::new();
        let bytes = borsh::to_vec(&body).expect("encode");
        assert_eq!(
            ComposableBody::try_from_slice(&bytes).expect("decode"),
            body
        );
    }

    /// Wire layout: 20 bare handler bytes, 32 bare salt bytes, then a
    /// `u32`-length-prefixed static input.
    #[test]
    fn wire_matches_the_raw_array_layout() {
        let mut expected = Vec::new();
        expected.extend_from_slice(&[0xab; 20]);
        expected.extend_from_slice(&[0xcd; 32]);
        expected.extend_from_slice(&5_u32.to_le_bytes());
        expected.extend_from_slice(&[1, 2, 3, 4, 5]);
        assert_eq!(borsh::to_vec(&sample()).expect("encode"), expected);
    }

    /// A truncated handler is a decode error, not a silent short read.
    #[test]
    fn truncated_input_fails_to_decode() {
        let bytes = borsh::to_vec(&sample()).expect("encode");
        assert!(ComposableBody::try_from_slice(&bytes[..10]).is_err());
    }
}
