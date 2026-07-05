//! # nexum-sdk-test
//!
//! In-memory implementations of the [`nexum_sdk::host`] traits
//! plus assertion helpers, so a module can write integration
//! tests for its strategy logic without `wit-bindgen`, `wasmtime`, or
//! a network round-trip.
//!
//! ## Usage
//!
//! Add as a dev-dep on the module crate:
//!
//! ```toml
//! [dev-dependencies]
//! nexum-sdk-test = { path = "../../crates/nexum-sdk-test" }
//! ```
//!
//! Structure the module's strategy function around the host traits:
//!
//! ```rust,ignore
//! pub fn handle_block<H: nexum_sdk::host::Host>(
//!     host: &H,
//!     chain_id: u64,
//!     block_number: u64,
//! ) -> Result<(), nexum_sdk::host::HostError> {
//!     // ...
//!     let res = host.request(chain_id, "eth_call", "[]")?;
//!     host.set("last_block", &block_number.to_le_bytes())?;
//!     host.log(nexum_sdk::Level::INFO, "saw block");
//!     Ok(())
//! }
//! ```
//!
//! Test against [`MockHost`]:
//!
//! ```rust
//! // Glob-import the host traits so the method shortcuts resolve.
//! use nexum_sdk::host::*;
//! use nexum_sdk_test::MockHost;
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
//! The traits use [`nexum_sdk::host::HostError`] rather than the
//! `HostError` `wit_bindgen::generate!` emits per-module. A module
//! bridges with two trivial `From` impls (one each direction) on its
//! own crate boundary - see the M3 tutorial for the exact
//! shape.
//!
//! Domain SDK test crates compose these mocks with their own (the CoW
//! `shepherd-sdk-test` embeds them next to its `MockCowApi`).

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![warn(missing_docs)]

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fmt::{self, Write as _};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use nexum_sdk::Level;
use nexum_sdk::host::{ChainHost, HostError, HostErrorKind, LocalStoreHost, LoggingHost};
use tracing::field::{Field, Visit};
use tracing::level_filters::LevelFilter;
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

/// Composed in-memory host. Each field exposes the per-trait mock so
/// tests can program responses and assert on calls.
#[derive(Default)]
pub struct MockHost {
    /// `nexum:host/chain` mock.
    pub chain: MockChain,
    /// `nexum:host/local-store` mock.
    pub store: MockLocalStore,
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

impl LoggingHost for MockHost {
    fn log(&self, level: Level, message: &str) {
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

// ---------------------------------------------------------------- logging

/// One recorded log line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogLine {
    /// Severity the module passed.
    pub level: Level,
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
    pub fn count_at(&self, level: Level) -> usize {
        self.lines
            .borrow()
            .iter()
            .filter(|l| l.level == level)
            .count()
    }
}

impl LoggingHost for MockLogging {
    fn log(&self, level: Level, message: &str) {
        self.lines.borrow_mut().push(LogLine {
            level,
            message: message.to_string(),
        });
    }
}

// ---------------------------------------------------------------- tracing capture

/// One tracing event captured pre-flattening.
#[derive(Clone, Debug, PartialEq)]
pub struct CapturedEvent {
    /// Event severity.
    pub level: Level,
    /// Callsite target (module path by default).
    pub target: String,
    /// The `message` field; empty when the event carried none.
    pub message: String,
    /// Every non-message field, keyed by name.
    pub fields: BTreeMap<String, FieldValue>,
}

/// A field value as tracing's `Visit` delivered it.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldValue {
    /// A `record_str` value.
    Str(String),
    /// A `record_u64` value.
    U64(u64),
    /// A `record_i64` value.
    I64(i64),
    /// A `record_bool` value.
    Bool(bool),
    /// A `record_debug` fallback (`?x`, `%x`, `f64`, ...), pre-rendered
    /// with `{:?}`.
    Debug(String),
}

impl fmt::Display for FieldValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FieldValue::Str(v) | FieldValue::Debug(v) => f.write_str(v),
            FieldValue::U64(v) => write!(f, "{v}"),
            FieldValue::I64(v) => write!(f, "{v}"),
            FieldValue::Bool(v) => write!(f, "{v}"),
        }
    }
}

impl CapturedEvent {
    /// The value recorded for `name`, if the event carried it.
    pub fn field(&self, name: &str) -> Option<&FieldValue> {
        self.fields.get(name)
    }

    /// Display-rendered field, for string comparisons.
    pub fn field_str(&self, name: &str) -> Option<String> {
        self.fields.get(name).map(FieldValue::to_string)
    }
}

/// Events captured during [`capture_tracing`].
pub struct CapturedEvents {
    events: Arc<Mutex<Vec<CapturedEvent>>>,
}

impl CapturedEvents {
    /// Every captured event, in emission order.
    pub fn events(&self) -> Vec<CapturedEvent> {
        self.events.lock().unwrap().clone()
    }

    /// Whether no events were captured.
    pub fn is_empty(&self) -> bool {
        self.events.lock().unwrap().is_empty()
    }

    /// Count of events at `level`.
    pub fn count_at(&self, level: Level) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.level == level)
            .count()
    }

    /// Whether any captured event satisfies `pred`.
    pub fn any(&self, pred: impl Fn(&CapturedEvent) -> bool) -> bool {
        self.events.lock().unwrap().iter().any(pred)
    }

    /// Exactly one matching event; panics with the full capture dump
    /// otherwise.
    pub fn expect_one(&self, pred: impl Fn(&CapturedEvent) -> bool) -> CapturedEvent {
        let events = self.events.lock().unwrap();
        let matches: Vec<&CapturedEvent> = events.iter().filter(|e| pred(e)).collect();
        match matches.as_slice() {
            [only] => (*only).clone(),
            other => panic!(
                "expected exactly one matching event, found {}; captured: {events:#?}",
                other.len(),
            ),
        }
    }
}

type Buffer = Arc<Mutex<Vec<CapturedEvent>>>;

std::thread_local! {
    /// The capture buffer active on this thread, if any. `capture_tracing`
    /// installs one for the duration of `f` and restores the prior slot on
    /// return or unwind.
    static ACTIVE_CAPTURE: RefCell<Option<Buffer>> = const { RefCell::new(None) };
}

/// Restores the previous thread-local capture slot when a
/// `capture_tracing` call returns or unwinds.
struct CaptureGuard(Option<Buffer>);

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        ACTIVE_CAPTURE.with(|slot| *slot.borrow_mut() = self.0.take());
    }
}

/// Events-only subscriber that records each event as a typed
/// [`CapturedEvent`] into the buffer active on the emitting thread,
/// dropping events when none is set. Spans are inert.
struct CaptureSubscriber {
    next_id: AtomicU64,
}

impl Subscriber for CaptureSubscriber {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(LevelFilter::TRACE)
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        // Spans are inert, but a valid non-zero id must be returned.
        let raw = self.next_id.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        Id::from_u64(raw.max(1))
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        let captured = CapturedEvent {
            level: *event.metadata().level(),
            target: event.metadata().target().to_owned(),
            message: visitor.message,
            fields: visitor.fields,
        };
        ACTIVE_CAPTURE.with(|slot| {
            if let Some(buffer) = slot.borrow().as_ref() {
                buffer.lock().unwrap().push(captured);
            }
        });
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

/// Splits an event into its `message` field and a name-keyed map of the
/// rest, mirroring the facade's dispatch so captured values match the
/// rendered line field for field.
#[derive(Default)]
struct FieldVisitor {
    message: String,
    fields: BTreeMap<String, FieldValue>,
}

impl Visit for FieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            // `write!` on `fmt::Arguments` renders unquoted, matching the
            // facade's message rendering.
            let _ = write!(self.message, "{value:?}");
        } else {
            self.fields.insert(
                field.name().to_owned(),
                FieldValue::Debug(format!("{value:?}")),
            );
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            self.fields
                .insert(field.name().to_owned(), FieldValue::Str(value.to_owned()));
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields
            .insert(field.name().to_owned(), FieldValue::U64(value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields
            .insert(field.name().to_owned(), FieldValue::I64(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields
            .insert(field.name().to_owned(), FieldValue::Bool(value));
    }
}

static INSTALL_ROUTING: std::sync::Once = std::sync::Once::new();

/// Run `f`, returning its value and every `tracing` event it emitted.
///
/// Capture routes through a single process-global default subscriber
/// installed on first use, keyed to the emitting thread by a thread-local
/// buffer. A process-global default is required rather than a
/// `with_default` scoped one: `tracing` caches each callsite's `Interest`
/// the first time the callsite is hit, computed against whichever
/// dispatcher is current on that thread at that instant. Under parallel
/// tests a callsite exercised outside any capture (e.g. a sibling test
/// calling the same strategy function directly) registers against the
/// no-op default and is cached `never` for the rest of the process,
/// silently starving every later scoped capture of that event. Installing
/// the capture subscriber as the global default makes the cached interest
/// stable and capture independent of test scheduling.
pub fn capture_tracing<R>(f: impl FnOnce() -> R) -> (R, CapturedEvents) {
    INSTALL_ROUTING.call_once(|| {
        let _ = tracing::subscriber::set_global_default(CaptureSubscriber {
            next_id: AtomicU64::new(0),
        });
    });

    let events: Buffer = Arc::new(Mutex::new(Vec::new()));
    let previous = ACTIVE_CAPTURE.with(|slot| slot.borrow_mut().replace(Arc::clone(&events)));
    let _guard = CaptureGuard(previous);
    let result = f();
    drop(_guard);
    (result, CapturedEvents { events })
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
    fn logging_captures_lines_and_filters_by_level() {
        let log = MockLogging::default();
        log.log(Level::INFO, "hello");
        log.log(Level::WARN, "uh oh");
        log.log(Level::INFO, "still here");

        assert_eq!(log.lines().len(), 3);
        assert_eq!(log.count_at(Level::INFO), 2);
        assert_eq!(log.count_at(Level::WARN), 1);
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

        // Through the `Host` supertrait.
        let _: &dyn nexum_sdk::host::Host = &host;
        host.set("key", b"val").unwrap();
        assert_eq!(host.get("key").unwrap().as_deref(), Some(&b"val"[..]));
        assert_eq!(host.request(1, "eth_blockNumber", "[]").unwrap(), "\"0x1\"");
        host.log(Level::INFO, "happy path");

        assert_eq!(host.chain.call_count(), 1);
        assert_eq!(host.logging.lines().len(), 1);
        assert_eq!(host.store.len(), 1);
    }

    #[test]
    fn capture_message_only_event_has_empty_fields() {
        let (_, logs) = capture_tracing(|| tracing::info!("hello"));
        let events = logs.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].level, Level::INFO);
        assert_eq!(events[0].message, "hello");
        assert!(events[0].fields.is_empty());
    }

    #[test]
    fn capture_fields_land_as_typed_values() {
        let (_, logs) = capture_tracing(|| {
            tracing::warn!(
                name = "eth",
                count = 7u64,
                signed = -3i64,
                ready = true,
                answer = ?Some(9),
                "changed",
            );
        });
        let ev = logs.expect_one(|e| e.level == Level::WARN);
        assert_eq!(ev.message, "changed");
        assert_eq!(ev.field("name"), Some(&FieldValue::Str("eth".to_owned())));
        assert_eq!(ev.field("count"), Some(&FieldValue::U64(7)));
        assert_eq!(ev.field("signed"), Some(&FieldValue::I64(-3)));
        assert_eq!(ev.field("ready"), Some(&FieldValue::Bool(true)));
        assert_eq!(
            ev.field("answer"),
            Some(&FieldValue::Debug("Some(9)".to_owned())),
        );
    }

    #[test]
    fn capture_display_recorded_value_lands_as_debug() {
        let (_, logs) = capture_tracing(|| tracing::info!(x = %42u32, "shown"));
        let ev = logs.expect_one(|e| e.message == "shown");
        assert!(matches!(ev.field("x"), Some(FieldValue::Debug(_))));
        assert_eq!(ev.field_str("x").as_deref(), Some("42"));
    }

    #[test]
    fn events_outside_capture_are_dropped() {
        // Prime the global default via one capture, then emit outside any.
        let (_, _) = capture_tracing(|| tracing::info!("primed"));
        tracing::info!("orphan");
        let (_, logs) = capture_tracing(|| tracing::info!("inside"));
        let events = logs.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].message, "inside");
    }

    #[test]
    fn concurrent_captures_are_thread_isolated() {
        use std::sync::Barrier;
        let barrier = Arc::new(Barrier::new(2));
        let other = Arc::clone(&barrier);
        let handle = std::thread::spawn(move || {
            let (_, logs) = capture_tracing(|| {
                other.wait();
                tracing::info!("thread-one");
            });
            logs.events()
        });
        let (_, main_logs) = capture_tracing(|| {
            barrier.wait();
            tracing::info!("thread-two");
        });
        let thread_events = handle.join().unwrap();

        assert_eq!(main_logs.events().len(), 1);
        assert_eq!(main_logs.events()[0].message, "thread-two");
        assert_eq!(thread_events.len(), 1);
        assert_eq!(thread_events[0].message, "thread-one");
    }
}
