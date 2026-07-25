//! `eth_call` JSON helpers.

use alloy_primitives::Address;

/// Build the JSON params array for `eth_call`: `[{to, data}, "latest"]`,
/// ready to pass to `chain::request` without re-serialising.
pub fn eth_call_params(to: &Address, data: &[u8]) -> String {
    // Both fields are hex, which never needs JSON escaping, so the
    // array is written directly instead of via a serde_json DOM.
    let data_hex = alloy_primitives::hex::encode_prefixed(data);
    format!(r#"[{{"to":"{to:#x}","data":"{data_hex}"}},"latest"]"#)
}

/// Decode the JSON-RPC `result` of an `eth_call`, a JSON string holding
/// `0x`-prefixed hex, to bytes. `None` on shape mismatch.
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
