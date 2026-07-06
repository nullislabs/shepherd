//! Pure strategy logic for the price-alert module.
//!
//! Every interaction with the world flows through the [`Host`] trait
//! seam exposed by `nexum-sdk` - no direct calls to wit-bindgen-
//! generated free functions live here. The `lib.rs` glue wraps a
//! `WitBindgenHost` adapter around the module's per-cdylib wit-bindgen
//! imports and hands it to [`on_block`]; tests under `#[cfg(test)]`
//! hand the same function a `nexum_sdk_test::MockHost`.

use alloy_primitives::I256;
use nexum_sdk::chain::chainlink::read_latest_answer;
use nexum_sdk::config::{self, ConfigError};
use nexum_sdk::host::{Fault, Host};
use nexum_sdk::prelude::Address;

/// Resolved configuration, parsed from `module.toml::[config]` at
/// `init` and read on every `on_event`.
#[derive(Debug)]
pub struct Settings {
    /// Chainlink AggregatorV3Interface address.
    pub oracle_address: Address,
    /// Threshold scaled to the oracle's native units
    /// (`threshold_decimal * 10**decimals`).
    pub threshold_scaled: I256,
    /// Which side of the threshold fires.
    pub direction: Direction,
    /// Throttle: only poll every Nth block.
    pub every_n_blocks: u64,
}

/// Which side of the threshold the alert fires on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    /// Fire when `answer >= threshold`.
    Above,
    /// Fire when `answer <= threshold`.
    Below,
}

/// React to a new block.
///
/// Returns `Ok(())` on success and on recoverable upstream failures
/// (oracle RPC error, decode failure) - the strategy logs a Warn and
/// lets the next block re-poll rather than propagating into the
/// supervisor. Only host-level I/O on the persistence side would
/// bubble up via `?`, and this module does not touch the store.
pub fn on_block<H: Host>(
    host: &H,
    chain_id: u64,
    settings: &Settings,
    block_number: u64,
) -> Result<(), Fault> {
    if !block_number.is_multiple_of(settings.every_n_blocks) {
        return Ok(());
    }
    let Some(answer) = read_latest_answer(host, chain_id, settings.oracle_address, "price-alert")
    else {
        // read_latest_answer already logged the failure at Warn.
        return Ok(());
    };
    if classify(answer, settings.threshold_scaled, settings.direction) {
        tracing::warn!(
            answer = %answer,
            threshold = %settings.threshold_scaled,
            direction = ?settings.direction,
            "price-alert: TRIGGERED",
        );
    } else {
        tracing::info!(
            answer = %answer,
            threshold = %settings.threshold_scaled,
            direction = ?settings.direction,
            "price-alert: ok",
        );
    }
    Ok(())
}

/// `true` when `answer` is on the firing side of `threshold` per
/// `direction`. Pure - exercised by the unit tests.
pub fn classify(answer: I256, threshold: I256, direction: Direction) -> bool {
    match direction {
        Direction::Above => answer >= threshold,
        Direction::Below => answer <= threshold,
    }
}

/// Parse `module.toml::[config]` into a typed [`Settings`].
///
/// One-shot config-parser style: returns `Result<T, Fault>` so the
/// `Guest::init` adapter can lower the failure into the wit-bindgen
/// `fault` with no extra plumbing.
pub fn parse_config(entries: &[(String, String)]) -> Result<Settings, Fault> {
    let oracle_address = config::get_required(entries, "oracle_address")
        .map_err(config_err)?
        .parse::<Address>()
        .map_err(|e| invalid(format!("oracle_address: {e}")))?;
    let decimals = config::get_required(entries, "decimals")
        .map_err(config_err)?
        .parse::<u32>()
        .map_err(|e| invalid(format!("decimals: {e}")))?;
    if decimals > 38 {
        return Err(invalid(format!(
            "decimals={decimals} exceeds the I256 power-of-ten budget"
        )));
    }
    let threshold_decimal = config::get_required(entries, "threshold").map_err(config_err)?;
    let threshold_scaled =
        config::scale_decimal(threshold_decimal, decimals, "threshold").map_err(config_err)?;
    let direction = match config::get_required(entries, "direction")
        .map_err(config_err)?
        .to_ascii_lowercase()
        .as_str()
    {
        "above" => Direction::Above,
        "below" => Direction::Below,
        other => {
            return Err(invalid(format!(
                "direction: expected 'above'|'below', got {other:?}"
            )));
        }
    };
    let every_n_blocks = config::get_optional(entries, "every_n_blocks")
        .map(|s| {
            s.parse::<u64>()
                .map_err(|e| invalid(format!("every_n_blocks: {e}")))
        })
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

/// Lift a free-text invalid-config detail into a [`Fault::InvalidInput`].
/// Used when the SDK helper does not own the error (e.g. an
/// `Address::from_str` failure).
fn invalid(message: impl Into<String>) -> Fault {
    Fault::InvalidInput(message.into())
}

/// Project a `nexum_sdk::config::ConfigError` into a
/// [`Fault::InvalidInput`] via `Display`, preserving the detail at the
/// WIT boundary.
fn config_err(e: ConfigError) -> Fault {
    invalid(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{U256, hex};
    use alloy_sol_types::SolCall;
    use nexum_sdk::Level;
    use nexum_sdk::chain::chainlink::AggregatorV3;
    use nexum_sdk::chain::eth_call_params;
    use nexum_sdk::host::{ChainError, Fault};
    use nexum_sdk_test::{MockHost, capture_tracing};

    fn sample_settings(trigger_scaled_dec: i128, direction: Direction) -> Settings {
        Settings {
            oracle_address: "0x694AA1769357215DE4FAC081bf1f309aDC325306"
                .parse()
                .unwrap(),
            threshold_scaled: I256::try_from(trigger_scaled_dec).unwrap(),
            direction,
            every_n_blocks: 1,
        }
    }

    /// Encode a `latestRoundData` return into the `"0x..."` JSON string
    /// the host's `chain::request` would yield.
    fn oracle_response_json(answer_scaled: i128) -> String {
        use alloy_primitives::aliases::U80;
        let returns = AggregatorV3::latestRoundDataReturn {
            roundId: U80::ZERO,
            answer: I256::try_from(answer_scaled).unwrap(),
            startedAt: U256::ZERO,
            updatedAt: U256::ZERO,
            answeredInRound: U80::ZERO,
        };
        let encoded = AggregatorV3::latestRoundDataCall::abi_encode_returns(&returns);
        let hex = hex::encode_prefixed(encoded);
        format!("\"{hex}\"")
    }

    fn programmed_eth_call(host: &MockHost, oracle: Address, response: Result<String, ChainError>) {
        let call_data = AggregatorV3::latestRoundDataCall {}.abi_encode();
        let params = eth_call_params(&oracle, &call_data);
        host.chain.respond_to("eth_call", &params, response);
    }

    // ---- pure helpers ----

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

    // Decimal-parsing tests for the shared scaler live in
    // `nexum-sdk::config::tests` now (lifted out of this module per
    // PR #55 review). The integration-level parse_config tests below
    // still exercise the wiring end-to-end with the SDK helper.

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
    fn parse_config_rejects_missing_key() {
        let entries = vec![
            ("decimals".into(), "8".into()),
            ("threshold".into(), "1".into()),
            ("direction".into(), "above".into()),
        ];
        let err = parse_config(&entries).unwrap_err();
        let Fault::InvalidInput(message) = err else {
            panic!("expected invalid-input fault, got {err:?}");
        };
        assert!(message.contains("oracle_address"));
    }

    // ---- strategy behaviour against MockHost ----

    #[test]
    fn on_block_idle_when_price_above_below_trigger() {
        let host = MockHost::new();
        let settings = sample_settings(/*trigger*/ 250_050_000_000, Direction::Below);
        programmed_eth_call(
            &host,
            settings.oracle_address,
            Ok(oracle_response_json(300_000_000_000)),
        );

        let (result, logs) = capture_tracing(|| on_block(&host, 11_155_111, &settings, 100));
        result.unwrap();

        assert_eq!(host.chain.call_count(), 1);
        assert_eq!(logs.count_at(Level::WARN), 0);
        let ev = logs.expect_one(|e| e.level == Level::INFO && e.message == "price-alert: ok");
        assert!(ev.field("answer").is_some());
        assert_eq!(ev.field_str("threshold").as_deref(), Some("250050000000"));
    }

    #[test]
    fn on_block_triggers_below_threshold() {
        let host = MockHost::new();
        let settings = sample_settings(250_050_000_000, Direction::Below);
        programmed_eth_call(
            &host,
            settings.oracle_address,
            Ok(oracle_response_json(200_000_000_000)),
        );

        let (result, logs) = capture_tracing(|| on_block(&host, 11_155_111, &settings, 100));
        result.unwrap();

        // `expect_one` on the WARN level pins the single-alert count.
        let ev = logs.expect_one(|e| e.level == Level::WARN);
        assert_eq!(ev.message, "price-alert: TRIGGERED");
        assert_eq!(ev.field_str("direction").as_deref(), Some("Below"));
        assert_eq!(ev.field_str("answer").as_deref(), Some("200000000000"));
    }

    #[test]
    fn on_block_triggers_above_threshold() {
        let host = MockHost::new();
        let settings = sample_settings(100, Direction::Above);
        programmed_eth_call(
            &host,
            settings.oracle_address,
            Ok(oracle_response_json(200)),
        );

        let (result, logs) = capture_tracing(|| on_block(&host, 11_155_111, &settings, 100));
        result.unwrap();

        let ev = logs.expect_one(|e| e.level == Level::WARN);
        assert_eq!(ev.message, "price-alert: TRIGGERED");
        assert_eq!(ev.field_str("direction").as_deref(), Some("Above"));
    }

    #[test]
    fn on_block_warns_and_continues_on_rpc_error() {
        let host = MockHost::new();
        let settings = sample_settings(100, Direction::Below);
        programmed_eth_call(
            &host,
            settings.oracle_address,
            Err(ChainError::Fault(Fault::Timeout)),
        );

        // Strategy returns Ok so the supervisor moves on.
        let (result, logs) = capture_tracing(|| on_block(&host, 11_155_111, &settings, 100));
        result.unwrap();
        // The oracle-read failure is logged by the SDK chainlink helper
        // through the host logging call, so it lands on `host.logging`.
        assert!(host.logging.contains("eth_call failed"));
        // No facade event at all: the strategy returns before emitting
        // either the ok or TRIGGERED line.
        assert!(logs.is_empty());
    }

    #[test]
    fn on_block_warns_on_undecodable_result() {
        let host = MockHost::new();
        let settings = sample_settings(100, Direction::Below);
        programmed_eth_call(&host, settings.oracle_address, Ok("not-json".into()));

        on_block(&host, 11_155_111, &settings, 100).unwrap();
        assert!(host.logging.contains("cannot decode result hex"));
    }

    #[test]
    fn on_block_respects_every_n_blocks_throttle() {
        let host = MockHost::new();
        let mut settings = sample_settings(100, Direction::Below);
        settings.every_n_blocks = 5;
        programmed_eth_call(&host, settings.oracle_address, Ok(oracle_response_json(50)));

        // Blocks 1..5 do not poll; only block 5 (which divides evenly).
        for n in 1..5 {
            on_block(&host, 11_155_111, &settings, n).unwrap();
        }
        assert_eq!(host.chain.call_count(), 0);

        on_block(&host, 11_155_111, &settings, 5).unwrap();
        assert_eq!(host.chain.call_count(), 1);
    }
}
