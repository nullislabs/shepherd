//! Pure stop-loss strategy logic. Reads an oracle, optionally submits
//! a pre-signed CoW order, dedups via local-store. Every interaction
//! with the world flows through the [`Host`] trait so the tests can
//! drive it against `shepherd_sdk_test::MockHost`.

use alloy_primitives::I256;
use shepherd_sdk::chain::chainlink::read_latest_answer;
use shepherd_sdk::config::{self, ConfigError};
use shepherd_sdk::cow::{RetryAction, classify_api_error, gpv2_to_order_data};
use shepherd_sdk::host::{Host, HostError, HostErrorKind, LogLevel};
use shepherd_sdk::prelude::{
    Address, BuyTokenDestination, Bytes, Chain, EMPTY_APP_DATA_JSON, GPv2OrderData, OrderCreation,
    OrderKind, OrderUid, SellTokenSource, Signature, U256,
};

/// Resolved configuration parsed from `module.toml::[config]`.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Chainlink AggregatorV3Interface address.
    pub oracle_address: Address,
    /// Trigger price scaled to the oracle's native units.
    pub trigger_price_scaled: I256,
    /// Order owner (= EIP-712 signer / PreSign caller).
    pub owner: Address,
    /// Sell side of the order.
    pub sell_token: Address,
    /// Buy side of the order.
    pub buy_token: Address,
    /// Sell amount in atomic units of `sell_token`.
    pub sell_amount: U256,
    /// Buy amount in atomic units of `buy_token`.
    pub buy_amount: U256,
    /// Order expiry (Unix seconds).
    pub valid_to: u32,
}

/// React to a new block.
///
/// Returns `Ok(())` on success and on recoverable upstream failures
/// (oracle RPC error, decode failure). Only host-store errors bubble
/// up via `?` so the supervisor can surface persistence issues - all
/// other faults log and let the next block re-poll.
pub fn on_block<H: Host>(host: &H, chain_id: u64, settings: &Settings) -> Result<(), HostError> {
    let price = match read_latest_answer(host, chain_id, settings.oracle_address, "stop-loss") {
        Some(p) => p,
        None => return Ok(()), // logged inside read_latest_answer
    };

    if price > settings.trigger_price_scaled {
        host.log(
            LogLevel::Info,
            &format!(
                "stop-loss idle: price={price} > trigger={}",
                settings.trigger_price_scaled,
            ),
        );
        return Ok(());
    }

    // Compute UID up-front so we can dedup before paying for the
    // serialise + submit round trip.
    let (creation, uid) = match build_creation(chain_id, settings) {
        Ok(x) => x,
        Err(e) => {
            host.log(LogLevel::Warn, &format!("stop-loss skipped (build): {e}"));
            return Ok(());
        }
    };
    let uid_hex = format!("{uid}");
    let dedup_key = format!("submitted:{uid_hex}");
    if host.get(&dedup_key)?.is_some() {
        host.log(
            LogLevel::Info,
            &format!("stop-loss: {uid_hex} already submitted, idle"),
        );
        return Ok(());
    }
    let dropped_key = format!("dropped:{uid_hex}");
    if host.get(&dropped_key)?.is_some() {
        host.log(
            LogLevel::Info,
            &format!("stop-loss: {uid_hex} previously dropped, idle"),
        );
        return Ok(());
    }

    let body = match serde_json::to_vec(&creation) {
        Ok(b) => b,
        Err(e) => {
            host.log(
                LogLevel::Error,
                &format!("OrderCreation JSON encode failed: {e}"),
            );
            return Ok(());
        }
    };
    match host.submit_order(chain_id, &body) {
        Ok(server_uid) => {
            if server_uid != uid_hex {
                host.log(
                    LogLevel::Warn,
                    &format!("stop-loss uid drift: local={uid_hex} server={server_uid}"),
                );
            }
            host.set(&format!("submitted:{server_uid}"), b"")?;
            host.log(
                LogLevel::Warn,
                &format!(
                    "stop-loss TRIGGERED: price={price} <= trigger={}, uid={server_uid}",
                    settings.trigger_price_scaled,
                ),
            );
        }
        Err(err) => match classify_api_error(err.data.as_deref()) {
            RetryAction::TryNextBlock | RetryAction::Backoff { .. } => {
                host.log(
                    LogLevel::Warn,
                    &format!(
                        "stop-loss retry on next block ({}): {}",
                        err.code, err.message
                    ),
                );
            }
            RetryAction::Drop => {
                host.set(&dropped_key, b"")?;
                host.log(
                    LogLevel::Warn,
                    &format!(
                        "stop-loss dropped {uid_hex} ({}): {}",
                        err.code, err.message
                    ),
                );
            }
            // `RetryAction` is `#[non_exhaustive]`; treat unknown
            // future variants like `TryNextBlock` rather than
            // silently dropping the watch on an SDK bump.
            _ => {
                host.log(
                    LogLevel::Warn,
                    &format!(
                        "stop-loss unknown retry-action ({}): {} - retry on next block",
                        err.code, err.message
                    ),
                );
            }
        },
    }
    Ok(())
}

// `read_oracle` moved into `shepherd_sdk::chain::chainlink::read_latest_answer`
// (PR #55 review): the same flow + `Option<I256>` return shape now serves
// price-alert + stop-loss from the SDK, with `domain: &str` carrying the
// module label into the Warn log.

/// Assemble the `OrderCreation` body + canonical UID from settings.
/// Uses `Signature::PreSign` so the module ships zero ECDSA - the
/// owner is expected to have called `GPv2Signing.setPreSignature`
/// on-chain ahead of the trigger.
fn build_creation(
    chain_id: u64,
    settings: &Settings,
) -> Result<(OrderCreation, OrderUid), HostError> {
    let chain = Chain::try_from(chain_id).map_err(|_| HostError {
        domain: "stop-loss".into(),
        kind: HostErrorKind::Unsupported,
        code: 0,
        message: format!("chain {chain_id} not supported by cowprotocol"),
        data: None,
    })?;
    let domain = chain.settlement_domain();
    let gpv2 = GPv2OrderData {
        sellToken: settings.sell_token,
        buyToken: settings.buy_token,
        receiver: settings.owner,
        sellAmount: settings.sell_amount,
        buyAmount: settings.buy_amount,
        validTo: settings.valid_to,
        appData: cowprotocol::EMPTY_APP_DATA_HASH,
        feeAmount: U256::ZERO,
        kind: OrderKind::SELL,
        partiallyFillable: false,
        sellTokenBalance: SellTokenSource::ERC20,
        buyTokenBalance: BuyTokenDestination::ERC20,
    };
    let order_data = gpv2_to_order_data(&gpv2).ok_or_else(|| HostError {
        domain: "stop-loss".into(),
        kind: HostErrorKind::InvalidInput,
        code: 0,
        message: "GPv2OrderData carried an unknown enum marker".into(),
        data: None,
    })?;
    let uid = order_data.uid(&domain, settings.owner);
    let creation = OrderCreation::from_signed_order_data(
        &order_data,
        Signature::PreSign,
        settings.owner,
        EMPTY_APP_DATA_JSON.to_string(),
        None,
    )
    .map_err(|e| HostError {
        domain: "stop-loss".into(),
        kind: HostErrorKind::InvalidInput,
        code: 0,
        message: format!("cowprotocol rejected the body: {e}"),
        data: None,
    })?;
    // Silence the unused `Bytes` import on builds where `Signature::
    // PreSign` is the only signature variant we construct.
    let _: Option<Bytes> = None;
    Ok((creation, uid))
}

/// Parse `module.toml::[config]` into a typed [`Settings`].
pub fn parse_config(entries: &[(String, String)]) -> Result<Settings, HostError> {
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
    let trigger_price_scaled = config::scale_decimal(
        config::get_required(entries, "trigger_price").map_err(config_err)?,
        decimals,
        "trigger_price",
    )
    .map_err(config_err)?;
    let owner = config::get_required(entries, "owner")
        .map_err(config_err)?
        .parse::<Address>()
        .map_err(|e| invalid(format!("owner: {e}")))?;
    let sell_token = config::get_required(entries, "sell_token")
        .map_err(config_err)?
        .parse::<Address>()
        .map_err(|e| invalid(format!("sell_token: {e}")))?;
    let buy_token = config::get_required(entries, "buy_token")
        .map_err(config_err)?
        .parse::<Address>()
        .map_err(|e| invalid(format!("buy_token: {e}")))?;
    let sell_amount = config::get_required(entries, "sell_amount_wei")
        .map_err(config_err)?
        .parse::<U256>()
        .map_err(|e| invalid(format!("sell_amount_wei: {e}")))?;
    let buy_amount = config::get_required(entries, "buy_amount_wei")
        .map_err(config_err)?
        .parse::<U256>()
        .map_err(|e| invalid(format!("buy_amount_wei: {e}")))?;
    let valid_to = config::get_required(entries, "valid_to_seconds")
        .map_err(config_err)?
        .parse::<u32>()
        .map_err(|e| invalid(format!("valid_to_seconds: {e}")))?;
    Ok(Settings {
        oracle_address,
        trigger_price_scaled,
        owner,
        sell_token,
        buy_token,
        sell_amount,
        buy_amount,
        valid_to,
    })
}

/// Lift a free-text invalid-config detail into the stop-loss `HostError`
/// shape. Used when the SDK helper does not own the error (e.g. an
/// `Address::from_str` failure or a `U256::from_str` overflow).
fn invalid(message: impl Into<String>) -> HostError {
    HostError {
        domain: "stop-loss".into(),
        kind: HostErrorKind::InvalidInput,
        code: 0,
        message: format!("stop-loss: invalid [config]: {}", message.into()),
        data: None,
    }
}

/// Project a `shepherd_sdk::config::ConfigError` into the stop-loss
/// `HostError` shape via `Display`.
fn config_err(e: ConfigError) -> HostError {
    invalid(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::hex;
    use alloy_sol_types::SolCall;
    use shepherd_sdk::chain::chainlink::AggregatorV3;
    use shepherd_sdk::chain::eth_call_params;
    use shepherd_sdk::host::HostErrorKind as Kind;
    use shepherd_sdk_test::MockHost;

    const SEPOLIA: u64 = 11_155_111;

    fn settings_below(trigger_scaled: i128) -> Settings {
        Settings {
            oracle_address: "0x694AA1769357215DE4FAC081bf1f309aDC325306"
                .parse()
                .unwrap(),
            trigger_price_scaled: I256::try_from(trigger_scaled).unwrap(),
            owner: "0x70997970C51812dc3A010C7d01b50e0d17dc79C8"
                .parse()
                .unwrap(),
            sell_token: "0x6810e776880C02933D47DB1b9fc05908e5386b96"
                .parse()
                .unwrap(),
            buy_token: "0xfff9976782d46cc05630d1f6ebab18b2324d6b14"
                .parse()
                .unwrap(),
            sell_amount: U256::from(1_000_000_000_000_000_000_u128),
            buy_amount: U256::from(300_000_000_000_000_000_u128),
            valid_to: u32::MAX,
        }
    }

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
        let hex_body = hex::encode_prefixed(encoded);
        format!("\"{hex_body}\"")
    }

    fn program_oracle(host: &MockHost, oracle: Address, response: Result<String, HostError>) {
        let call_data = AggregatorV3::latestRoundDataCall {}.abi_encode();
        let params = eth_call_params(&oracle, &call_data);
        host.chain.respond_to("eth_call", &params, response);
    }

    fn programmed_uid(settings: &Settings) -> String {
        let (_creation, uid) = build_creation(SEPOLIA, settings).unwrap();
        format!("{uid}")
    }

    /// Regression test pinning the OrderUid produced by the
    /// E2E run's `modules/examples/stop-loss/module.toml` config so an
    /// operator can `setPreSignature(uid, true)` ahead of the run
    /// without re-deriving the UID from the EIP-712 / domain-
    /// separator dance. If this assertion ever flips, either:
    ///   (a) the module.toml has drifted from the pinned settings, or
    ///   (b) the EIP-712 type-hash / domain-separator changed,
    /// and the runbook's `setPreSignature` step needs the new UID.
    #[test]
    fn e2e_settings_yield_expected_uid() {
        let settings = Settings {
            oracle_address: "0x694AA1769357215DE4FAC081bf1f309aDC325306"
                .parse()
                .unwrap(),
            trigger_price_scaled: I256::try_from(200_000_000_000_i128).unwrap(),
            owner: "0x7bF140727D27ea64b607E042f1225680B40ECa6A"
                .parse()
                .unwrap(),
            sell_token: "0xfFf9976782d46CC05630D1f6eBAb18b2324d6B14"
                .parse()
                .unwrap(),
            buy_token: "0x0625aFB445C3B6B7B929342a04A22599fd5dBB59"
                .parse()
                .unwrap(),
            sell_amount: U256::from(5_000_000_000_000_000_u128),
            buy_amount: U256::from(20_000_000_000_000_000_000_u128),
            valid_to: u32::MAX,
        };
        assert_eq!(
            programmed_uid(&settings),
            "0xc2b9cb4ea1ee5a86d8049ac09d8f494bf04cca0a68407285f31e2e6379800be87bf140727d27ea64b607e042f1225680b40eca6affffffff",
        );
    }

    #[test]
    fn idle_when_price_above_trigger() {
        let host = MockHost::new();
        let s = settings_below(/*trigger*/ 250_000_000_000);
        program_oracle(
            &host,
            s.oracle_address,
            Ok(oracle_response_json(300_000_000_000)),
        );

        on_block(&host, SEPOLIA, &s).unwrap();

        assert_eq!(host.cow_api.call_count(), 0);
        assert_eq!(host.store.len(), 0);
        assert!(host.logging.contains("stop-loss idle"));
    }

    #[test]
    fn triggers_and_submits_once_then_dedups() {
        let host = MockHost::new();
        let s = settings_below(250_000_000_000);
        program_oracle(
            &host,
            s.oracle_address,
            Ok(oracle_response_json(200_000_000_000)),
        );
        let uid = programmed_uid(&s);
        host.cow_api.respond(Ok(uid.clone()));

        // First block: submits.
        on_block(&host, SEPOLIA, &s).unwrap();
        assert_eq!(host.cow_api.call_count(), 1);
        assert!(host.logging.contains("TRIGGERED"));
        assert!(
            host.store
                .snapshot()
                .contains_key(&format!("submitted:{uid}"))
        );

        // Second block at the same price: dedup'd, no new submit.
        on_block(&host, SEPOLIA, &s).unwrap();
        assert_eq!(host.cow_api.call_count(), 1);
        assert!(host.logging.contains("already submitted"));
    }

    #[test]
    fn permanent_submit_error_marks_dropped() {
        let host = MockHost::new();
        let s = settings_below(250_000_000_000);
        program_oracle(
            &host,
            s.oracle_address,
            Ok(oracle_response_json(200_000_000_000)),
        );

        // Orderbook returns InvalidSignature - permanent per the
        // retriable-error classifier.
        let api_body = serde_json::json!({
            "errorType": "InvalidSignature",
            "description": "bad sig",
        })
        .to_string();
        host.cow_api.respond(Err(HostError {
            domain: "cow-api".into(),
            kind: Kind::Denied,
            code: 400,
            message: "InvalidSignature".into(),
            data: Some(api_body),
        }));

        on_block(&host, SEPOLIA, &s).unwrap();
        let uid = programmed_uid(&s);
        assert!(
            host.store
                .snapshot()
                .contains_key(&format!("dropped:{uid}"))
        );
        assert!(
            !host
                .store
                .snapshot()
                .contains_key(&format!("submitted:{uid}"))
        );
        assert!(host.logging.contains("dropped"));

        // Second block: dropped marker idles the loop.
        on_block(&host, SEPOLIA, &s).unwrap();
        assert_eq!(host.cow_api.call_count(), 1); // no resubmit
        assert!(host.logging.contains("previously dropped"));
    }

    #[test]
    fn transient_submit_error_leaves_state_unchanged() {
        let host = MockHost::new();
        let s = settings_below(250_000_000_000);
        program_oracle(
            &host,
            s.oracle_address,
            Ok(oracle_response_json(200_000_000_000)),
        );

        let api_body = serde_json::json!({
            "errorType": "InsufficientFee",
            "description": "fee too low",
        })
        .to_string();
        host.cow_api.respond(Err(HostError {
            domain: "cow-api".into(),
            kind: Kind::Denied,
            code: 400,
            message: "InsufficientFee".into(),
            data: Some(api_body),
        }));

        on_block(&host, SEPOLIA, &s).unwrap();

        // No persistence flag - next block will retry.
        assert_eq!(host.store.len(), 0);
        assert!(host.logging.contains("retry on next block"));
    }

    #[test]
    fn oracle_rpc_error_is_warn_and_continue() {
        let host = MockHost::new();
        let s = settings_below(250_000_000_000);
        program_oracle(
            &host,
            s.oracle_address,
            Err(HostError {
                domain: "chain".into(),
                kind: Kind::Timeout,
                code: 504,
                message: "upstream timed out".into(),
                data: None,
            }),
        );

        on_block(&host, SEPOLIA, &s).unwrap();

        assert_eq!(host.cow_api.call_count(), 0);
        assert_eq!(host.store.len(), 0);
        assert!(host.logging.contains("oracle eth_call failed"));
    }

    #[test]
    fn parse_config_round_trips_settings() {
        let entries = vec![
            (
                "oracle_address".into(),
                "0x694AA1769357215DE4FAC081bf1f309aDC325306".into(),
            ),
            ("decimals".into(), "8".into()),
            ("trigger_price".into(), "2500.00".into()),
            (
                "owner".into(),
                "0x70997970C51812dc3A010C7d01b50e0d17dc79C8".into(),
            ),
            (
                "sell_token".into(),
                "0x6810e776880C02933D47DB1b9fc05908e5386b96".into(),
            ),
            (
                "buy_token".into(),
                "0xfff9976782d46cc05630d1f6ebab18b2324d6b14".into(),
            ),
            ("sell_amount_wei".into(), "1000000000000000000".into()),
            ("buy_amount_wei".into(), "300000000000000000".into()),
            ("valid_to_seconds".into(), "4294967295".into()),
        ];
        let s = parse_config(&entries).unwrap();
        assert_eq!(s.valid_to, u32::MAX);
        assert_eq!(
            s.trigger_price_scaled,
            I256::try_from(250_000_000_000_i64).unwrap()
        );
    }

    #[test]
    fn parse_config_rejects_missing_owner() {
        let entries = vec![
            (
                "oracle_address".into(),
                "0x694AA1769357215DE4FAC081bf1f309aDC325306".into(),
            ),
            ("decimals".into(), "8".into()),
            ("trigger_price".into(), "1.0".into()),
            (
                "sell_token".into(),
                "0x6810e776880C02933D47DB1b9fc05908e5386b96".into(),
            ),
            (
                "buy_token".into(),
                "0xfff9976782d46cc05630d1f6ebab18b2324d6b14".into(),
            ),
            ("sell_amount_wei".into(), "1".into()),
            ("buy_amount_wei".into(), "1".into()),
            ("valid_to_seconds".into(), "1".into()),
        ];
        let err = parse_config(&entries).unwrap_err();
        assert!(matches!(err.kind, Kind::InvalidInput));
        assert!(err.message.contains("owner"));
    }
}
