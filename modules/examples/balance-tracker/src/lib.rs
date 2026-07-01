//! # balance-tracker (example Shepherd module)
//!
//! Subscribes to blocks, reads `eth_getBalance(addr)` for every
//! address in `[config].addresses` (comma-separated), persists the
//! last seen value under `balance:{addr}` in local-store, and emits
//! a Warn-level log line when the balance changes by more than
//! `[config].change_threshold` wei since the previous block.
//!
//! Demonstrates:
//!
//! - `chain::request` with a non-`eth_call` method (raw JSON-RPC),
//! - `local-store` for persistent per-key state across events,
//! - a "diff against last seen" pattern that is generic across many
//!   indexer modules (transfer monitor, allowance tracker, …).
//!
//! ## Config
//!
//! ```toml
//! [config]
//! # Comma-separated list of 0x-prefixed 20-byte addresses.
//! addresses = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8,0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
//! # Change threshold in wei; an alert fires when the delta exceeds it.
//! change_threshold = "100000000000000000"  # 0.1 ETH
//! ```

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: ["../../../wit/nexum-host", "../../../wit/shepherd-cow"],
    world: "shepherd:cow/shepherd",
    generate_all,
});

use std::sync::OnceLock;

use shepherd_sdk::prelude::{Address, U256};

use nexum::host::types::HostErrorKind;
use nexum::host::{chain, local_store, logging, types};

/// Resolved settings parsed from `[config]` at `init` and read on
/// every event.
#[derive(Debug)]
struct Settings {
    addresses: Vec<Address>,
    change_threshold: U256,
}

static SETTINGS: OnceLock<Settings> = OnceLock::new();

struct BalanceTracker;

impl Guest for BalanceTracker {
    fn init(config: Vec<(String, String)>) -> Result<(), HostError> {
        match parse_settings(&config) {
            Ok(s) => {
                logging::log(
                    logging::Level::Info,
                    &format!(
                        "balance-tracker init: {} addresses, threshold={} wei",
                        s.addresses.len(),
                        s.change_threshold,
                    ),
                );
                let _ = SETTINGS.set(s);
                Ok(())
            }
            Err(e) => Err(HostError {
                domain: "balance-tracker".into(),
                kind: HostErrorKind::InvalidInput,
                code: 0,
                message: format!("balance-tracker: invalid [config]: {e}"),
                data: None,
            }),
        }
    }

    fn on_event(event: types::Event) -> Result<(), HostError> {
        let Some(s) = SETTINGS.get() else {
            return Ok(()); // init failed; no-op.
        };
        if let types::Event::Block(block) = event {
            for addr in &s.addresses {
                if let Err(err) = check_one(block.chain_id, *addr, s.change_threshold) {
                    // Surface but do not propagate - a single flaky
                    // eth_getBalance shouldn't stop the loop.
                    logging::log(
                        logging::Level::Warn,
                        &format!("balance-tracker {addr:#x} ({}): {}", err.code, err.message),
                    );
                }
            }
        }
        Ok(())
    }
}

/// Poll one address: fetch latest balance, diff against the last
/// stored value, emit a log if the delta crosses `threshold`, then
/// persist the new value under `balance:{addr}`.
fn check_one(chain_id: u64, addr: Address, threshold: U256) -> Result<(), HostError> {
    let current = fetch_balance(chain_id, addr)?;
    let key = balance_key(&addr);
    let prior = local_store::get(&key)?
        .and_then(|b| parse_u256_le(&b))
        .unwrap_or(U256::ZERO);

    if abs_diff(current, prior) >= threshold {
        // Distinguish first-seen (prior == ZERO and we have no
        // record) from a real change - the Warn line carries the
        // delta direction so an operator can grep.
        let direction = if current > prior { "+" } else { "-" };
        logging::log(
            logging::Level::Warn,
            &format!(
                "balance-tracker {addr:#x} changed {direction}{} wei (prior={prior}, current={current})",
                abs_diff(current, prior),
            ),
        );
    }
    // Always persist the latest reading so the next event's diff is
    // accurate even when the change was below threshold.
    local_store::set(&key, &u256_to_le_bytes(current))?;
    Ok(())
}

/// `chain::request("eth_getBalance", [addr, "latest"])` -> `U256`.
/// Returns a typed HostError on any failure; the caller decides
/// whether to keep going or surface upward.
fn fetch_balance(chain_id: u64, addr: Address) -> Result<U256, HostError> {
    let params = format!("[\"{addr:#x}\",\"latest\"]");
    let result_json = chain::request(chain_id, "eth_getBalance", &params)?;
    parse_balance_hex(&result_json).ok_or_else(|| HostError {
        domain: "balance-tracker".into(),
        kind: HostErrorKind::InvalidInput,
        code: 0,
        message: format!("eth_getBalance result not a hex string: {result_json}"),
        data: None,
    })
}

// ---- pure helpers (tested) -----------------------------------------

/// Parse the `"0x..."` JSON string `eth_getBalance` returns into a
/// `U256`. `None` on shape mismatch.
fn parse_balance_hex(result_json: &str) -> Option<U256> {
    let trimmed = result_json.trim();
    let body = trimmed
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))?;
    let hex = body.strip_prefix("0x").unwrap_or(body);
    // Empty hex (`"0x"`) is a legitimate zero balance.
    if hex.is_empty() {
        return Some(U256::ZERO);
    }
    U256::from_str_radix(hex, 16).ok()
}

fn balance_key(addr: &Address) -> String {
    format!("balance:{addr:#x}")
}

fn abs_diff(a: U256, b: U256) -> U256 {
    if a >= b { a - b } else { b - a }
}

fn u256_to_le_bytes(v: U256) -> [u8; 32] {
    v.to_le_bytes()
}

fn parse_u256_le(bytes: &[u8]) -> Option<U256> {
    if bytes.len() != 32 {
        return None;
    }
    let mut buf = [0u8; 32];
    buf.copy_from_slice(bytes);
    Some(U256::from_le_bytes(buf))
}

/// Parse a comma-separated address list, stripping whitespace.
fn parse_addresses(raw: &str) -> Result<Vec<Address>, String> {
    let mut out = Vec::new();
    for (i, part) in raw.split(',').enumerate() {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        let addr = trimmed
            .parse::<Address>()
            .map_err(|e| format!("address #{i} ({trimmed:?}): {e}"))?;
        out.push(addr);
    }
    if out.is_empty() {
        return Err("expected at least one address".into());
    }
    Ok(out)
}

fn parse_settings(entries: &[(String, String)]) -> Result<Settings, String> {
    let addresses_raw = entries
        .iter()
        .find(|(k, _)| k == "addresses")
        .map(|(_, v)| v.as_str())
        .ok_or_else(|| "missing key \"addresses\"".to_string())?;
    let change_threshold_raw = entries
        .iter()
        .find(|(k, _)| k == "change_threshold")
        .map(|(_, v)| v.as_str())
        .ok_or_else(|| "missing key \"change_threshold\"".to_string())?;
    let addresses = parse_addresses(addresses_raw)?;
    let change_threshold = change_threshold_raw
        .parse::<U256>()
        .map_err(|e| format!("change_threshold: {e}"))?;
    Ok(Settings {
        addresses,
        change_threshold,
    })
}

export!(BalanceTracker);

#[cfg(test)]
mod tests {
    use super::*;
    use shepherd_sdk::prelude::address;

    #[test]
    fn parse_balance_hex_decodes_canonical_response() {
        // 0x16345785d8a0000 = 100_000_000_000_000_000 = 0.1 ETH.
        assert_eq!(
            parse_balance_hex("\"0x16345785d8a0000\""),
            Some(U256::from(100_000_000_000_000_000_u128)),
        );
    }

    #[test]
    fn parse_balance_hex_handles_zero() {
        assert_eq!(parse_balance_hex("\"0x0\""), Some(U256::ZERO));
        assert_eq!(parse_balance_hex("\"0x\""), Some(U256::ZERO));
    }

    #[test]
    fn parse_balance_hex_rejects_unquoted() {
        // Real responses are always quoted; reject as a safety net.
        assert!(parse_balance_hex("0x1234").is_none());
    }

    #[test]
    fn parse_balance_hex_rejects_garbage() {
        assert!(parse_balance_hex("\"hello\"").is_none());
    }

    #[test]
    fn u256_le_round_trip() {
        let v = U256::from(42_u64);
        let bytes = u256_to_le_bytes(v);
        assert_eq!(parse_u256_le(&bytes), Some(v));
    }

    #[test]
    fn parse_u256_le_rejects_wrong_length() {
        assert!(parse_u256_le(&[0u8; 16]).is_none());
        assert!(parse_u256_le(&[0u8; 64]).is_none());
    }

    #[test]
    fn abs_diff_is_symmetric() {
        let a = U256::from(100_u64);
        let b = U256::from(30_u64);
        assert_eq!(abs_diff(a, b), U256::from(70_u64));
        assert_eq!(abs_diff(b, a), U256::from(70_u64));
        assert_eq!(abs_diff(a, a), U256::ZERO);
    }

    #[test]
    fn parse_addresses_handles_whitespace_and_multiple() {
        let raw = "  0x70997970C51812dc3A010C7d01b50e0d17dc79C8 ,\
                   0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";
        let parsed = parse_addresses(raw).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(
            parsed[0],
            address!("70997970C51812dc3A010C7d01b50e0d17dc79C8"),
        );
    }

    #[test]
    fn parse_addresses_skips_empty_segments() {
        let parsed = parse_addresses("0x70997970C51812dc3A010C7d01b50e0d17dc79C8,,").unwrap();
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn parse_addresses_rejects_empty_list() {
        assert!(parse_addresses("").is_err());
        assert!(parse_addresses(", ,").is_err());
    }

    #[test]
    fn parse_addresses_rejects_malformed() {
        assert!(parse_addresses("not-an-address").is_err());
    }

    #[test]
    fn parse_settings_happy_path() {
        let entries = vec![
            (
                "addresses".into(),
                "0x70997970C51812dc3A010C7d01b50e0d17dc79C8".into(),
            ),
            ("change_threshold".into(), "100000000000000000".into()),
        ];
        let s = parse_settings(&entries).unwrap();
        assert_eq!(s.addresses.len(), 1);
        assert_eq!(s.change_threshold, U256::from(100_000_000_000_000_000_u128));
    }

    #[test]
    fn parse_settings_rejects_missing_keys() {
        assert!(
            parse_settings(&[("change_threshold".into(), "1".into())])
                .unwrap_err()
                .contains("addresses")
        );
        assert!(
            parse_settings(&[(
                "addresses".into(),
                "0x70997970C51812dc3A010C7d01b50e0d17dc79C8".into()
            )])
            .unwrap_err()
            .contains("change_threshold")
        );
    }
}
