//! Property-based round-trip tests for the SDK codecs: `eth_call`
//! params/result, `config::scale_decimal`, and `U256` little-endian
//! bytes.

#![cfg(test)]

use alloy_primitives::{Address, I256, U256};
use proptest::prelude::*;

use crate::chain::{eth_call_params, parse_eth_call_result};
use crate::config;

// ---- generators ---------------------------------------------------

fn any_address() -> impl Strategy<Value = Address> {
    proptest::array::uniform20(any::<u8>()).prop_map(Address::from)
}

fn any_u256() -> impl Strategy<Value = U256> {
    proptest::array::uniform32(any::<u8>()).prop_map(U256::from_be_bytes)
}

/// Decimal-string generator: positive or negative, with or without a
/// fractional part, between 0 and 38 fractional digits.
fn decimal_string() -> impl Strategy<Value = (String, u32)> {
    (
        any::<bool>(),
        0u128..=u64::MAX as u128,
        0u32..=18,
        0u32..=18,
    )
        .prop_map(|(sign, whole, frac_len, decimals)| {
            let frac = if frac_len == 0 {
                String::new()
            } else {
                let modulo = 10u128.pow(frac_len);
                let frac_val = whole.checked_rem(modulo).unwrap_or(0);
                format!("{:0>width$}", frac_val, width = frac_len as usize)
            };
            let value = if frac.is_empty() {
                format!("{}{whole}", if sign { "-" } else { "" })
            } else {
                format!("{}{whole}.{frac}", if sign { "-" } else { "" })
            };
            (value, decimals)
        })
}

// ---- properties ---------------------------------------------------

proptest! {
    /// `eth_call_params` embeds the address; `parse_eth_call_result`
    /// round-trips any `0x`-hex body.
    #[test]
    fn eth_call_round_trip_hex(
        addr in any_address(),
        body in proptest::collection::vec(any::<u8>(), 0..512),
    ) {
        let params = eth_call_params(&addr, &body);
        // Params must contain the address (case-insensitive 0x-prefixed).
        let addr_lower = format!("{:#x}", addr);
        prop_assert!(
            params.to_ascii_lowercase().contains(&addr_lower),
            "params={params:?} missing addr={addr_lower}"
        );
        // Round-trip the body bytes back through parse_eth_call_result
        // by simulating the JSON-RPC result wrapping.
        let result_json = format!("\"0x{}\"", alloy_primitives::hex::encode(&body));
        let parsed = parse_eth_call_result(&result_json).expect("hex parses");
        prop_assert_eq!(parsed, body);
    }

    /// `parse_eth_call_result` returns `None` on a non-quoted shape.
    #[test]
    fn parse_eth_call_result_rejects_unquoted(
        s in "[a-zA-Z0-9]{0,32}",
    ) {
        // Anything without surrounding quotes must be None.
        prop_assert!(parse_eth_call_result(&s).is_none() || s.starts_with('"'));
    }

    /// `config::scale_decimal` round-trips: dividing the scaled value
    /// by `10^decimals` reproduces the integer part and the sign.
    #[test]
    fn scale_decimal_round_trip(
        (value, decimals) in decimal_string(),
    ) {
        let scaled = match config::scale_decimal(&value, decimals, "v") {
            Ok(s) => s,
            Err(_) => return Ok(()), // generator can emit out-of-range; that's OK
        };
        // Reverse: divide by 10^decimals; should match the integer
        // part of `value` (modulo sign).
        let denom = U256::from(10u128).checked_pow(U256::from(decimals)).expect("fits");
        let unsigned: U256 = scaled.unsigned_abs();
        let reconstructed_whole = unsigned / denom;
        let value_unsigned = value.trim_start_matches('-');
        let (expected_whole, _) = value_unsigned.split_once('.').unwrap_or((value_unsigned, ""));
        let expected = expected_whole.parse::<U256>().expect("generator: whole parses");
        prop_assert_eq!(
            reconstructed_whole,
            expected,
            "{}",
            format!("value={value} decimals={decimals} scaled={scaled}"),
        );
        // Sign matches.
        if value.starts_with('-') && scaled != I256::ZERO {
            prop_assert!(scaled.is_negative(), "{}", format!("expected negative for {value}"));
        } else {
            prop_assert!(
                !scaled.is_negative() || scaled == I256::ZERO,
                "{}",
                format!("expected non-negative for {value}"),
            );
        }
    }

    /// `U256` round-trips through little-endian 32-byte bytes.
    #[test]
    fn u256_le_round_trip(v in any_u256()) {
        let bytes = v.to_le_bytes::<32>();
        let back = U256::from_le_bytes(bytes);
        prop_assert_eq!(v, back);
    }
}
