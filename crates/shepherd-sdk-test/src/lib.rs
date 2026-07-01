//! # shepherd-sdk-test
//!
//! In-memory implementations of the [`shepherd_sdk::host`] traits
//! plus assertion helpers, so a Shepherd module can write integration
//! tests for its strategy logic without `wit-bindgen`, `wasmtime`, or
//! a network round-trip.
//!
//! ## Usage
//!
//! Add as a dev-dep on the module crate:
//!
//! ```toml
//! [dev-dependencies]
//! shepherd-sdk-test = { path = "../../crates/shepherd-sdk-test" }
//! ```
//!
//! Structure the module's strategy function around the host traits:
//!
//! ```rust,ignore
//! pub fn handle_block<H: shepherd_sdk::host::Host>(
//!     host: &H,
//!     chain_id: u64,
//!     block_number: u64,
//! ) -> Result<(), shepherd_sdk::host::HostError> {
//!     // ...
//!     let res = host.request(chain_id, "eth_call", "[]")?;
//!     host.set("last_block", &block_number.to_le_bytes())?;
//!     host.log(shepherd_sdk::host::LogLevel::Info, "saw block");
//!     Ok(())
//! }
//! ```
//!
//! Test against [`MockHost`]:
//!
//! ```rust
//! // Glob-import the host traits so the method shortcuts resolve.
//! use shepherd_sdk::host::*;
//! use shepherd_sdk_test::MockHost;
//!
//! let host = MockHost::new();
//! host.chain.respond_to("eth_blockNumber", "[]", Ok("\"0x1\"".into()));
//!
//! // Call the strategy directly:
//! assert_eq!(host.request(1, "eth_blockNumber", "[]").unwrap(), "\"0x1\"");
//!
//! // Inspect:
//! assert_eq!(host.chain.calls().len(), 1);
//! ```
//!
//! ## Adapting from wit-bindgen
//!
//! The traits use [`shepherd_sdk::host::HostError`] rather than the
//! `HostError` `wit_bindgen::generate!` emits per-module. A module
//! bridges with two trivial `From` impls (one each direction) on its
//! own crate boundary - see the M3 tutorial for the exact
//! shape.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![warn(missing_docs)]

use std::cell::RefCell;
use std::collections::HashMap;

use shepherd_sdk::host::{
    ChainHost, CowApiHost, HostError, HostErrorKind, LocalStoreHost, LogLevel, LoggingHost,
};

/// Composed in-memory host. Each field exposes the per-trait mock so
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

// ---------------------------------------------------------------- chain

/// In-memory [`ChainHost`] backed by a `(method, params)` -> response
/// map. Records every call so tests can assert dispatch shape.
#[derive(Default)]
pub struct MockChain {
    responses: RefCell<HashMap<(String, String), Result<String, HostError>>>,
    calls: RefCell<Vec<ChainCall>>,
}

/// One recorded [`MockChain::request`] invocation.
#[derive(Clone, Debug)]
pub struct ChainCall {
    /// EVM chain id the guest passed.
    pub chain_id: u64,
    /// JSON-RPC method name.
    pub method: String,
    /// JSON-encoded params array (verbatim).
    pub params: String,
}

impl MockChain {
    /// Program a response for the `(method, params)` pair. Overwrites
    /// any prior entry.
    pub fn respond_to(
        &self,
        method: impl Into<String>,
        params: impl Into<String>,
        result: Result<String, HostError>,
    ) {
        self.responses
            .borrow_mut()
            .insert((method.into(), params.into()), result);
    }

    /// All calls received, in arrival order.
    pub fn calls(&self) -> Vec<ChainCall> {
        self.calls.borrow().clone()
    }

    /// Last call received, if any.
    pub fn last_call(&self) -> Option<ChainCall> {
        self.calls.borrow().last().cloned()
    }

    /// Total call count.
    pub fn call_count(&self) -> usize {
        self.calls.borrow().len()
    }
}

impl ChainHost for MockChain {
    fn request(&self, chain_id: u64, method: &str, params: &str) -> Result<String, HostError> {
        self.calls.borrow_mut().push(ChainCall {
            chain_id,
            method: method.to_string(),
            params: params.to_string(),
        });
        self.responses
            .borrow()
            .get(&(method.to_string(), params.to_string()))
            .cloned()
            .unwrap_or_else(|| {
                Err(HostError {
                    domain: "chain".into(),
                    kind: HostErrorKind::Unsupported,
                    code: 0,
                    message: format!("MockChain: no response configured for {method} {params}"),
                    data: None,
                })
            })
    }
}

// ---------------------------------------------------------------- local-store

/// In-memory [`LocalStoreHost`] backed by a `HashMap`. Each operation
/// runs in O(1) except `list_keys`, which scans (small N expected for
/// tests).
///
/// Supports optional error injection via [`MockLocalStore::fail_on`]
/// and entry-count limits via [`MockLocalStore::set_max_entries`].
#[derive(Default)]
pub struct MockLocalStore {
    rows: RefCell<HashMap<String, Vec<u8>>>,
    /// When set, `set` returns `StorageFull` if the store reaches this many entries.
    max_entries: RefCell<Option<usize>>,
    /// Key patterns that trigger injected errors on any operation.
    error_patterns: RefCell<Vec<(String, HostError)>>,
}

impl MockLocalStore {
    /// Number of rows currently held.
    pub fn len(&self) -> usize {
        self.rows.borrow().len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.rows.borrow().is_empty()
    }

    /// Direct read for assertions - bypasses the trait.
    pub fn snapshot(&self) -> HashMap<String, Vec<u8>> {
        self.rows.borrow().clone()
    }

    /// Set a maximum number of entries. Once reached, `set` on a new
    /// key returns a `StorageFull` error. `None` disables the limit.
    pub fn set_max_entries(&self, limit: usize) {
        *self.max_entries.borrow_mut() = Some(limit);
    }

    /// Inject an error for any operation where the key starts with
    /// `prefix`. Multiple patterns can be registered; the first
    /// matching one fires.
    pub fn fail_on(&self, prefix: impl Into<String>, error: HostError) {
        self.error_patterns
            .borrow_mut()
            .push((prefix.into(), error));
    }

    fn check_injected_error(&self, key: &str) -> Result<(), HostError> {
        for (pattern, error) in self.error_patterns.borrow().iter() {
            if key.starts_with(pattern) {
                return Err(error.clone());
            }
        }
        Ok(())
    }
}

impl LocalStoreHost for MockLocalStore {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, HostError> {
        self.check_injected_error(key)?;
        Ok(self.rows.borrow().get(key).cloned())
    }
    fn set(&self, key: &str, value: &[u8]) -> Result<(), HostError> {
        self.check_injected_error(key)?;
        if let Some(limit) = *self.max_entries.borrow() {
            let rows = self.rows.borrow();
            if rows.len() >= limit && !rows.contains_key(key) {
                return Err(HostError {
                    domain: "local-store".into(),
                    kind: HostErrorKind::Internal,
                    code: 0,
                    message: format!("MockLocalStore: max entries ({limit}) reached"),
                    data: None,
                });
            }
        }
        self.rows
            .borrow_mut()
            .insert(key.to_string(), value.to_vec());
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<(), HostError> {
        self.check_injected_error(key)?;
        self.rows.borrow_mut().remove(key);
        Ok(())
    }
    fn list_keys(&self, prefix: &str) -> Result<Vec<String>, HostError> {
        self.check_injected_error(prefix)?;
        let mut keys: Vec<String> = self
            .rows
            .borrow()
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect();
        keys.sort();
        Ok(keys)
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

// ---------------------------------------------------------------- logging

/// One recorded log line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogLine {
    /// Severity the module passed.
    pub level: LogLevel,
    /// Message body.
    pub message: String,
}

/// In-memory [`LoggingHost`] that buffers every emitted line.
#[derive(Default)]
pub struct MockLogging {
    lines: RefCell<Vec<LogLine>>,
}

impl MockLogging {
    /// All buffered log lines, in emission order.
    pub fn lines(&self) -> Vec<LogLine> {
        self.lines.borrow().clone()
    }

    /// `true` if any buffered line contains `needle` (substring match).
    pub fn contains(&self, needle: &str) -> bool {
        self.lines
            .borrow()
            .iter()
            .any(|l| l.message.contains(needle))
    }

    /// Count of lines at `level`.
    pub fn count_at(&self, level: LogLevel) -> usize {
        self.lines
            .borrow()
            .iter()
            .filter(|l| l.level == level)
            .count()
    }
}

impl LoggingHost for MockLogging {
    fn log(&self, level: LogLevel, message: &str) {
        self.lines.borrow_mut().push(LogLine {
            level,
            message: message.to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_records_calls_and_returns_programmed_response() {
        let chain = MockChain::default();
        chain.respond_to("eth_blockNumber", "[]", Ok("\"0x1234\"".into()));

        assert_eq!(
            chain.request(1, "eth_blockNumber", "[]").unwrap(),
            "\"0x1234\""
        );
        assert_eq!(chain.call_count(), 1);
        let last = chain.last_call().unwrap();
        assert_eq!(last.chain_id, 1);
        assert_eq!(last.method, "eth_blockNumber");
    }

    #[test]
    fn chain_unconfigured_method_returns_unsupported() {
        let chain = MockChain::default();
        let err = chain.request(1, "eth_call", "[]").unwrap_err();
        assert_eq!(err.kind, HostErrorKind::Unsupported);
        assert!(err.message.contains("MockChain"));
        assert_eq!(chain.call_count(), 1);
    }

    #[test]
    fn local_store_round_trips() {
        let store = MockLocalStore::default();
        store.set("k", b"v").unwrap();
        assert_eq!(store.get("k").unwrap().as_deref(), Some(&b"v"[..]));
        store.delete("k").unwrap();
        assert!(store.get("k").unwrap().is_none());
    }

    #[test]
    fn local_store_list_keys_prefix_scan() {
        let store = MockLocalStore::default();
        store.set("watch:a:1", b"").unwrap();
        store.set("watch:a:2", b"").unwrap();
        store.set("submitted:1", b"").unwrap();
        let keys = store.list_keys("watch:").unwrap();
        assert_eq!(keys, vec!["watch:a:1", "watch:a:2"]);
    }

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
    fn logging_captures_lines_and_filters_by_level() {
        let log = MockLogging::default();
        log.log(LogLevel::Info, "hello");
        log.log(LogLevel::Warn, "uh oh");
        log.log(LogLevel::Info, "still here");

        assert_eq!(log.lines().len(), 3);
        assert_eq!(log.count_at(LogLevel::Info), 2);
        assert_eq!(log.count_at(LogLevel::Warn), 1);
        assert!(log.contains("uh oh"));
    }

    #[test]
    fn local_store_error_injection() {
        let store = MockLocalStore::default();
        store.fail_on(
            "bad:",
            HostError {
                domain: "local-store".into(),
                kind: HostErrorKind::Internal,
                code: 0,
                message: "injected".into(),
                data: None,
            },
        );
        // Non-matching keys work fine.
        store.set("good:k", b"v").unwrap();
        assert_eq!(store.get("good:k").unwrap().as_deref(), Some(&b"v"[..]));
        // Matching keys trigger the error.
        assert!(store.set("bad:k", b"v").is_err());
        assert!(store.get("bad:k").is_err());
        assert!(store.delete("bad:k").is_err());
        assert!(store.list_keys("bad:").is_err());
    }

    #[test]
    fn local_store_max_entries_enforced() {
        let store = MockLocalStore::default();
        store.set_max_entries(2);
        store.set("a", b"1").unwrap();
        store.set("b", b"2").unwrap();
        // Updating an existing key is OK even at the limit.
        store.set("b", b"3").unwrap();
        // Adding a new key exceeds the limit.
        let err = store.set("c", b"4").unwrap_err();
        assert!(err.message.contains("max entries"));
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn mock_host_dispatches_through_supertrait() {
        let host = MockHost::new();
        host.chain
            .respond_to("eth_blockNumber", "[]", Ok("\"0x1\"".into()));
        host.cow_api.respond(Ok("0xuid".into()));

        // Through the `Host` supertrait.
        let _: &dyn shepherd_sdk::host::Host = &host;
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
