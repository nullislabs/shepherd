//! `eth_call` JSON helpers.

use alloy_primitives::Address;

use crate::cow::composable::{PollOutcome, decode_revert};

/// Build the JSON params array for `eth_call`: `[{to, data}, "latest"]`.
///
/// Returned as a `String` rather than `serde_json::Value` so the caller
/// can hand it straight to `chain::request(chain_id, "eth_call", &p)`
/// without re-serialising.
///
/// # Example
///
/// ```
/// use shepherd_sdk::chain::eth_call_params;
/// use shepherd_sdk::prelude::Address;
///
/// let to: Address = "0xfdaFc9d1902f4e0b84f65F49f244b32b31013b74"
///     .parse()
///     .unwrap();
/// let selector = [0xaa, 0xbb, 0xcc, 0xdd]; // 4-byte function selector
/// let params = eth_call_params(&to, &selector);
///
/// assert!(params.contains("\"to\":\"0xfdafc9d1902f4e0b84f65f49f244b32b31013b74\""));
/// assert!(params.contains("\"data\":\"0xaabbccdd\""));
/// assert!(params.contains("\"latest\""));
/// ```
pub fn eth_call_params(to: &Address, data: &[u8]) -> String {
    let to_hex = format!("{to:#x}");
    let data_hex = alloy_primitives::hex::encode_prefixed(data);
    serde_json::json!([{ "to": to_hex, "data": data_hex }, "latest"]).to_string()
}

/// Parse the raw JSON-RPC `result` field a host's `chain::request`
/// returns for an `eth_call`. The value is a JSON string holding hex
/// like `"0x1234..."`; strip the JSON quotes, strip the `0x` prefix,
/// and hex-decode. Returns `None` on shape mismatch.
///
/// # Example
///
/// ```
/// use shepherd_sdk::chain::parse_eth_call_result;
///
/// // What the host typically returns for an eth_call result: a JSON
/// // string holding 0x-prefixed hex.
/// let raw = r#""0xdeadbeef""#;
/// assert_eq!(
///     parse_eth_call_result(raw),
///     Some(vec![0xde, 0xad, 0xbe, 0xef]),
/// );
///
/// // Shape mismatch (not JSON-quoted) -> None.
/// assert_eq!(parse_eth_call_result("not json"), None);
/// ```
#[must_use]
pub fn parse_eth_call_result(result_json: &str) -> Option<Vec<u8>> {
    let s = serde_json::from_str::<String>(result_json).ok()?;
    let hex = s.strip_prefix("0x").unwrap_or(&s);
    alloy_primitives::hex::decode(hex).ok()
}

/// Decode a hex string carrying revert bytes (optionally `0x`-prefixed,
/// optionally JSON-quoted) into a [`PollOutcome`] via
/// [`crate::cow::composable::decode_revert`].
///
/// This is the bridge between the host's structured error data (a hex
/// string in `host-error.data`) and the typed
/// [`crate::cow::composable::PollOutcome`] dispatch.
///
/// # Example
///
/// ```
/// use alloy_sol_types::SolError;
/// use shepherd_sdk::chain::decode_revert_hex;
/// use shepherd_sdk::cow::{IConditionalOrder, PollOutcome};
///
/// // Simulate the host forwarding an OrderNotValid revert payload.
/// let revert = IConditionalOrder::OrderNotValid {
///     reason: "expired".into(),
/// }
/// .abi_encode();
/// let host_data = format!("\"0x{}\"", alloy_primitives::hex::encode(&revert));
///
/// assert!(matches!(
///     decode_revert_hex(&host_data),
///     Some(PollOutcome::DontTryAgain),
/// ));
/// ```
#[must_use]
pub fn decode_revert_hex(s: &str) -> Option<PollOutcome> {
    let stripped = s.trim_matches('"');
    let stripped = stripped.strip_prefix("0x").unwrap_or(stripped);
    let bytes = alloy_primitives::hex::decode(stripped).ok()?;
    decode_revert(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{U256, address, hex};
    use alloy_sol_types::SolError;

    use crate::cow::composable::IConditionalOrder;

    #[test]
    fn eth_call_params_shape() {
        let to = address!("fdaFc9d1902f4e0b84f65F49f244b32b31013b74");
        let data = hex!("aabbcc").to_vec();
        let p = eth_call_params(&to, &data);
        let parsed: serde_json::Value = serde_json::from_str(&p).unwrap();
        assert_eq!(
            parsed[0]["to"],
            "0xfdafc9d1902f4e0b84f65f49f244b32b31013b74"
        );
        assert_eq!(parsed[0]["data"], "0xaabbcc");
        assert_eq!(parsed[1], "latest");
    }

    #[test]
    fn parse_eth_call_result_decodes_hex_string() {
        assert_eq!(
            parse_eth_call_result(r#""0xdeadbeef""#),
            Some(vec![0xde, 0xad, 0xbe, 0xef]),
        );
    }

    #[test]
    fn parse_eth_call_result_handles_empty_hex() {
        assert_eq!(parse_eth_call_result(r#""0x""#), Some(vec![]));
    }

    #[test]
    fn parse_eth_call_result_rejects_non_json() {
        assert_eq!(parse_eth_call_result("garbage"), None);
    }

    #[test]
    fn decode_revert_hex_strips_prefix_and_quotes() {
        let err = IConditionalOrder::PollTryAtBlock {
            blockNumber: U256::from(42_u64),
            reason: "x".to_string(),
        };
        let payload = alloy_primitives::hex::encode_prefixed(err.abi_encode());
        let quoted = format!("\"{payload}\"");
        assert!(matches!(
            decode_revert_hex(&quoted),
            Some(PollOutcome::TryOnBlock(42))
        ));
    }

    #[test]
    fn decode_revert_hex_handles_unprefixed_naked_hex() {
        let err = IConditionalOrder::PollTryNextBlock {
            reason: "noop".to_string(),
        };
        let payload = alloy_primitives::hex::encode(err.abi_encode());
        assert!(matches!(
            decode_revert_hex(&payload),
            Some(PollOutcome::TryNextBlock)
        ));
    }
}
