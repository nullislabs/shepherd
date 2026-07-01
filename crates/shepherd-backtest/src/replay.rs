//! Per-event replay against `ethflow_watcher::strategy::on_logs`.
//!
//! Each [`EthFlowFixture`] is driven through the production strategy
//! exactly the way the live engine does it: a fresh [`MockHost`] is
//! constructed, the resolved `app_data` JSON is programmed as the
//! `GET /api/v1/app_data/{hash}` response, the
//! `cow_api.submit_order` response is programmed to echo the
//! fixture's pre-derived UID, and `strategy::on_logs(&host, &[view])`
//! is invoked with a [`LogView`] reconstructed from the raw
//! `eth_getLogs` payload.
//!
//! The classification falls into one of the four buckets defined in
//! the issue:
//!
//! - `Submitted`: the strategy called `cow_api.submit_order` with an
//!   `OrderCreation` body. The body is captured for downstream
//!   validation (Phase 2B / orderbook quote round-trip).
//! - `RejectedExpected`: the strategy returned without submitting in
//!   a documented case - e.g. the app_data hash didn't resolve
//!   (documented skip path), or dedup already saw the UID.
//! - `RejectedUnexpected`: the strategy returned without submitting
//!   in a path we don't recognise; a follow-up should be
//!   filed before the report closes.
//! - `StrategyError`: `on_logs` returned `Err(HostError)`. A test
//!   bug or an `unreachable!` we want to investigate.

use ethflow_watcher::strategy::{self, LogView};
use shepherd_sdk::host::{HostError, HostErrorKind};
use shepherd_sdk_test::MockHost;

use crate::fixtures::{EthFlowFixture, parse_address};

/// The collected outcome for one replayed event.
#[derive(Debug)]
pub struct ReplayOutcome {
    pub uid: String,
    pub block_number: u64,
    pub block_timestamp: u64,
    pub class: Classification,
    /// `cow_api.submit_order` body the strategy would have POST'd
    /// to the orderbook, if any. Captured as JSON so a Phase 2B
    /// follow-up can round-trip it against `POST /api/v1/quote`
    /// without re-replaying. Read by the report renderer when
    /// dumping anomalies; otherwise informational.
    #[allow(dead_code)]
    pub submitted_body: Option<serde_json::Value>,
    /// Log lines the strategy emitted while processing this fixture.
    /// Surfaced in the report for failure triage.
    pub log_lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Classification {
    Submitted,
    RejectedExpected(String),
    RejectedUnexpected(String),
    StrategyError(String),
}

impl Classification {
    pub fn label(&self) -> &'static str {
        match self {
            Classification::Submitted => "Submitted",
            Classification::RejectedExpected(_) => "RejectedExpected",
            Classification::RejectedUnexpected(_) => "RejectedUnexpected",
            Classification::StrategyError(_) => "StrategyError",
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            Classification::Submitted => "",
            Classification::RejectedExpected(d)
            | Classification::RejectedUnexpected(d)
            | Classification::StrategyError(d) => d,
        }
    }
}

/// Replay one EthFlow fixture through the production strategy.
pub fn replay_ethflow(fx: &EthFlowFixture, chain_id: u64) -> ReplayOutcome {
    let host = MockHost::new();

    // Program the orderbook to echo the fixture's pre-derived UID
    // on submission. This is what the live orderbook does for a
    // valid placement; the replay's job is to verify the strategy
    // assembles a body the orderbook would have accepted, not to
    // re-run the orderbook itself.
    host.cow_api.respond(Ok(fx.uid.clone()));

    // Program the `app_data` resolution path. If the
    // collector captured a resolved document, hand it back verbatim;
    // if the hash 404'd at collection time, return a host-side
    // `Unavailable` so the strategy hits its documented "appData
    // hash not mirrored" branch.
    let app_data_path = format!("/api/v1/app_data/{}", fx.app_data_hash);
    let app_data_response = match &fx.app_data_resolved {
        Some(doc) => Ok(serde_json::to_string(doc).expect("re-serialise app_data")),
        None => Err(HostError {
            domain: "cow-api".into(),
            kind: HostErrorKind::Unavailable,
            code: 404,
            message: "app_data hash not mirrored".into(),
            data: None,
        }),
    };
    host.cow_api
        .respond_to_request_for("GET", app_data_path, app_data_response);

    // Reconstruct the LogView. Topics + data come straight from the
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
    let view = LogView {
        chain_id,
        address: &address,
        topics: &topics,
        data: &data,
    };

    // Drive the strategy.
    let result = strategy::on_logs(&host, &[view]);
    let log_lines: Vec<String> = host
        .logging
        .lines()
        .into_iter()
        .map(|l| format!("[{:?}] {}", l.level, l.message))
        .collect();

    let class = match result {
        Err(e) => Classification::StrategyError(format!("{:?}: {}", e.kind, e.message)),
        Ok(()) => classify_ok(&host, fx, &log_lines),
    };
    let submitted_body = host.cow_api.last_body_as_json();

    ReplayOutcome {
        uid: fx.uid.clone(),
        block_number: fx.block_number,
        block_timestamp: fx.block_timestamp,
        class,
        submitted_body,
        log_lines,
    }
}

fn error_outcome(fx: &EthFlowFixture, reason: String) -> ReplayOutcome {
    ReplayOutcome {
        uid: fx.uid.clone(),
        block_number: fx.block_number,
        block_timestamp: fx.block_timestamp,
        class: Classification::StrategyError(reason),
        submitted_body: None,
        log_lines: vec![],
    }
}

fn classify_ok(host: &MockHost, fx: &EthFlowFixture, log_lines: &[String]) -> Classification {
    if host.cow_api.call_count() > 0 {
        return Classification::Submitted;
    }
    // The strategy returned Ok without submitting. Distinguish the
    // documented branches from anomalies.
    if fx.app_data_resolved.is_none() {
        return Classification::RejectedExpected(
            "app_data hash not mirrored (documented skip path)".into(),
        );
    }
    // `prior_outcome` short-circuits on Submitted/Dropped - but the
    // MockHost store starts empty per replay so that shouldn't fire.
    // Surface anything else for triage.
    let last_log = log_lines.last().cloned().unwrap_or_default();
    Classification::RejectedUnexpected(format!(
        "Ok with zero submits and resolved app_data; last log: {last_log}"
    ))
}
