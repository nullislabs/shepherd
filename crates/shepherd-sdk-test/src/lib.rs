//! # shepherd-sdk-test
//!
//! In-memory implementation of the CoW-domain
//! [`shepherd_sdk::cow::CowApiHost`] trait, plus a [`MockHost`] that
//! composes it with the generic `nexum-sdk-test` mocks so a CoW module
//! can write integration tests for its strategy logic without
//! `wit-bindgen`, `wasmtime`, or a network round-trip.
//!
//! ## Usage
//!
//! Add as a dev-dep on the module crate and test against [`MockHost`]:
//!
//! ```rust
//! // Glob-import the host traits so the method shortcuts resolve.
//! use nexum_sdk::host::*;
//! use shepherd_sdk::cow::CowApiHost as _;
//! use shepherd_sdk_test::MockHost;
//!
//! let host = MockHost::new();
//! host.cow_api.respond(Ok("0xuid".into()));
//!
//! assert_eq!(host.submit_order(1, b"{}").unwrap(), "0xuid");
//! assert_eq!(host.cow_api.call_count(), 1);
//! ```
//!
//! Per-call venue scripting - outcome queues, status sequences, fault
//! injection - goes through [`MockVenue`] on the same seam:
//!
//! ```rust
//! use nexum_sdk::host::Fault;
//! use shepherd_sdk::cow::{CowApiError, CowApiHost as _};
//! use shepherd_sdk_test::MockHost;
//!
//! let host = MockHost::with_venue();
//! host.cow_api
//!     .enqueue_submit(Err(CowApiError::Fault(Fault::Timeout)));
//! host.cow_api.enqueue_submit(Ok("0xuid".into()));
//!
//! assert!(host.submit_order(1, b"{}").is_err());
//! assert_eq!(host.submit_order(1, b"{}").unwrap(), "0xuid");
//! ```
//!
//! Modules that never touch the orderbook use `nexum-sdk-test`'s
//! `MockHost` directly instead.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![warn(missing_docs)]

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

use nexum_sdk::Level;
use nexum_sdk::host::{ChainError, ChainHost, Fault, LocalStoreHost, LoggingHost};
use nexum_sdk_test::{MockChain, MockLocalStore, MockLogging};
use shepherd_sdk::cow::{CowApiError, CowApiHost};

/// Composed in-memory host for CoW modules: the generic per-trait
/// mocks plus a venue mock on the `shepherd:cow/cow-api` seam -
/// [`MockCowApi`] by default, [`MockVenue`] via
/// [`with_venue`](MockHost::with_venue). Each field exposes the
/// per-trait mock so tests can program responses and assert on calls.
#[derive(Default)]
pub struct MockHost<V = MockCowApi> {
    /// `nexum:host/chain` mock.
    pub chain: MockChain,
    /// `nexum:host/local-store` mock.
    pub store: MockLocalStore,
    /// `shepherd:cow/cow-api` mock.
    pub cow_api: V,
    /// `nexum:host/logging` mock.
    pub logging: MockLogging,
}

impl MockHost {
    /// Fresh empty host. Equivalent to `Default::default`.
    pub fn new() -> Self {
        Self::default()
    }
}

impl MockHost<MockVenue> {
    /// Fresh empty host with [`MockVenue`] on the cow-api seam.
    pub fn with_venue() -> Self {
        Self::default()
    }
}

impl<V> ChainHost for MockHost<V> {
    fn request(&self, chain_id: u64, method: &str, params: &str) -> Result<String, ChainError> {
        self.chain.request(chain_id, method, params)
    }
}

impl<V> LocalStoreHost for MockHost<V> {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Fault> {
        self.store.get(key)
    }
    fn set(&self, key: &str, value: &[u8]) -> Result<(), Fault> {
        self.store.set(key, value)
    }
    fn delete(&self, key: &str) -> Result<(), Fault> {
        self.store.delete(key)
    }
    fn list_keys(&self, prefix: &str) -> Result<Vec<String>, Fault> {
        self.store.list_keys(prefix)
    }
}

impl<V: CowApiHost> CowApiHost for MockHost<V> {
    fn submit_order(&self, chain_id: u64, body: &[u8]) -> Result<String, CowApiError> {
        self.cow_api.submit_order(chain_id, body)
    }
    fn cow_api_request(
        &self,
        chain_id: u64,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<String, CowApiError> {
        self.cow_api.cow_api_request(chain_id, method, path, body)
    }
}

impl<V> LoggingHost for MockHost<V> {
    fn log(&self, level: Level, message: &str) {
        self.logging.log(level, message);
    }
}

// ---------------------------------------------------------------- cow-api

/// In-memory [`CowApiHost`] that captures every submission and returns
/// a programmable response.
#[derive(Default)]
pub struct MockCowApi {
    response: RefCell<Option<Result<String, CowApiError>>>,
    calls: RefCell<Vec<SubmitCall>>,
    /// `cow_api_request` mock state. Keyed by `(method, path)` so
    /// tests can program different responses for `GET
    /// /api/v1/app_data/0x...` vs other endpoints. Falls back to the
    /// unkeyed `request_response` if no key matches.
    request_responses:
        RefCell<std::collections::HashMap<(String, String), Result<String, CowApiError>>>,
    request_response: RefCell<Option<Result<String, CowApiError>>>,
    request_calls: RefCell<Vec<RequestCall>>,
}

/// One recorded [`MockCowApi::submit_order`] invocation.
#[derive(Clone, Debug)]
pub struct SubmitCall {
    /// Chain the guest targeted.
    pub chain_id: u64,
    /// Raw `OrderCreation` JSON body.
    pub body: Vec<u8>,
}

/// One recorded [`MockCowApi::cow_api_request`] invocation.
#[derive(Clone, Debug)]
pub struct RequestCall {
    /// Chain the guest targeted.
    pub chain_id: u64,
    /// HTTP-style verb.
    pub method: String,
    /// Absolute orderbook path, e.g. `/api/v1/app_data/0xabcd...`.
    pub path: String,
    /// Optional JSON body (for POST/PUT).
    pub body: Option<String>,
}

impl MockCowApi {
    /// Program the response the mock returns on every subsequent
    /// `submit_order` call. Defaults to an `Unsupported` fault if
    /// unset.
    pub fn respond(&self, result: Result<String, CowApiError>) {
        *self.response.borrow_mut() = Some(result);
    }

    /// All submissions, in arrival order.
    pub fn calls(&self) -> Vec<SubmitCall> {
        self.calls.borrow().clone()
    }

    /// Last submission, if any.
    pub fn last_call(&self) -> Option<SubmitCall> {
        self.calls.borrow().last().cloned()
    }

    /// Convenience: parse the most recent body as JSON.
    pub fn last_body_as_json(&self) -> Option<serde_json::Value> {
        self.last_call()
            .and_then(|c| serde_json::from_slice(&c.body).ok())
    }

    /// Count of submissions.
    pub fn call_count(&self) -> usize {
        self.calls.borrow().len()
    }
}

impl MockCowApi {
    /// Program a response for a specific `(method, path)` pair.
    /// Highest priority - used when both this and `respond_to_request`
    /// are set.
    pub fn respond_to_request_for(
        &self,
        method: impl Into<String>,
        path: impl Into<String>,
        result: Result<String, CowApiError>,
    ) {
        self.request_responses
            .borrow_mut()
            .insert((method.into(), path.into()), result);
    }

    /// Program the catch-all response for `cow_api_request` calls
    /// that don't match a specific `(method, path)` key. Defaults
    /// to an `Unsupported` fault.
    pub fn respond_to_request(&self, result: Result<String, CowApiError>) {
        *self.request_response.borrow_mut() = Some(result);
    }

    /// All `cow_api_request` invocations, in arrival order.
    pub fn request_calls(&self) -> Vec<RequestCall> {
        self.request_calls.borrow().clone()
    }
}

impl CowApiHost for MockCowApi {
    fn submit_order(&self, chain_id: u64, body: &[u8]) -> Result<String, CowApiError> {
        self.calls.borrow_mut().push(SubmitCall {
            chain_id,
            body: body.to_vec(),
        });
        self.response.borrow().clone().unwrap_or_else(|| {
            Err(CowApiError::Fault(Fault::Unsupported(
                "MockCowApi: no response configured".to_string(),
            )))
        })
    }

    fn cow_api_request(
        &self,
        chain_id: u64,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<String, CowApiError> {
        self.request_calls.borrow_mut().push(RequestCall {
            chain_id,
            method: method.to_string(),
            path: path.to_string(),
            body: body.map(str::to_string),
        });
        if let Some(r) = self
            .request_responses
            .borrow()
            .get(&(method.to_string(), path.to_string()))
            .cloned()
        {
            return r;
        }
        self.request_response.borrow().clone().unwrap_or_else(|| {
            Err(CowApiError::Fault(Fault::Unsupported(
                "MockCowApi: no cow_api_request response configured".to_string(),
            )))
        })
    }
}

// ---------------------------------------------------------------- venue

/// Scripted in-memory venue on the [`CowApiHost`] seam: programmable
/// per-call behaviour, unlike [`MockCowApi`]'s single replayed
/// response. Compose it with the generic mocks via
/// [`MockHost::with_venue`].
///
/// The two queue disciplines differ deliberately. Submissions are
/// discrete effects, so the submit queue strictly drains - one outcome
/// per call, then the configured fallback (default: an `Unsupported`
/// fault), so a test that scripts N outcomes catches an unexpected
/// N+1th submit. Responses are observations, so a `(method, path)`
/// sequence advances per call and its final entry replays forever - a
/// terminal order status persists no matter how often it is re-polled.
/// An injected fault overrides both (without consuming the queues)
/// until cleared, modelling a venue outage.
#[derive(Default)]
pub struct MockVenue {
    submit_queue: RefCell<VecDeque<VenueOutcome>>,
    submit_fallback: RefCell<Option<VenueOutcome>>,
    response_sequences: RefCell<HashMap<(String, String), VecDeque<VenueOutcome>>>,
    response_fallback: RefCell<Option<VenueOutcome>>,
    fault: RefCell<Option<CowApiError>>,
    calls: RefCell<Vec<SubmitCall>>,
    request_calls: RefCell<Vec<RequestCall>>,
}

/// One scripted venue reply: the body / UID on success, a typed
/// [`CowApiError`] otherwise.
type VenueOutcome = Result<String, CowApiError>;

impl MockVenue {
    /// Append one `submit_order` outcome to the queue; each call
    /// consumes one, in order.
    pub fn enqueue_submit(&self, outcome: Result<String, CowApiError>) {
        self.submit_queue.borrow_mut().push_back(outcome);
    }

    /// Steady-state `submit_order` response once the queue is drained.
    /// Unset, a drained queue yields an `Unsupported` fault.
    pub fn set_submit_fallback(&self, outcome: Result<String, CowApiError>) {
        *self.submit_fallback.borrow_mut() = Some(outcome);
    }

    /// Append one outcome to the `(method, path)` response sequence.
    /// Each matching `cow_api_request` call advances the sequence; the
    /// final entry sticks.
    pub fn enqueue_response(
        &self,
        method: impl Into<String>,
        path: impl Into<String>,
        outcome: Result<String, CowApiError>,
    ) {
        self.response_sequences
            .borrow_mut()
            .entry((method.into(), path.into()))
            .or_default()
            .push_back(outcome);
    }

    /// Append one status-probe outcome for the order, keyed on the
    /// orderbook's `GET /api/v1/orders/{uid}` route.
    pub fn enqueue_order_status(&self, uid: &str, outcome: Result<String, CowApiError>) {
        self.enqueue_response("GET", format!("/api/v1/orders/{uid}"), outcome);
    }

    /// Catch-all `cow_api_request` response for calls with no
    /// programmed sequence. Unset, those yield an `Unsupported` fault.
    pub fn set_response_fallback(&self, outcome: Result<String, CowApiError>) {
        *self.response_fallback.borrow_mut() = Some(outcome);
    }

    /// Fail every venue call with `err` until
    /// [`clear_fault`](Self::clear_fault) - a scripted outage. Queued
    /// outcomes are not consumed while the fault is active.
    pub fn inject_fault(&self, err: CowApiError) {
        *self.fault.borrow_mut() = Some(err);
    }

    /// Lift an injected fault; queued outcomes resume where they left
    /// off.
    pub fn clear_fault(&self) {
        *self.fault.borrow_mut() = None;
    }

    /// All submissions, in arrival order.
    pub fn calls(&self) -> Vec<SubmitCall> {
        self.calls.borrow().clone()
    }

    /// Last submission, if any.
    pub fn last_call(&self) -> Option<SubmitCall> {
        self.calls.borrow().last().cloned()
    }

    /// Convenience: parse the most recent submission body as JSON.
    pub fn last_body_as_json(&self) -> Option<serde_json::Value> {
        self.last_call()
            .and_then(|c| serde_json::from_slice(&c.body).ok())
    }

    /// Count of submissions (failed and injected-fault calls included).
    pub fn call_count(&self) -> usize {
        self.calls.borrow().len()
    }

    /// All `cow_api_request` invocations, in arrival order.
    pub fn request_calls(&self) -> Vec<RequestCall> {
        self.request_calls.borrow().clone()
    }

    /// Scripted submit outcomes not yet consumed - assert `0` to prove
    /// a scenario played out in full.
    pub fn pending_submits(&self) -> usize {
        self.submit_queue.borrow().len()
    }
}

impl CowApiHost for MockVenue {
    fn submit_order(&self, chain_id: u64, body: &[u8]) -> Result<String, CowApiError> {
        self.calls.borrow_mut().push(SubmitCall {
            chain_id,
            body: body.to_vec(),
        });
        if let Some(err) = self.fault.borrow().as_ref() {
            return Err(err.clone());
        }
        if let Some(outcome) = self.submit_queue.borrow_mut().pop_front() {
            return outcome;
        }
        self.submit_fallback.borrow().clone().unwrap_or_else(|| {
            Err(CowApiError::Fault(Fault::Unsupported(
                "MockVenue: submit queue exhausted and no fallback configured".to_string(),
            )))
        })
    }

    fn cow_api_request(
        &self,
        chain_id: u64,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<String, CowApiError> {
        self.request_calls.borrow_mut().push(RequestCall {
            chain_id,
            method: method.to_string(),
            path: path.to_string(),
            body: body.map(str::to_string),
        });
        if let Some(err) = self.fault.borrow().as_ref() {
            return Err(err.clone());
        }
        if let Some(sequence) = self
            .response_sequences
            .borrow_mut()
            .get_mut(&(method.to_string(), path.to_string()))
        {
            // Advance until one entry remains, then replay it: the
            // sequence's final state persists.
            if sequence.len() > 1 {
                return sequence.pop_front().expect("length checked above");
            }
            if let Some(last) = sequence.front() {
                return last.clone();
            }
        }
        self.response_fallback.borrow().clone().unwrap_or_else(|| {
            Err(CowApiError::Fault(Fault::Unsupported(
                "MockVenue: no response programmed for this request".to_string(),
            )))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cow_api_captures_body_and_returns_uid() {
        let api = MockCowApi::default();
        api.respond(Ok("0xdeadbeef".into()));
        let uid = api.submit_order(1, b"{\"x\":1}").unwrap();
        assert_eq!(uid, "0xdeadbeef");
        let last = api.last_call().unwrap();
        assert_eq!(last.chain_id, 1);
        assert_eq!(last.body, b"{\"x\":1}");
        assert_eq!(api.last_body_as_json().unwrap()["x"], 1);
    }

    #[test]
    fn cow_api_default_response_is_unsupported() {
        let api = MockCowApi::default();
        let err = api.submit_order(1, b"{}").unwrap_err();
        assert!(
            matches!(err, CowApiError::Fault(Fault::Unsupported(_))),
            "got {err:?}",
        );
    }

    // ---- MockVenue ----

    #[test]
    fn venue_submit_queue_drains_in_order_then_falls_back() {
        let venue = MockVenue::default();
        venue.enqueue_submit(Err(CowApiError::Fault(Fault::Timeout)));
        venue.enqueue_submit(Ok("0xuid".into()));

        assert!(matches!(
            venue.submit_order(1, b"{}"),
            Err(CowApiError::Fault(Fault::Timeout)),
        ));
        assert_eq!(venue.submit_order(1, b"{}").unwrap(), "0xuid");
        assert_eq!(venue.pending_submits(), 0);

        // A drained queue is unsupported by default: an unscripted
        // extra submit fails loudly.
        assert!(matches!(
            venue.submit_order(1, b"{}"),
            Err(CowApiError::Fault(Fault::Unsupported(_))),
        ));

        venue.set_submit_fallback(Ok("0xsteady".into()));
        assert_eq!(venue.submit_order(1, b"{}").unwrap(), "0xsteady");
        assert_eq!(venue.call_count(), 4, "every call is recorded");
    }

    #[test]
    fn venue_records_submissions_like_the_single_shot_mock() {
        let venue = MockVenue::default();
        venue.enqueue_submit(Ok("0xuid".into()));
        venue.submit_order(7, b"{\"x\":1}").unwrap();

        let last = venue.last_call().unwrap();
        assert_eq!(last.chain_id, 7);
        assert_eq!(last.body, b"{\"x\":1}");
        assert_eq!(venue.last_body_as_json().unwrap()["x"], 1);
    }

    #[test]
    fn venue_fault_injection_overrides_queues_until_cleared() {
        let venue = MockVenue::default();
        venue.enqueue_submit(Ok("0xuid".into()));
        venue.enqueue_response("GET", "/api/v1/orders/0x1", Ok("{}".into()));
        venue.inject_fault(CowApiError::Fault(Fault::Unavailable("down".into())));

        assert!(matches!(
            venue.submit_order(1, b"{}"),
            Err(CowApiError::Fault(Fault::Unavailable(_))),
        ));
        assert!(
            venue
                .cow_api_request(1, "GET", "/api/v1/orders/0x1", None)
                .is_err()
        );

        // The outage consumed nothing: outcomes resume on recovery.
        venue.clear_fault();
        assert_eq!(venue.submit_order(1, b"{}").unwrap(), "0xuid");
        assert_eq!(
            venue
                .cow_api_request(1, "GET", "/api/v1/orders/0x1", None)
                .unwrap(),
            "{}",
        );
        assert_eq!(venue.call_count(), 2);
        assert_eq!(venue.request_calls().len(), 2);
    }

    #[test]
    fn venue_response_sequence_advances_and_final_entry_sticks() {
        let venue = MockVenue::default();
        for body in ["\"open\"", "\"open\"", "\"fulfilled\""] {
            venue.enqueue_order_status("0xuid", Ok(body.into()));
        }
        let probe = || {
            venue
                .cow_api_request(1, "GET", "/api/v1/orders/0xuid", None)
                .unwrap()
        };
        assert_eq!(probe(), "\"open\"");
        assert_eq!(probe(), "\"open\"");
        assert_eq!(probe(), "\"fulfilled\"");
        // The terminal entry replays for any later re-poll.
        assert_eq!(probe(), "\"fulfilled\"");
    }

    #[test]
    fn venue_unscripted_request_uses_the_fallback_then_defaults() {
        let venue = MockVenue::default();
        assert!(matches!(
            venue.cow_api_request(1, "GET", "/api/v1/anything", None),
            Err(CowApiError::Fault(Fault::Unsupported(_))),
        ));
        venue.set_response_fallback(Ok("catch-all".into()));
        assert_eq!(
            venue
                .cow_api_request(1, "GET", "/api/v1/anything", None)
                .unwrap(),
            "catch-all",
        );
    }

    #[test]
    fn mock_host_with_venue_dispatches_through_cow_host_bound() {
        let host = MockHost::with_venue();
        host.cow_api.enqueue_submit(Ok("0xuid".into()));

        let _: &dyn shepherd_sdk::cow::CowHost = &host;
        assert_eq!(host.submit_order(1, b"{}").unwrap(), "0xuid");
        assert_eq!(host.cow_api.call_count(), 1);
    }

    #[test]
    fn mock_host_dispatches_through_cow_host_bound() {
        let host = MockHost::new();
        host.chain
            .respond_to("eth_blockNumber", "[]", Ok("\"0x1\"".into()));
        host.cow_api.respond(Ok("0xuid".into()));

        // Through the `CowHost` bound.
        let _: &dyn shepherd_sdk::cow::CowHost = &host;
        host.set("key", b"val").unwrap();
        assert_eq!(host.get("key").unwrap().as_deref(), Some(&b"val"[..]));
        assert_eq!(host.request(1, "eth_blockNumber", "[]").unwrap(), "\"0x1\"");
        assert_eq!(host.submit_order(1, b"{}").unwrap(), "0xuid");
        host.log(Level::INFO, "happy path");

        assert_eq!(host.chain.call_count(), 1);
        assert_eq!(host.cow_api.call_count(), 1);
        assert_eq!(host.logging.lines().len(), 1);
        assert_eq!(host.store.len(), 1);
    }
}
