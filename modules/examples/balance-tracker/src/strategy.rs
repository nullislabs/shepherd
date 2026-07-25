//! Pure strategy logic for the balance-tracker module.
//!
//! World access flows through the [`ChainHost`] + [`LocalStoreHost`]
//! trait seam; `lib.rs` hands [`on_block`] a `WitBindgenHost`, tests a
//! `nexum_sdk_test::MockHost`.

use nexum_sdk::address::parse_address_list;
use nexum_sdk::config::{self, ConfigError};
use nexum_sdk::host::{ChainHost, Fault, LocalStoreHost};
use nexum_sdk::prelude::{Address, U256};

/// Settings parsed from `[config]` at `init`.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Addresses to track.
    pub addresses: Vec<Address>,
    /// Alert fires when a balance moves more than this many wei.
    pub change_threshold: U256,
}

/// Poll every tracked address on a new block, warn on threshold-crossing
/// diffs, persist the latest reading. A single flaky `eth_getBalance` is
/// logged and skipped, not fatal to the loop.
pub fn on_block<H: ChainHost + LocalStoreHost>(
    host: &H,
    chain_id: u64,
    settings: &Settings,
) -> Result<(), Fault> {
    for addr in &settings.addresses {
        if let Err(err) = check_one(host, chain_id, *addr, settings.change_threshold) {
            tracing::warn!("balance-tracker {addr:#x}: {err}");
        }
    }
    Ok(())
}

/// Fetch one address's balance, warn if the diff against the stored
/// value crosses `threshold`, then persist it under `balance:{addr}`.
fn check_one<H: ChainHost + LocalStoreHost>(
    host: &H,
    chain_id: u64,
    addr: Address,
    threshold: U256,
) -> Result<(), Fault> {
    let current = fetch_balance(host, chain_id, addr)?;
    let key = balance_key(&addr);
    let prior = host.get(&key)?.and_then(|b| parse_u256_le(&b));

    if let Some(prior) = prior
        && abs_diff(current, prior) >= threshold
    {
        let direction = if current > prior { "+" } else { "-" };
        tracing::warn!(
            "balance-tracker {addr:#x} changed {direction}{} wei (prior={prior}, current={current})",
            abs_diff(current, prior),
        );
    }
    // Always persist the latest reading so the next event's diff is
    // accurate even when the change was below threshold (or when this
    // is the first observation for the address).
    host.set(&key, &u256_to_le_bytes(current))?;
    Ok(())
}

/// `chain::request("eth_getBalance", [addr, "latest"])` -> `U256`.
fn fetch_balance<H: ChainHost>(host: &H, chain_id: u64, addr: Address) -> Result<U256, Fault> {
    let params = format!("[\"{addr:#x}\",\"latest\"]");
    let result_json = host.request(chain_id, "eth_getBalance", &params)?;
    parse_balance_hex(&result_json).ok_or_else(|| {
        invalid_input(format!(
            "eth_getBalance result not a hex string: {result_json}"
        ))
    })
}

// ---- pure helpers (unit-testable, no host) ------------------------

/// Parse the `"0x..."` JSON string `eth_getBalance` returns; `None` on
/// shape mismatch.
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

/// Parse `module.toml::[config]` into a typed [`Settings`].
pub fn parse_config(entries: &[(String, String)]) -> Result<Settings, Fault> {
    let addresses_raw = config::get_required(entries, "addresses").map_err(config_err)?;
    let change_threshold_raw =
        config::get_required(entries, "change_threshold").map_err(config_err)?;
    let addresses = parse_address_list(addresses_raw).map_err(|e| invalid_input(e.to_string()))?;
    let change_threshold = change_threshold_raw
        .parse::<U256>()
        .map_err(|e| invalid_input(format!("change_threshold: {e}")))?;
    Ok(Settings {
        addresses,
        change_threshold,
    })
}

fn invalid_input(message: impl Into<String>) -> Fault {
    Fault::InvalidInput(message.into())
}

fn config_err(e: ConfigError) -> Fault {
    invalid_input(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexum_sdk::Level;
    use nexum_sdk::host::{ChainError, Fault};
    use nexum_sdk::prelude::address;
    use nexum_sdk_test::{MockHost, capture_tracing};

    const SEPOLIA: u64 = 11_155_111;

    // ---- pure helpers ----

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
    fn parse_config_happy_path() {
        let entries = vec![
            (
                "addresses".into(),
                "0x70997970C51812dc3A010C7d01b50e0d17dc79C8".into(),
            ),
            ("change_threshold".into(), "100000000000000000".into()),
        ];
        let s = parse_config(&entries).unwrap();
        assert_eq!(s.addresses.len(), 1);
        assert_eq!(s.change_threshold, U256::from(100_000_000_000_000_000_u128));
    }

    #[test]
    fn parse_config_rejects_missing_addresses() {
        let err = parse_config(&[("change_threshold".into(), "1".into())]).unwrap_err();
        let Fault::InvalidInput(message) = err else {
            panic!("expected invalid-input fault, got {err:?}");
        };
        assert!(message.contains("addresses"));
    }

    #[test]
    fn parse_config_rejects_missing_change_threshold() {
        let err = parse_config(&[(
            "addresses".into(),
            "0x70997970C51812dc3A010C7d01b50e0d17dc79C8".into(),
        )])
        .unwrap_err();
        let Fault::InvalidInput(message) = err else {
            panic!("expected invalid-input fault, got {err:?}");
        };
        assert!(message.contains("change_threshold"));
    }

    // ---- MockHost-driven coverage of check_one / fetch_balance ----

    fn one_addr_settings(threshold_wei: u128) -> Settings {
        Settings {
            addresses: vec![address!("70997970C51812dc3A010C7d01b50e0d17dc79C8")],
            change_threshold: U256::from(threshold_wei),
        }
    }

    fn encode_balance_response(wei: u128) -> String {
        format!("\"0x{:x}\"", wei)
    }

    #[test]
    fn first_seen_persists_without_alert() {
        let host = MockHost::new();
        let settings = one_addr_settings(50);
        let addr = settings.addresses[0];
        let params = format!("[\"{addr:#x}\",\"latest\"]");
        host.chain
            .respond_to("eth_getBalance", &params, Ok(encode_balance_response(100)));

        let (result, logs) = capture_tracing(|| on_block(&host, SEPOLIA, &settings));
        result.unwrap();

        // First observation: no prior value in the store, so no
        // comparison fires - the balance is just persisted silently.
        assert_eq!(logs.count_at(Level::WARN), 0);
        // Balance persisted for the next block's diff.
        let stored = host
            .store
            .snapshot()
            .get(&format!("balance:{addr:#x}"))
            .cloned()
            .expect("balance persisted");
        assert_eq!(parse_u256_le(&stored), Some(U256::from(100u64)));
    }

    #[test]
    fn balance_change_below_threshold_persists_without_log() {
        let host = MockHost::new();
        let settings = one_addr_settings(1_000);
        let addr = settings.addresses[0];
        // Pre-seed prior balance = 100.
        host.store
            .set(
                &format!("balance:{addr:#x}"),
                &u256_to_le_bytes(U256::from(100u64)),
            )
            .unwrap();
        let params = format!("[\"{addr:#x}\",\"latest\"]");
        host.chain
            .respond_to("eth_getBalance", &params, Ok(encode_balance_response(150)));

        let (result, logs) = capture_tracing(|| on_block(&host, SEPOLIA, &settings));
        result.unwrap();

        // Delta of 50 is under the 1_000 threshold; no Warn line for
        // a "changed" event.
        assert_eq!(logs.count_at(Level::WARN), 0);
        // But the new value is persisted.
        let stored = host
            .store
            .snapshot()
            .get(&format!("balance:{addr:#x}"))
            .cloned()
            .unwrap();
        assert_eq!(parse_u256_le(&stored), Some(U256::from(150u64)));
    }

    #[test]
    fn fetch_balance_error_logs_warn_does_not_abort_loop() {
        let host = MockHost::new();
        // Two addresses; the first errors out, the second succeeds.
        let addr_a = address!("70997970C51812dc3A010C7d01b50e0d17dc79C8");
        let addr_b = address!("f39Fd6e51aad88F6F4ce6aB8827279cffFb92266");
        let settings = Settings {
            addresses: vec![addr_a, addr_b],
            change_threshold: U256::from(1u64),
        };
        let params_a = format!("[\"{addr_a:#x}\",\"latest\"]");
        let params_b = format!("[\"{addr_b:#x}\",\"latest\"]");
        host.chain.respond_to(
            "eth_getBalance",
            &params_a,
            Err(ChainError::Fault(Fault::Unavailable("rpc down".into()))),
        );
        host.chain
            .respond_to("eth_getBalance", &params_b, Ok(encode_balance_response(42)));

        let (result, captured) = capture_tracing(|| on_block(&host, SEPOLIA, &settings));
        result.unwrap();

        // First address errored; Warn line emitted with addr_a.
        let ev = captured.expect_one(|e| e.level == Level::WARN);
        assert!(ev.message.contains(&format!("{addr_a:#x}")));
        assert!(ev.message.contains("rpc down"));
        // Second address still ran; its balance persisted.
        assert!(
            host.store
                .snapshot()
                .contains_key(&format!("balance:{addr_b:#x}"))
        );
    }

    #[test]
    fn balance_change_at_or_above_threshold_warns() {
        let host = MockHost::new();
        let settings = one_addr_settings(1_000);
        let addr = settings.addresses[0];
        // Pre-seed prior balance = 100.
        host.store
            .set(
                &format!("balance:{addr:#x}"),
                &u256_to_le_bytes(U256::from(100u64)),
            )
            .unwrap();
        let params = format!("[\"{addr:#x}\",\"latest\"]");
        host.chain.respond_to(
            "eth_getBalance",
            &params,
            Ok(encode_balance_response(1_150)),
        );

        let (result, logs) = capture_tracing(|| on_block(&host, SEPOLIA, &settings));
        result.unwrap();

        // Delta of 1_050 meets the 1_000 threshold: exactly one Warn.
        let ev = logs.expect_one(|e| e.level == Level::WARN);
        assert!(ev.message.contains(&format!("{addr:#x}")));
        assert!(ev.message.contains("changed +1050 wei"));
        assert!(ev.message.contains("prior=100"));
        assert!(ev.message.contains("current=1150"));
        // New reading persisted for the next block's diff.
        let stored = host
            .store
            .snapshot()
            .get(&format!("balance:{addr:#x}"))
            .cloned()
            .unwrap();
        assert_eq!(parse_u256_le(&stored), Some(U256::from(1_150u64)));
    }
}
