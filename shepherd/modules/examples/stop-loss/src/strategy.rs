//! Pure stop-loss strategy logic. Reads an oracle, optionally submits
//! a CoW order intent through the typed venue client, dedups via
//! local-store. Every interaction with the world flows through the
//! `nexum_sdk::host` trait seams and the videre [`VenueTransport`]
//! under the typed [`CowClient`], so tests drive it against
//! `nexum_sdk_test::MockHost` and a scripted transport.

use alloy_primitives::I256;
use cow_venue::{BuyToken, CowClient, CowIntent, CowIntentBody, OrderBody, SellToken, intent_id};
use nexum_sdk::chain::chainlink::read_latest_answer;
use nexum_sdk::config::{self, ConfigError};
use nexum_sdk::host::{ChainHost, Fault, LocalStoreHost, LoggingHost};
use nexum_sdk::keeper::RetryAction;
use nexum_sdk::prelude::{Address, U256, hex};
use videre_sdk::keeper::retry_action;
use videre_sdk::{ClientError, SubmitOutcome, VenueTransport, rt};

/// Resolved configuration parsed from `module.toml::[config]`.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Chainlink AggregatorV3Interface address.
    pub oracle_address: Address,
    /// Trigger price scaled to the oracle's native units.
    pub trigger_price_scaled: I256,
    /// Order owner (= the `setPreSignature` caller and buy-token
    /// receiver).
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
/// (oracle RPC error, decode failure, venue refusal). Only host-store
/// errors bubble up via `?` so the supervisor can surface persistence
/// issues - all other faults log and let the next block re-poll.
pub fn on_block<H, T>(
    host: &H,
    venue: &CowClient<T>,
    chain_id: u64,
    settings: &Settings,
) -> Result<(), Fault>
where
    H: ChainHost + LoggingHost + LocalStoreHost,
    T: VenueTransport,
{
    let price = match read_latest_answer(host, chain_id, settings.oracle_address, "stop-loss") {
        Some(p) => p,
        None => return Ok(()), // logged inside read_latest_answer
    };

    if price > settings.trigger_price_scaled {
        tracing::info!(
            price = %price,
            trigger = %settings.trigger_price_scaled,
            "stop-loss idle",
        );
        return Ok(());
    }

    // Derive the venue-and-body intent-id up-front so the dedup guard
    // runs before any network work.
    let intent = build_intent(settings);
    let id = match intent_id(&intent) {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, "intent body encode failed");
            return Ok(());
        }
    };
    let dedup_key = format!("submitted:{id}");
    if host.get(&dedup_key)?.is_some() {
        tracing::info!(intent = %id, "stop-loss already submitted, idle");
        return Ok(());
    }
    let dropped_key = format!("dropped:{id}");
    if host.get(&dropped_key)?.is_some() {
        tracing::info!(intent = %id, "stop-loss previously dropped, idle");
        return Ok(());
    }

    let Some(outcome) = rt::complete(venue.submit(&intent)) else {
        // Guest transports never suspend; retry on the next block.
        tracing::error!("stop-loss submit future suspended; retrying next block");
        return Ok(());
    };
    match outcome {
        Ok(SubmitOutcome::Accepted(receipt)) => {
            host.set(&dedup_key, b"")?;
            tracing::warn!(
                price = %price,
                trigger = %settings.trigger_price_scaled,
                receipt = %hex::encode_prefixed(&receipt),
                "stop-loss TRIGGERED",
            );
        }
        Ok(SubmitOutcome::RequiresSigning(_)) => {
            // The orderbook holds the order as signature-pending; the
            // owner activates it with the on-chain `setPreSignature`
            // call made ahead of the trigger. Journalled so the next
            // block idles instead of re-posting.
            host.set(&dedup_key, b"")?;
            tracing::warn!(
                price = %price,
                trigger = %settings.trigger_price_scaled,
                "stop-loss TRIGGERED (pre-sign pending on-chain activation)",
            );
        }
        Err(ClientError::Body(e)) => {
            tracing::error!(error = %e, "intent body encode failed");
        }
        Err(ClientError::Venue(fault)) => match retry_action(&fault) {
            RetryAction::TryNextBlock | RetryAction::Backoff { .. } => {
                tracing::warn!(error = %fault, "stop-loss retry on next block");
            }
            RetryAction::Drop => {
                host.set(&dropped_key, b"")?;
                tracing::warn!(intent = %id, error = %fault, "stop-loss dropped");
            }
            // `RetryAction` is `#[non_exhaustive]`; treat unknown
            // future variants like `TryNextBlock` rather than
            // silently dropping the order on an SDK bump.
            _ => {
                tracing::warn!(
                    error = %fault,
                    "stop-loss unknown retry-action - retry on next block",
                );
            }
        },
        // `ClientError` is non-exhaustive; retry on the next block.
        Err(e) => tracing::error!(error = %e, "stop-loss submit failed"),
    }
    Ok(())
}

/// Assemble the order intent from settings: an unsigned order the cow
/// adapter posts pre-sign. The owner receives the buy token and the
/// app-data hash pins the canonical empty document.
fn build_intent(settings: &Settings) -> CowIntentBody {
    let order = OrderBody::sell(
        SellToken(settings.sell_token.into_array()),
        settings.sell_amount.to_be_bytes(),
    )
    .for_at_least(
        BuyToken(settings.buy_token.into_array()),
        settings.buy_amount.to_be_bytes(),
    )
    .valid_to(settings.valid_to)
    .receiver(settings.owner.into_array())
    .app_data(cowprotocol::EMPTY_APP_DATA_HASH.0)
    .build();
    CowIntentBody::V1(CowIntent::Order(order))
}

/// Parse `module.toml::[config]` into a typed [`Settings`].
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

/// Lift a free-text invalid-config detail into a [`Fault::InvalidInput`].
/// Used when the SDK helper does not own the error (e.g. an
/// `Address::from_str` failure or a `U256::from_str` overflow).
fn invalid(message: impl Into<String>) -> Fault {
    Fault::InvalidInput(message.into())
}

/// Project a `nexum_sdk::config::ConfigError` into a
/// [`Fault::InvalidInput`] via `Display`.
fn config_err(e: ConfigError) -> Fault {
    invalid(e.to_string())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use alloy_primitives::hex;
    use alloy_sol_types::SolCall;
    use nexum_sdk::Level;
    use nexum_sdk::chain::chainlink::AggregatorV3;
    use nexum_sdk::chain::eth_call_params;
    use nexum_sdk::host::ChainError;
    use nexum_sdk_test::{MockHost, capture_tracing};
    use videre_sdk::client::sealed::SealedTransport;
    use videre_sdk::{IntentStatus, Quotation, UnsignedTx, VenueFault, VenueId};

    use super::*;

    const SEPOLIA: u64 = 11_155_111;

    /// Scripted venue transport: one submit outcome per queued entry,
    /// every submit recorded.
    #[derive(Default)]
    struct MockVenue {
        outcomes: RefCell<VecDeque<Result<SubmitOutcome, VenueFault>>>,
        submits: RefCell<Vec<(String, Vec<u8>)>>,
    }

    impl MockVenue {
        fn enqueue_submit(&self, outcome: Result<SubmitOutcome, VenueFault>) {
            self.outcomes.borrow_mut().push_back(outcome);
        }

        fn submit_count(&self) -> usize {
            self.submits.borrow().len()
        }
    }

    impl SealedTransport for &MockVenue {}

    impl VenueTransport for &MockVenue {
        async fn quote(&self, _venue: &VenueId, _body: Vec<u8>) -> Result<Quotation, VenueFault> {
            unreachable!("quote not exercised")
        }

        async fn submit(
            &self,
            venue: &VenueId,
            body: Vec<u8>,
        ) -> Result<SubmitOutcome, VenueFault> {
            self.submits.borrow_mut().push((venue.to_string(), body));
            self.outcomes.borrow_mut().pop_front().unwrap_or_else(|| {
                Err(VenueFault::Unavailable(
                    "MockVenue: unscripted submit".into(),
                ))
            })
        }

        async fn status(
            &self,
            _venue: &VenueId,
            _receipt: &[u8],
        ) -> Result<IntentStatus, VenueFault> {
            unreachable!("status not exercised")
        }

        async fn cancel(&self, _venue: &VenueId, _receipt: &[u8]) -> Result<(), VenueFault> {
            unreachable!("cancel not exercised")
        }
    }

    fn client(venue: &MockVenue) -> CowClient<&MockVenue> {
        CowClient::with_transport(venue)
    }

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

    fn program_oracle(host: &MockHost, oracle: Address, response: Result<String, ChainError>) {
        let call_data = AggregatorV3::latestRoundDataCall {}.abi_encode();
        let params = eth_call_params(&oracle, &call_data);
        host.chain.respond_to("eth_call", &params, response);
    }

    fn programmed_id(settings: &Settings) -> String {
        intent_id(&build_intent(settings)).unwrap()
    }

    /// Regression test pinning the orderbook UID derived from the
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
        let CowIntentBody::V1(CowIntent::Order(body)) = build_intent(&settings) else {
            panic!("stop-loss emits an unsigned order intent");
        };
        let order = cow_venue::assembly::body_to_order_data(&body);
        let uid = cow_venue::assembly::order_uid(
            cowprotocol::Chain::try_from(SEPOLIA).unwrap(),
            &order,
            settings.owner,
        );
        assert_eq!(
            format!("{uid}"),
            "0xc2b9cb4ea1ee5a86d8049ac09d8f494bf04cca0a68407285f31e2e6379800be87bf140727d27ea64b607e042f1225680b40eca6affffffff",
        );
    }

    #[test]
    fn idle_when_price_above_trigger() {
        let host = MockHost::new();
        let venue = MockVenue::default();
        let s = settings_below(/*trigger*/ 250_000_000_000);
        program_oracle(
            &host,
            s.oracle_address,
            Ok(oracle_response_json(300_000_000_000)),
        );

        on_block(&host, &client(&venue), SEPOLIA, &s).unwrap();

        assert_eq!(venue.submit_count(), 0);
        assert_eq!(host.store.len(), 0);
        assert_eq!(
            host.chain.call_count(),
            1,
            "oracle consulted: idle because above trigger, not because unread"
        );
    }

    #[test]
    fn triggers_and_submits_once_then_dedups() {
        let host = MockHost::new();
        let venue = MockVenue::default();
        let s = settings_below(250_000_000_000);
        program_oracle(
            &host,
            s.oracle_address,
            Ok(oracle_response_json(200_000_000_000)),
        );
        venue.enqueue_submit(Ok(SubmitOutcome::Accepted(vec![0xAA; 56])));

        // First block: submits.
        on_block(&host, &client(&venue), SEPOLIA, &s).unwrap();
        assert_eq!(venue.submit_count(), 1);
        let id = programmed_id(&s);
        assert!(
            host.store
                .snapshot()
                .contains_key(&format!("submitted:{id}"))
        );

        // Second block at the same price: dedup'd, no new submit.
        on_block(&host, &client(&venue), SEPOLIA, &s).unwrap();
        assert_eq!(venue.submit_count(), 1);
        assert_eq!(
            host.chain.call_count(),
            2,
            "oracle still polled each block; dedup is at the submit stage"
        );
    }

    /// The adapter posts the unsigned order pre-sign and asks for the
    /// on-chain activation: the intent is journalled so the next block
    /// idles instead of re-posting.
    #[test]
    fn requires_signing_outcome_records_the_marker_and_idles() {
        let host = MockHost::new();
        let venue = MockVenue::default();
        let s = settings_below(250_000_000_000);
        program_oracle(
            &host,
            s.oracle_address,
            Ok(oracle_response_json(200_000_000_000)),
        );
        venue.enqueue_submit(Ok(SubmitOutcome::RequiresSigning(UnsignedTx {
            chain: SEPOLIA,
            to: vec![0x11; 20],
            value: Vec::new(),
            data: vec![0x22],
        })));

        on_block(&host, &client(&venue), SEPOLIA, &s).unwrap();

        let id = programmed_id(&s);
        assert!(
            host.store
                .snapshot()
                .contains_key(&format!("submitted:{id}"))
        );

        on_block(&host, &client(&venue), SEPOLIA, &s).unwrap();
        assert_eq!(venue.submit_count(), 1);
    }

    #[test]
    fn permanent_submit_error_marks_dropped() {
        let host = MockHost::new();
        let venue = MockVenue::default();
        let s = settings_below(250_000_000_000);
        program_oracle(
            &host,
            s.oracle_address,
            Ok(oracle_response_json(200_000_000_000)),
        );

        // A structured permanent refusal - `Denied` classifies as
        // `Drop` in the videre retry table.
        venue.enqueue_submit(Err(VenueFault::Denied("InvalidSignature: bad sig".into())));

        on_block(&host, &client(&venue), SEPOLIA, &s).unwrap();
        let id = programmed_id(&s);
        assert!(host.store.snapshot().contains_key(&format!("dropped:{id}")));
        assert!(
            !host
                .store
                .snapshot()
                .contains_key(&format!("submitted:{id}"))
        );

        // Second block: dropped marker idles the loop.
        on_block(&host, &client(&venue), SEPOLIA, &s).unwrap();
        assert_eq!(venue.submit_count(), 1); // no resubmit
    }

    #[test]
    fn transient_submit_error_leaves_state_unchanged() {
        let host = MockHost::new();
        let venue = MockVenue::default();
        let s = settings_below(250_000_000_000);
        program_oracle(
            &host,
            s.oracle_address,
            Ok(oracle_response_json(200_000_000_000)),
        );

        venue.enqueue_submit(Err(VenueFault::Unavailable("orderbook http 502".into())));

        let (result, logs) = capture_tracing(|| on_block(&host, &client(&venue), SEPOLIA, &s));
        result.unwrap();

        // No persistence flag - next block will retry.
        assert_eq!(host.store.len(), 0);
        assert_eq!(venue.submit_count(), 1, "the submit was attempted");
        logs.expect_one(|e| e.level == Level::WARN && e.message.contains("retry on next block"));
    }

    #[test]
    fn oracle_rpc_error_is_warn_and_continue() {
        let host = MockHost::new();
        let venue = MockVenue::default();
        let s = settings_below(250_000_000_000);
        program_oracle(
            &host,
            s.oracle_address,
            Err(ChainError::Fault(Fault::Timeout)),
        );

        on_block(&host, &client(&venue), SEPOLIA, &s).unwrap();

        assert_eq!(venue.submit_count(), 0);
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
        let Fault::InvalidInput(message) = err else {
            panic!("expected invalid-input fault, got {err:?}");
        };
        assert!(message.contains("owner"));
    }
}
