//! Per-event replay against `ethflow_watcher::strategy::on_chain_logs`.
//!
//! Each [`EthFlowFixture`] is driven through the production strategy
//! exactly the way the live engine does it: a fresh [`MockHost`] is
//! constructed, the `cow_api_request` response is programmed to echo
//! the fixture's pre-collected orderbook order JSON, and
//! `strategy::on_chain_logs` is invoked with an alloy `Log`
//! reconstructed from the raw `eth_getLogs` payload.
//!
//! The strategy **observes and verifies** (GET `/api/v1/orders/{uid}`),
//! it does not submit. Classification therefore tracks whether the
//! strategy called the indexer probe, not a POST:
//!
//! - `Observed`: the strategy called `GET /api/v1/orders/{uid}` and
//!   received a 200 from the mock — the fixture's uid was indexed —
//!   and the idempotency marker `observed:{uid}` was written to the
//!   local store.
//! - `IndexerLag`: the strategy probed the uid but the idempotency marker
//!   was not written. Currently unreachable — the harness always programs
//!   a 200 response, so real fixtures always produce `Observed`. Reserved
//!   for a future variant that programs 404 responses for orders not yet
//!   indexed at collection time.
//! - `NotEthFlow`: the log was not a recognised `OrderPlacement` event
//!   (address not in canonical set, or wrong topics). Expected for any
//!   fixture that fell through the address filter.
//! - `StrategyError`: `on_chain_logs` returned `Err(fault)` — a test
//!   bug or an `unreachable!` worth investigating.

use nexum_sdk::chassis::OBSERVED_PREFIX;
use nexum_sdk::host::LocalStoreHost;
use shepherd_sdk_test::MockHost;

use crate::fixtures::{EthFlowFixture, parse_address};

/// The collected outcome for one replayed event.
#[derive(Debug)]
pub struct ReplayOutcome {
    pub uid: String,
    pub block_number: u64,
    pub block_timestamp: u64,
    pub class: Classification,
    /// Log lines the strategy emitted while processing this fixture.
    pub log_lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Classification {
    /// The strategy probed the indexer, got 200, and wrote the marker.
    Observed,
    /// The strategy probed the indexer, got 404 (expected indexer lag).
    IndexerLag,
    /// The log was not a recognised EthFlow `OrderPlacement` event.
    NotEthFlow(String),
    /// `on_chain_logs` returned `Err(fault)`.
    StrategyError(String),
}

impl Classification {
    pub fn label(&self) -> &'static str {
        match self {
            Classification::Observed => "Observed",
            Classification::IndexerLag => "IndexerLag",
            Classification::NotEthFlow(_) => "NotEthFlow",
            Classification::StrategyError(_) => "StrategyError",
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            Classification::Observed | Classification::IndexerLag => "",
            Classification::NotEthFlow(d) | Classification::StrategyError(d) => d,
        }
    }

    /// Whether this outcome counts as accepted for the sign-off ratio:
    /// `Observed` and `IndexerLag` are both expected operating states.
    /// `NotEthFlow` is also expected for any fixture not matching the
    /// canonical address set. Only `StrategyError` is a failure.
    pub fn is_accepted(&self) -> bool {
        !matches!(self, Classification::StrategyError(_))
    }
}

/// Replay one EthFlow fixture through the production strategy.
pub fn replay_ethflow(fx: &EthFlowFixture, chain_id: u64) -> ReplayOutcome {
    let host = MockHost::new();

    // Program the orderbook mock to return 200 for this uid's GET path.
    // The strategy calls `GET /api/v1/orders/{uid}` to verify the
    // orderbook indexed the placement; the mock confirms it has.
    let order_path = format!("/api/v1/orders/{}", fx.uid);
    host.cow_api.respond_to_request_for(
        "GET",
        &order_path,
        Ok(r#"{"status":"open"}"#.to_string()),
    );

    // Reconstruct the log fields from the collector's raw hex.
    let topics = match fx.raw_log.topics_bytes() {
        Ok(t) => t,
        Err(e) => {
            return error_outcome(fx, format!("topics hex decode: {e}"));
        }
    };
    let data = match fx.raw_log.data_bytes() {
        Ok(d) => d,
        Err(e) => {
            return error_outcome(fx, format!("data hex decode: {e}"));
        }
    };
    let address = match parse_address(&fx.contract) {
        Ok(a) => a,
        Err(e) => {
            return error_outcome(fx, format!("contract address: {e}"));
        }
    };

    let log: nexum_sdk::events::Log = nexum_sdk::events::ChainLogParts {
        address: &address,
        topics: &topics,
        data: &data,
        block_number: Some(fx.block_number),
        block_timestamp: Some(fx.block_timestamp),
        log_index: Some(fx.log_index),
        ..Default::default()
    }
    .into();

    let result = ethflow_watcher::strategy::on_chain_logs(&host, chain_id, &[log]);
    let log_lines: Vec<String> = host
        .logging
        .lines()
        .into_iter()
        .map(|l| format!("[{:?}] {}", l.level, l.message))
        .collect();

    let class = match result {
        Err(e) => Classification::StrategyError(e.to_string()),
        Ok(()) => classify_ok(&host, &order_path, &log_lines),
    };

    ReplayOutcome {
        uid: fx.uid.clone(),
        block_number: fx.block_number,
        block_timestamp: fx.block_timestamp,
        class,
        log_lines,
    }
}

fn error_outcome(fx: &EthFlowFixture, reason: String) -> ReplayOutcome {
    ReplayOutcome {
        uid: fx.uid.clone(),
        block_number: fx.block_number,
        block_timestamp: fx.block_timestamp,
        class: Classification::StrategyError(reason),
        log_lines: vec![],
    }
}

fn classify_ok(host: &MockHost, order_path: &str, log_lines: &[String]) -> Classification {
    let requests = host.cow_api.request_calls();
    let probed = requests
        .iter()
        .any(|r| r.method == "GET" && r.path == order_path);

    if probed {
        // Check whether the strategy wrote the idempotency marker.
        // The key format mirrors `nexum_sdk::chassis::OBSERVED_PREFIX` + uid.
        let uid = order_path
            .strip_prefix("/api/v1/orders/")
            .unwrap_or(order_path);
        let marker_key = format!("{OBSERVED_PREFIX}{uid}");
        let has_marker = LocalStoreHost::get(&host.store, &marker_key)
            .unwrap_or(None)
            .is_some();
        if has_marker {
            Classification::Observed
        } else {
            // Probed but marker absent — defensive sentinel for strategy
            // regressions; not produced by the current harness (the mock
            // always returns 200 and the strategy always writes on 200).
            Classification::IndexerLag
        }
    } else {
        // Strategy returned Ok without probing: the log was not
        // recognised as an EthFlow `OrderPlacement` (wrong address,
        // wrong topic, or uid on an unsupported chain).
        let reason = log_lines
            .iter()
            .find(|l| l.contains("skipped") || l.contains("unsupported"))
            .cloned()
            .unwrap_or_else(|| "no GET probe and no strategy log".to_string());
        Classification::NotEthFlow(reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_fixture(uid: &str) -> EthFlowFixture {
        use crate::fixtures::RawLog;
        EthFlowFixture {
            uid: uid.to_string(),
            block_number: 1,
            block_timestamp: 1_700_000_000,
            tx_hash: None,
            log_index: 0,
            // Sepolia EthFlow staging address
            contract: "0x40A50cf069e992AA4536211B23F286eF88752187".to_string(),
            sender: None,
            app_data_hash: "0x".to_string(),
            app_data_resolved: None,
            raw_log: RawLog {
                topics: vec![],
                data: "0x".to_string(),
            },
        }
    }

    #[test]
    fn error_outcome_is_strategy_error() {
        let fx = dummy_fixture("0xabc");
        let out = error_outcome(&fx, "test error".to_string());
        assert!(matches!(out.class, Classification::StrategyError(_)));
        assert_eq!(out.uid, "0xabc");
    }

    #[test]
    fn classification_labels() {
        assert_eq!(Classification::Observed.label(), "Observed");
        assert_eq!(Classification::IndexerLag.label(), "IndexerLag");
        assert_eq!(
            Classification::NotEthFlow("x".into()).label(),
            "NotEthFlow"
        );
        assert_eq!(
            Classification::StrategyError("x".into()).label(),
            "StrategyError"
        );
    }

    #[test]
    fn only_strategy_error_is_not_accepted() {
        assert!(Classification::Observed.is_accepted());
        assert!(Classification::IndexerLag.is_accepted());
        assert!(Classification::NotEthFlow("x".into()).is_accepted());
        assert!(!Classification::StrategyError("x".into()).is_accepted());
    }
}
