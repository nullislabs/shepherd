//! # price-alert (example Shepherd module)
//!
//! Polls a Chainlink price oracle on every new block and emits a
//! Warn-level log when the price crosses a config-supplied
//! threshold. Demonstrates the three load-bearing patterns of a
//! Shepherd module:
//!
//! - `chain::request` + ABI decode via `alloy_sol_types`
//! - `shepherd_sdk` helpers (`prelude`, `chain::eth_call_params`,
//!   `chain::parse_eth_call_result`)
//! - `[config]` driven behaviour parsed once in `init` and read on
//!   every subsequent event
//!
//! ## Settings
//!
//! ```toml
//! [config]
//! # Chainlink AggregatorV3Interface address.
//! oracle_address = "0x694AA1769357215DE4FAC081bf1f309aDC325306"  # ETH/USD on Sepolia
//! # Oracle's decimals (Chainlink USD pairs are 8; ETH pairs 18).
//! decimals = "8"
//! # Threshold in the oracle's native units (decimal string). The
//! # module multiplies by 10**decimals at init.
//! threshold = "2500.00"
//! # Either "above" or "below". Fires when the answer crosses on
//! # the configured side.
//! direction = "below"
//! # Optional throttle: poll every N blocks. Default 1.
//! every_n_blocks = "1"
//! ```

// wit_bindgen::generate! expands to host-import shims whose arity matches
// the WIT signatures, which can exceed clippy's too-many-arguments threshold.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: ["../../../wit/nexum-host", "../../../wit/shepherd-cow"],
    world: "shepherd:cow/shepherd",
    generate_all,
});

use std::sync::OnceLock;

use alloy_primitives::{Address, I256, U256};
use alloy_sol_types::{SolCall, sol};
use shepherd_sdk::chain::{eth_call_params, parse_eth_call_result};

use nexum::host::types::HostErrorKind;
use nexum::host::{chain, logging, types};

sol! {
    /// Chainlink AggregatorV3Interface - only the function this
    /// module needs.
    interface AggregatorV3 {
        function latestRoundData() external view returns (
            uint80 roundId,
            int256 answer,
            uint256 startedAt,
            uint256 updatedAt,
            uint80 answeredInRound
        );
    }
}

/// Resolved configuration, parsed from `module.toml::[config]` at
/// `init` and read on every `on_event`. Stored in a `OnceLock` so
/// the module is single-init by construction.
#[derive(Debug)]
struct Settings {
    oracle_address: Address,
    /// Threshold scaled to the oracle's native units
    /// (`threshold_decimal * 10**decimals`).
    threshold_scaled: I256,
    direction: Direction,
    every_n_blocks: u64,
}

/// Which side of the threshold the alert fires on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Direction {
    /// Fire when `answer >= threshold`.
    Above,
    /// Fire when `answer <= threshold`.
    Below,
}

static CONFIG: OnceLock<Settings> = OnceLock::new();

struct PriceAlert;

impl Guest for PriceAlert {
    fn init(config: Vec<(String, String)>) -> Result<(), HostError> {
        match parse_config(&config) {
            Ok(cfg) => {
                logging::log(
                    logging::Level::Info,
                    &format!(
                        "price-alert init: oracle={:#x} threshold={} direction={:?} every_n_blocks={}",
                        cfg.oracle_address, cfg.threshold_scaled, cfg.direction, cfg.every_n_blocks,
                    ),
                );
                // OnceLock::set fails only if already set - in a
                // single-init module that means a re-entry from the
                // supervisor, which is not a hard error; we keep the
                // first parse.
                let _ = CONFIG.set(cfg);
                Ok(())
            }
            Err(e) => Err(HostError {
                domain: "price-alert".into(),
                kind: HostErrorKind::InvalidInput,
                code: 0,
                message: format!("price-alert: invalid [config]: {e}"),
                data: None,
            }),
        }
    }

    fn on_event(event: types::Event) -> Result<(), HostError> {
        let Some(cfg) = CONFIG.get() else {
            return Ok(()); // init failed; no-op until a fresh load.
        };
        if let types::Event::Block(block) = event {
            if block.number % cfg.every_n_blocks != 0 {
                return Ok(());
            }
            poll_oracle(block.chain_id, cfg);
        }
        // Logs / Tick / Message are not used by this example.
        Ok(())
    }
}

/// Build + dispatch the `latestRoundData` eth_call. Result is
/// logged: Info if the threshold is not crossed, Warn if it is.
/// Returns nothing so a single bad RPC reply does not propagate
/// into the supervisor - the next block re-polls.
fn poll_oracle(chain_id: u64, cfg: &Settings) {
    let call_data = AggregatorV3::latestRoundDataCall {}.abi_encode();
    let params = eth_call_params(&cfg.oracle_address, &call_data);
    let result_json = match chain::request(chain_id, "eth_call", &params) {
        Ok(s) => s,
        Err(err) => {
            logging::log(
                logging::Level::Warn,
                &format!(
                    "price-alert eth_call failed ({}): {}",
                    err.code, err.message
                ),
            );
            return;
        }
    };
    let Some(bytes) = parse_eth_call_result(&result_json) else {
        logging::log(
            logging::Level::Warn,
            &format!("price-alert: cannot decode result hex {result_json}"),
        );
        return;
    };
    let decoded = match AggregatorV3::latestRoundDataCall::abi_decode_returns(&bytes) {
        Ok(d) => d,
        Err(e) => {
            logging::log(
                logging::Level::Warn,
                &format!("price-alert: latestRoundData decode failed: {e}"),
            );
            return;
        }
    };
    let answer = decoded.answer;
    if classify(answer, cfg.threshold_scaled, cfg.direction) {
        logging::log(
            logging::Level::Warn,
            &format!(
                "price-alert: TRIGGERED answer={answer} threshold={} ({:?})",
                cfg.threshold_scaled, cfg.direction,
            ),
        );
    } else {
        logging::log(
            logging::Level::Info,
            &format!(
                "price-alert: ok answer={answer} threshold={} ({:?})",
                cfg.threshold_scaled, cfg.direction,
            ),
        );
    }
}

/// `true` when `answer` is on the firing side of `threshold` per
/// `direction`. Pure - exercised by the unit tests.
fn classify(answer: I256, threshold: I256, direction: Direction) -> bool {
    match direction {
        Direction::Above => answer >= threshold,
        Direction::Below => answer <= threshold,
    }
}

/// Parse `module.toml::[config]` into a typed [`Settings`]. Returns a
/// human-readable error string the engine surfaces under
/// `host_error.message`.
fn parse_config(entries: &[(String, String)]) -> Result<Settings, String> {
    let oracle_address = config_get(entries, "oracle_address")?
        .parse::<Address>()
        .map_err(|e| format!("oracle_address: {e}"))?;
    let decimals = config_get(entries, "decimals")?
        .parse::<u32>()
        .map_err(|e| format!("decimals: {e}"))?;
    if decimals > 38 {
        return Err(format!(
            "decimals={decimals} exceeds the I256 power-of-ten budget"
        ));
    }
    let threshold_decimal = config_get(entries, "threshold")?;
    let threshold_scaled = scale_threshold(threshold_decimal, decimals)?;
    let direction = match config_get(entries, "direction")?
        .to_ascii_lowercase()
        .as_str()
    {
        "above" => Direction::Above,
        "below" => Direction::Below,
        other => {
            return Err(format!(
                "direction: expected 'above'|'below', got {other:?}"
            ));
        }
    };
    let every_n_blocks = config_get_optional(entries, "every_n_blocks")
        .map(|s| s.parse::<u64>().map_err(|e| format!("every_n_blocks: {e}")))
        .transpose()?
        .unwrap_or(1)
        .max(1);
    Ok(Settings {
        oracle_address,
        threshold_scaled,
        direction,
        every_n_blocks,
    })
}

fn config_get<'a>(entries: &'a [(String, String)], key: &str) -> Result<&'a str, String> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .ok_or_else(|| format!("missing key {key:?}"))
}

fn config_get_optional<'a>(entries: &'a [(String, String)], key: &str) -> Option<&'a str> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// Multiply `threshold_decimal` (e.g. `"2500.00"`) by `10**decimals`
/// into an `I256` for direct comparison with the oracle's answer.
/// Hand-rolled because alloy does not ship a `Decimal::parse_units`-
/// style helper and the module needs to stay no-std-ish.
fn scale_threshold(threshold_decimal: &str, decimals: u32) -> Result<I256, String> {
    let (sign, body) = if let Some(rest) = threshold_decimal.strip_prefix('-') {
        (-1i32, rest)
    } else {
        (1, threshold_decimal)
    };
    let (whole, frac) = match body.split_once('.') {
        Some((w, f)) => (w, f),
        None => (body, ""),
    };
    if whole.is_empty() && frac.is_empty() {
        return Err("threshold: empty".into());
    }
    if !whole.chars().all(|c| c.is_ascii_digit()) || !frac.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!(
            "threshold: non-digit character in {threshold_decimal:?}"
        ));
    }
    // Compose the un-scaled integer string, padding / truncating the
    // fractional part against `decimals`.
    let frac_len = frac.len() as u32;
    let composed: String = if frac_len <= decimals {
        let mut s = String::with_capacity(whole.len() + decimals as usize);
        s.push_str(whole);
        s.push_str(frac);
        // Pad with zeros for the missing fractional digits.
        for _ in 0..(decimals - frac_len) {
            s.push('0');
        }
        s
    } else {
        // Fractional part is longer than `decimals` - truncate
        // (chops trailing digits; deliberately not rounding to keep
        // behaviour predictable).
        let mut s = String::with_capacity(whole.len() + decimals as usize);
        s.push_str(whole);
        s.push_str(&frac[..decimals as usize]);
        s
    };
    let raw = if composed.is_empty() { "0" } else { &composed };
    let unsigned: U256 = raw.parse().map_err(|e| format!("threshold parse: {e}"))?;
    let signed = I256::try_from(unsigned).map_err(|e| format!("threshold range: {e}"))?;
    Ok(if sign < 0 { -signed } else { signed })
}

export!(PriceAlert);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_config_happy_path() {
        let entries = vec![
            (
                "oracle_address".into(),
                "0x694AA1769357215DE4FAC081bf1f309aDC325306".into(),
            ),
            ("decimals".into(), "8".into()),
            ("threshold".into(), "2500.50".into()),
            ("direction".into(), "below".into()),
            ("every_n_blocks".into(), "5".into()),
        ];
        let cfg = parse_config(&entries).unwrap();
        assert_eq!(cfg.direction, Direction::Below);
        assert_eq!(cfg.every_n_blocks, 5);
        // 2500.50 with 8 decimals = 2500_50000000 = 250_050_000_000
        assert_eq!(
            cfg.threshold_scaled,
            I256::try_from(250_050_000_000_i64).unwrap()
        );
    }

    #[test]
    fn parse_config_defaults_every_n_blocks_to_one() {
        let entries = vec![
            (
                "oracle_address".into(),
                "0x694AA1769357215DE4FAC081bf1f309aDC325306".into(),
            ),
            ("decimals".into(), "8".into()),
            ("threshold".into(), "1".into()),
            ("direction".into(), "above".into()),
        ];
        let cfg = parse_config(&entries).unwrap();
        assert_eq!(cfg.every_n_blocks, 1);
        assert_eq!(cfg.direction, Direction::Above);
    }

    #[test]
    fn parse_config_rejects_unknown_direction() {
        let entries = vec![
            (
                "oracle_address".into(),
                "0x694AA1769357215DE4FAC081bf1f309aDC325306".into(),
            ),
            ("decimals".into(), "8".into()),
            ("threshold".into(), "1".into()),
            ("direction".into(), "sideways".into()),
        ];
        assert!(parse_config(&entries).is_err());
    }

    #[test]
    fn parse_config_rejects_missing_key() {
        let entries = vec![
            ("decimals".into(), "8".into()),
            ("threshold".into(), "1".into()),
            ("direction".into(), "above".into()),
        ];
        let err = parse_config(&entries).unwrap_err();
        assert!(err.contains("oracle_address"));
    }

    #[test]
    fn scale_threshold_pads_short_fractional() {
        assert_eq!(
            scale_threshold("1.5", 8).unwrap(),
            I256::try_from(150_000_000_i64).unwrap()
        );
    }

    #[test]
    fn scale_threshold_truncates_long_fractional() {
        // "1.123456789" with 8 decimals truncates to "1.12345678".
        assert_eq!(
            scale_threshold("1.123456789", 8).unwrap(),
            I256::try_from(112_345_678_i64).unwrap(),
        );
    }

    #[test]
    fn scale_threshold_handles_no_decimal_point() {
        assert_eq!(
            scale_threshold("42", 8).unwrap(),
            I256::try_from(4_200_000_000_i64).unwrap()
        );
    }

    #[test]
    fn scale_threshold_handles_negative_values() {
        // Useful for non-USD pairs (yield curves, basis spreads, etc.).
        assert_eq!(
            scale_threshold("-1.5", 8).unwrap(),
            -I256::try_from(150_000_000_i64).unwrap(),
        );
    }

    #[test]
    fn scale_threshold_rejects_garbage() {
        assert!(scale_threshold("abc", 8).is_err());
        assert!(scale_threshold("1.2.3", 8).is_err());
    }

    #[test]
    fn classify_below_fires_at_or_under_threshold() {
        let t = I256::try_from(100_i32).unwrap();
        assert!(classify(
            I256::try_from(99_i32).unwrap(),
            t,
            Direction::Below
        ));
        assert!(classify(
            I256::try_from(100_i32).unwrap(),
            t,
            Direction::Below
        ));
        assert!(!classify(
            I256::try_from(101_i32).unwrap(),
            t,
            Direction::Below
        ));
    }

    #[test]
    fn classify_above_fires_at_or_over_threshold() {
        let t = I256::try_from(100_i32).unwrap();
        assert!(classify(
            I256::try_from(101_i32).unwrap(),
            t,
            Direction::Above
        ));
        assert!(classify(
            I256::try_from(100_i32).unwrap(),
            t,
            Direction::Above
        ));
        assert!(!classify(
            I256::try_from(99_i32).unwrap(),
            t,
            Direction::Above
        ));
    }
}
