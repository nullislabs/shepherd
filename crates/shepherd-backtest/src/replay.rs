//! Per-event replay against `ethflow_watcher::strategy::on_chain_logs`.
//!
//! Each [`EthFlowFixture`] is driven through the production strategy
//! exactly the way the live engine does it: a fresh [`MockHost`] is
//! constructed, a catch-all 200 response is programmed for any
//! `cow_api_request` call (the observe+verify strategy GETs
//! `/api/v1/orders/{uid}` to confirm the orderbook has indexed the
//! order), and `strategy::on_chain_logs` is invoked with an alloy
//! `Log` reconstructed from the raw `eth_getLogs` payload.
//!
//! The classification falls into one of the four buckets defined in
//! the issue:
//!
//! - `Observed`: the strategy verified the order with exactly one
//!   `GET /api/v1/orders/{uid}` and wrote `observed:{uid}` to the
//!   local store. This is the success case under the observe+verify
//!   strategy.
//! - `RejectedExpected`: the strategy returned without observing in a
//!   documented case (reserved for fixtures where the mock returns 404
//!   — not applicable when all fixtures program 200).
//! - `RejectedUnexpected`: the strategy returned Ok but the observe
//!   contract was violated (no `observed:{uid}` marker, an unexpected
//!   orderbook call shape, or a `submit_order` attempt); a follow-up
//!   should be filed before the report closes.
//! - `StrategyError`: `on_chain_logs` returned `Err(fault)`. A test
//!   bug or an `unreachable!` we want to investigate.

use ethflow_watcher::strategy;
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
    /// Surfaced in the report for failure triage.
    pub log_lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Classification {
    Observed,
    /// Reserved for any documented skip path (e.g. a fixture where the mock
    /// returns 404 for an un-indexed order). Not emitted in the current batch;
    /// retained so the acceptance-ratio formula is complete.
    #[allow(dead_code)]
    RejectedExpected(String),
    RejectedUnexpected(String),
    StrategyError(String),
}

impl Classification {
    pub fn label(&self) -> &'static str {
        match self {
            Classification::Observed => "Observed",
            Classification::RejectedExpected(_) => "RejectedExpected",
            Classification::RejectedUnexpected(_) => "RejectedUnexpected",
            Classification::StrategyError(_) => "StrategyError",
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            Classification::Observed => "",
            Classification::RejectedExpected(d)
            | Classification::RejectedUnexpected(d)
            | Classification::StrategyError(d) => d,
        }
    }
}

/// Replay one EthFlow fixture through the production strategy.
pub fn replay_ethflow(fx: &EthFlowFixture, chain_id: u64) -> ReplayOutcome {
    let host = MockHost::new();

    // Program a catch-all 200 response for any cow_api_request. In
    // the observe+verify strategy the module GETs
    // `/api/v1/orders/{uid}` to confirm the orderbook has indexed the
    // order. In backtest context all fixtures are confirmed real orders,
    // so the mock orderbook always returns 200 (indexed).
    host.cow_api.respond_to_request(Ok("{}".to_string()));

    // Reconstruct the log fields. Topics + data come straight from the
    // collector's `raw_log`; the contract address is the EthFlow
    // owner the fixture pins.
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
    // Assemble the alloy log the strategy consumes, threading the
    // fixture's block-scoped fields through the same WIT-edge conversion
    // the runtime uses.
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

    // Drive the strategy.
    let result = strategy::on_chain_logs(&host, chain_id, &[log]);
    let log_lines: Vec<String> = host
        .logging
        .lines()
        .into_iter()
        .map(|l| format!("[{:?}] {}", l.level, l.message))
        .collect();

    let class = match result {
        Err(e) => Classification::StrategyError(e.to_string()),
        Ok(()) => classify_ok(&host, &fx.uid, &log_lines),
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

/// Classify an `Ok(())` replay by inspecting the mock's recorded
/// side effects, independent of the strategy's own logging.
///
/// `Observed` demands the full observe contract, not just the marker:
/// exactly one orderbook call, shaped `GET /api/v1/orders/{uid}`,
/// zero `submit_order` attempts, and the `observed:{uid}` store key.
/// The exact-UID match (never an `observed:` prefix scan) means a
/// compute_uid divergence between module and collector cannot produce
/// a false `Observed`.
fn classify_ok(host: &MockHost, uid: &str, log_lines: &[String]) -> Classification {
    if host.cow_api.call_count() > 0 {
        return Classification::RejectedUnexpected(
            "strategy called submit_order; observe+verify must never submit".into(),
        );
    }
    let requests = host.cow_api.request_calls();
    let expected_path = format!("/api/v1/orders/{uid}");
    if requests.len() != 1 || requests[0].method != "GET" || requests[0].path != expected_path {
        let shapes: Vec<String> = requests
            .iter()
            .map(|r| format!("{} {}", r.method, r.path))
            .collect();
        return Classification::RejectedUnexpected(format!(
            "expected exactly one GET {expected_path}; saw [{}]",
            shapes.join(", ")
        ));
    }
    if host
        .store
        .snapshot()
        .contains_key(&format!("observed:{uid}"))
    {
        return Classification::Observed;
    }
    // The strategy returned Ok without writing an observed marker.
    // Surface for triage.
    let last_log = log_lines.last().cloned().unwrap_or_default();
    Classification::RejectedUnexpected(format!("Ok with no observed marker; last log: {last_log}"))
}
