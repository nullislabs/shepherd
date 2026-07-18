//! `eth_call` JSON helpers.

use alloy_primitives::Address;

/// Build the JSON params array for `eth_call`: `[{to, data}, "latest"]`.
///
/// Returned as a `String` rather than `serde_json::Value` so the caller
/// can hand it straight to `chain::request(chain_id, "eth_call", &p)`
/// without re-serialising.
///
/// # Example
///
/// ```
/// use nexum_sdk::chain::eth_call_params;
/// use nexum_sdk::prelude::Address;
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
    // Both fields are hex, which never needs JSON escaping, so the
    // array is written directly instead of via a serde_json DOM.
    let data_hex = alloy_primitives::hex::encode_prefixed(data);
    format!(r#"[{{"to":"{to:#x}","data":"{data_hex}"}},"latest"]"#)
}

/// Parse the raw JSON-RPC `result` field a host's `chain::request`
/// returns for an `eth_call`. The value is a JSON string holding hex
/// like `"0x1234..."`; strip the JSON quotes, strip the `0x` prefix,
/// and hex-decode. Returns `None` on shape mismatch.
///
/// # Example
///
/// ```
/// use nexum_sdk::chain::parse_eth_call_result;
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
    // Borrowed deserialization: valid hex payloads never contain JSON
    // escapes, and an escaped string would fail the hex decode anyway.
    let s = serde_json::from_str::<&str>(result_json).ok()?;
    let hex = s.strip_prefix("0x").unwrap_or(s);
    alloy_primitives::hex::decode(hex).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, hex};

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
}
