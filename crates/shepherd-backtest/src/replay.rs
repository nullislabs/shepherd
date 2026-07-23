//! Per-event replay against the ethflow-watcher strategy pair.
//!
//! Each [`EthFlowFixture`] is driven the way the live engine does it,
//! minus the wasm boundary: `strategy::on_chain_logs` runs over a
//! recording venue transport (the pool seam), then the status
//! transition the registry would poll for the watched receipt is
//! delivered through `strategy::on_intent_status`. In backtest context
//! all fixtures are confirmed real orders, so the simulated transition
//! is `open`.
//!
//! The classification falls into one of four buckets:
//!
//! - `Observed`: the strategy registered exactly one `cow` watch whose
//!   receipt is the fixture's UID and wrote `observed:{uid}` on the
//!   delivered transition. The success case.
//! - `RejectedExpected`: reserved for a documented skip path; not
//!   emitted in the current batch.
//! - `RejectedUnexpected`: the strategy returned Ok but the observe
//!   contract was violated (a wrong watch shape, a non-observe venue
//!   call, or a missing `observed:{uid}` marker); a follow-up should
//!   be filed before the report closes.
//! - `StrategyError`: a strategy call returned `Err(fault)`. A test
//!   bug or an `unreachable!` we want to investigate.

use std::cell::RefCell;
use std::rc::Rc;

use cow_venue::client::CowClient;
use ethflow_watcher::strategy;
use nexum_sdk_test::{CapturedEvents, MockHost, capture_tracing};
use videre_sdk::client::{VenueId, VenueTransport};
use videre_sdk::rt::complete;
use videre_sdk::status_body::{IntentStatus, StatusBody};
use videre_sdk::{Quotation, SubmitOutcome, VenueFault};

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
    /// Reserved for any documented skip path. Not emitted in the current
    /// batch; retained so the acceptance-ratio formula is complete.
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

/// One recorded transport call: the verb, and for `observe` the routed
/// venue and receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Call {
    Quote,
    Submit,
    Observe(String, Vec<u8>),
    Status,
    Cancel,
}

impl Call {
    fn label(&self) -> &'static str {
        match self {
            Call::Quote => "quote",
            Call::Submit => "submit",
            Call::Observe(..) => "observe",
            Call::Status => "status",
            Call::Cancel => "cancel",
        }
    }
}

/// Records every pool call; `observe` accepts, the other verbs refuse.
/// Cloneable over shared state so the replay keeps a handle after one
/// moves into the client.
#[derive(Clone, Default)]
struct RecordingVenues {
    calls: Rc<RefCell<Vec<Call>>>,
}

impl RecordingVenues {
    fn calls(&self) -> Vec<Call> {
        self.calls.borrow().clone()
    }
}

impl videre_sdk::client::sealed::SealedTransport for RecordingVenues {}

impl VenueTransport for RecordingVenues {
    async fn quote(&self, _venue: &VenueId, _body: Vec<u8>) -> Result<Quotation, VenueFault> {
        self.calls.borrow_mut().push(Call::Quote);
        Err(VenueFault::Unsupported)
    }

    async fn submit(&self, _venue: &VenueId, _body: Vec<u8>) -> Result<SubmitOutcome, VenueFault> {
        self.calls.borrow_mut().push(Call::Submit);
        Err(VenueFault::Unsupported)
    }

    async fn observe(&self, venue: &VenueId, receipt: &[u8]) -> Result<(), VenueFault> {
        self.calls
            .borrow_mut()
            .push(Call::Observe(venue.to_string(), receipt.to_vec()));
        Ok(())
    }

    async fn status(
        &self,
        _venue: &VenueId,
        _receipt: &[u8],
    ) -> Result<videre_sdk::IntentStatus, VenueFault> {
        self.calls.borrow_mut().push(Call::Status);
        Err(VenueFault::Unsupported)
    }

    async fn cancel(&self, _venue: &VenueId, _receipt: &[u8]) -> Result<(), VenueFault> {
        self.calls.borrow_mut().push(Call::Cancel);
        Err(VenueFault::Unsupported)
    }
}

/// The `open` transition the registry would report for an indexed order.
fn open_status() -> Result<Vec<u8>, String> {
    StatusBody {
        status: IntentStatus::Open,
        proof: None,
        reason: None,
    }
    .encode()
    .map_err(|e| format!("status body encode: {e}"))
}

/// Replay one EthFlow fixture through the production strategy pair.
pub fn replay_ethflow(fx: &EthFlowFixture, chain_id: u64) -> ReplayOutcome {
    let host = MockHost::new();
    let venues = RecordingVenues::default();
    let client = CowClient::with_transport(venues.clone());

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

    // Drive the strategy: register the watch, then deliver the status
    // transition the registry would poll for the watched receipt.
    let (result, logs) = capture_tracing(|| {
        complete(strategy::on_chain_logs(&host, &client, chain_id, &[log]))
            .ok_or_else(|| "strategy future suspended".to_owned())
            .and_then(|r| r.map_err(|e| e.to_string()))?;
        let calls = venues.calls();
        let [Call::Observe(venue, receipt)] = calls.as_slice() else {
            let shapes: Vec<&str> = calls.iter().map(Call::label).collect();
            return Err(format!(
                "expected exactly one observe; saw [{}]",
                shapes.join(", ")
            ));
        };
        if venue != "cow" {
            return Err(format!("watch routed to venue `{venue}`, not `cow`"));
        }
        strategy::on_intent_status(&host, venue, receipt, &open_status()?)
            .map_err(|e| e.to_string())?;
        Ok(receipt.clone())
    });
    let log_lines = log_lines(&logs);

    let class = match result {
        Err(detail) => classify_err(&venues, detail),
        Ok(receipt) => classify_ok(&host, &receipt, &fx.uid, &log_lines),
    };

    ReplayOutcome {
        uid: fx.uid.clone(),
        block_number: fx.block_number,
        block_timestamp: fx.block_timestamp,
        class,
        log_lines,
    }
}

fn log_lines(logs: &CapturedEvents) -> Vec<String> {
    logs.events()
        .into_iter()
        .map(|e| format!("[{:?}] {}", e.level, e.message))
        .collect()
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

/// A failed replay is an observe-contract violation when the strategy
/// itself returned Ok but touched the pool wrongly; otherwise a
/// strategy error.
fn classify_err(venues: &RecordingVenues, detail: String) -> Classification {
    if venues
        .calls()
        .iter()
        .any(|c| !matches!(c, Call::Observe(..)))
    {
        return Classification::RejectedUnexpected(format!(
            "strategy called a non-observe venue verb; observe-only must never submit ({detail})"
        ));
    }
    if detail.starts_with("expected exactly one observe")
        || detail.starts_with("watch routed to venue")
    {
        return Classification::RejectedUnexpected(detail);
    }
    Classification::StrategyError(detail)
}

/// Classify an `Ok` replay by inspecting the recorded side effects,
/// independent of the strategy's own logging.
///
/// `Observed` demands the full observe contract, not just the marker:
/// the one watch's receipt renders to exactly the fixture UID (never a
/// prefix scan, so a compute_uid divergence between module and
/// collector cannot produce a false `Observed`), and the delivered
/// transition wrote the `observed:{uid}` store key.
fn classify_ok(host: &MockHost, receipt: &[u8], uid: &str, log_lines: &[String]) -> Classification {
    let receipt_hex = format!("0x{}", hex::encode(receipt));
    if receipt_hex != uid {
        return Classification::RejectedUnexpected(format!(
            "watched receipt {receipt_hex} does not match the collector UID"
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
