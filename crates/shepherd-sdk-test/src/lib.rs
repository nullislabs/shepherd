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
//! Modules that never touch the orderbook use `nexum-sdk-test`'s
//! `MockHost` directly instead.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![warn(missing_docs)]

use std::cell::RefCell;

use nexum_sdk::host::{ChainHost, HostError, LocalStoreHost, LogLevel, LoggingHost};
use nexum_sdk_test::{MockChain, MockLocalStore, MockLogging};
use shepherd_sdk::cow::CowApiHost;

/// Composed in-memory host for CoW modules: the generic per-trait
/// mocks plus [`MockCowApi`]. Each field exposes the per-trait mock so
/// tests can program responses and assert on calls.
#[derive(Default)]
pub struct MockHost {
    /// `nexum:host/chain` mock.
    pub chain: MockChain,
    /// `nexum:host/local-store` mock.
    pub store: MockLocalStore,
    /// `shepherd:cow/cow-api` mock.
    pub cow_api: MockCowApi,
    /// `nexum:host/logging` mock.
    pub logging: MockLogging,
}

impl MockHost {
    /// Fresh empty host. Equivalent to `Default::default`.
    pub fn new() -> Self {
        Self::default()
    }
}

impl ChainHost for MockHost {
    fn request(&self, chain_id: u64, method: &str, params: &str) -> Result<String, HostError> {
        self.chain.request(chain_id, method, params)
    }
}

impl LocalStoreHost for MockHost {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, HostError> {
        self.store.get(key)
    }
    fn set(&self, key: &str, value: &[u8]) -> Result<(), HostError> {
        self.store.set(key, value)
    }
    fn delete(&self, key: &str) -> Result<(), HostError> {
        self.store.delete(key)
    }
    fn list_keys(&self, prefix: &str) -> Result<Vec<String>, HostError> {
        self.store.list_keys(prefix)
    }
}

impl CowApiHost for MockHost {
    fn submit_order(&self, chain_id: u64, body: &[u8]) -> Result<String, HostError> {
        self.cow_api.submit_order(chain_id, body)
    }
    fn cow_api_request(
        &self,
        chain_id: u64,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<String, HostError> {
        self.cow_api.cow_api_request(chain_id, method, path, body)
    }
}

impl LoggingHost for MockHost {
    fn log(&self, level: LogLevel, message: &str) {
        self.logging.log(level, message);
    }
}

// ---------------------------------------------------------------- cow-api

/// In-memory [`CowApiHost`] that captures every submission and returns
/// a programmable response.
#[derive(Default)]
pub struct MockCowApi {
    response: RefCell<Option<Result<String, HostError>>>,
    calls: RefCell<Vec<SubmitCall>>,
    /// `cow_api_request` mock state. Keyed by `(method, path)` so
    /// tests can program different responses for `GET
    /// /api/v1/app_data/0x...` vs other endpoints. Falls back to the
    /// unkeyed `request_response` if no key matches.
    request_responses:
        RefCell<std::collections::HashMap<(String, String), Result<String, HostError>>>,
    request_response: RefCell<Option<Result<String, HostError>>>,
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
    /// `submit_order` call. Defaults to a host-side `Unsupported`
    /// error if unset.
    pub fn respond(&self, result: Result<String, HostError>) {
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
        result: Result<String, HostError>,
    ) {
        self.request_responses
            .borrow_mut()
            .insert((method.into(), path.into()), result);
    }

    /// Program the catch-all response for `cow_api_request` calls
    /// that don't match a specific `(method, path)` key. Defaults
    /// to host-side `Unsupported`.
    pub fn respond_to_request(&self, result: Result<String, HostError>) {
        *self.request_response.borrow_mut() = Some(result);
    }

    /// All `cow_api_request` invocations, in arrival order.
    pub fn request_calls(&self) -> Vec<RequestCall> {
        self.request_calls.borrow().clone()
    }
}

impl CowApiHost for MockCowApi {
    fn submit_order(&self, chain_id: u64, body: &[u8]) -> Result<String, HostError> {
        self.calls.borrow_mut().push(SubmitCall {
            chain_id,
            body: body.to_vec(),
        });
        self.response.borrow().clone().unwrap_or_else(|| {
            Err(HostError::unsupported(
                "cow-api",
                "MockCowApi: no response configured",
            ))
        })
    }

    fn cow_api_request(
        &self,
        chain_id: u64,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<String, HostError> {
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
            Err(HostError::unsupported(
                "cow-api",
                "MockCowApi: no cow_api_request response configured",
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use nexum_sdk::host::HostErrorKind;

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
        assert_eq!(err.kind, HostErrorKind::Unsupported);
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
        host.log(LogLevel::Info, "happy path");

        assert_eq!(host.chain.call_count(), 1);
        assert_eq!(host.cow_api.call_count(), 1);
        assert_eq!(host.logging.lines().len(), 1);
        assert_eq!(host.store.len(), 1);
    }
}
