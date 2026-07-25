//! In-memory [`nexum_sdk::host`] trait implementations plus assertion
//! helpers, so a module can test its strategy logic without wit-bindgen,
//! wasmtime, or a network round-trip.
//!
//! [`MockHost`] composes the six per-seam mocks ([`MockChain`],
//! [`MockIdentity`], [`MockLocalStore`], [`MockRemoteStore`],
//! [`MockMessaging`], [`MockLogging`]); [`capture_tracing`] records
//! emitted `tracing` events.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![warn(missing_docs)]

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::fmt::{self, Write as _};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use nexum_sdk::Level;
use nexum_sdk::host::{
    ChainError, ChainHost, Fault, IdentityHost, LocalStoreHost, LoggingHost, Message,
    MessagingHost, RemoteStoreHost,
};
use nexum_sdk::prelude::{Address, B256, Signature, keccak256};
use tracing::field::{Field, Visit};
use tracing::level_filters::LevelFilter;
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

/// Composed in-memory host; each field is the per-seam mock.
#[derive(Default)]
pub struct MockHost {
    /// `nexum:host/chain` mock.
    pub chain: MockChain,
    /// `nexum:host/identity` mock.
    pub identity: MockIdentity,
    /// `nexum:host/local-store` mock.
    pub store: MockLocalStore,
    /// `nexum:host/remote-store` mock.
    pub remote_store: MockRemoteStore,
    /// `nexum:host/messaging` mock.
    pub messaging: MockMessaging,
    /// `nexum:host/logging` mock.
    pub logging: MockLogging,
}

impl MockHost {
    /// Fresh empty host.
    pub fn new() -> Self {
        Self::default()
    }
}

impl ChainHost for MockHost {
    fn request(&self, chain_id: u64, method: &str, params: &str) -> Result<String, ChainError> {
        self.chain.request(chain_id, method, params)
    }
}

impl LocalStoreHost for MockHost {
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
    fn contains(&self, key: &str) -> Result<bool, Fault> {
        self.store.contains(key)
    }
    fn len(&self, key: &str) -> Result<Option<u64>, Fault> {
        // Qualified: the mock's inherent `len` counts rows.
        LocalStoreHost::len(&self.store, key)
    }
    fn count(&self, prefix: &str) -> Result<u64, Fault> {
        self.store.count(prefix)
    }
}

impl IdentityHost for MockHost {
    fn accounts(&self) -> Result<Vec<Address>, Fault> {
        self.identity.accounts()
    }
    fn sign(&self, account: Address, message: &[u8]) -> Result<Signature, Fault> {
        self.identity.sign(account, message)
    }
    fn sign_typed_data(&self, account: Address, typed_data: &str) -> Result<Signature, Fault> {
        self.identity.sign_typed_data(account, typed_data)
    }
}

impl RemoteStoreHost for MockHost {
    fn upload(&self, data: &[u8]) -> Result<B256, Fault> {
        self.remote_store.upload(data)
    }
    fn download(&self, reference: B256) -> Result<Vec<u8>, Fault> {
        self.remote_store.download(reference)
    }
    fn read_feed(&self, owner: Address, topic: B256) -> Result<Option<Vec<u8>>, Fault> {
        self.remote_store.read_feed(owner, topic)
    }
    fn write_feed(&self, topic: B256, data: &[u8]) -> Result<B256, Fault> {
        self.remote_store.write_feed(topic, data)
    }
}

impl MessagingHost for MockHost {
    fn publish(&self, content_topic: &str, payload: &[u8]) -> Result<(), Fault> {
        self.messaging.publish(content_topic, payload)
    }
    fn query(
        &self,
        content_topic: &str,
        start_time: Option<u64>,
        end_time: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Vec<Message>, Fault> {
        self.messaging
            .query(content_topic, start_time, end_time, limit)
    }
}

impl LoggingHost for MockHost {
    fn log(&self, level: Level, message: &str) {
        self.logging.log(level, message);
    }
}

// ---------------------------------------------------------------- chain

/// In-memory [`ChainHost`] over a `(method, params)` response map;
/// records every call.
#[derive(Default)]
pub struct MockChain {
    responses: RefCell<HashMap<(String, String), Result<String, ChainError>>>,
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
    /// Program the response for `(method, params)`; overwrites any prior entry.
    pub fn respond_to(
        &self,
        method: impl Into<String>,
        params: impl Into<String>,
        result: Result<String, ChainError>,
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
    fn request(&self, chain_id: u64, method: &str, params: &str) -> Result<String, ChainError> {
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
                Err(ChainError::Fault(Fault::Unsupported(format!(
                    "MockChain: no response configured for {method} {params}"
                ))))
            })
    }
}

// ---------------------------------------------------------------- identity

/// One recorded [`MockIdentity`] signing invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignCall {
    /// Account the guest asked to sign with.
    pub account: Address,
    /// What was signed.
    pub payload: SignPayload,
}

/// The payload of a [`SignCall`], per signing entry point.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SignPayload {
    /// A `sign` call: raw message bytes (`personal_sign` semantics).
    Message(Vec<u8>),
    /// A `sign_typed_data` call: the JSON-encoded EIP-712 payload.
    TypedData(String),
}

/// In-memory [`IdentityHost`] with a programmable roster and one signing
/// outcome; records every call. Off-roster accounts fail
/// [`Fault::Denied`]; with no outcome programmed signing fails
/// [`Fault::Unsupported`].
#[derive(Default)]
pub struct MockIdentity {
    accounts: RefCell<Vec<Address>>,
    response: RefCell<Option<Result<Signature, Fault>>>,
    calls: RefCell<Vec<SignCall>>,
}

impl MockIdentity {
    /// Add an account to the roster.
    pub fn add_account(&self, account: Address) {
        self.accounts.borrow_mut().push(account);
    }

    /// Program the outcome every subsequent signing call returns.
    pub fn respond(&self, result: Result<Signature, Fault>) {
        *self.response.borrow_mut() = Some(result);
    }

    /// All signing calls received, in arrival order.
    pub fn calls(&self) -> Vec<SignCall> {
        self.calls.borrow().clone()
    }

    /// Last signing call received, if any.
    pub fn last_call(&self) -> Option<SignCall> {
        self.calls.borrow().last().cloned()
    }

    /// Total signing call count.
    pub fn call_count(&self) -> usize {
        self.calls.borrow().len()
    }

    fn dispatch(&self, account: Address, payload: SignPayload) -> Result<Signature, Fault> {
        self.calls.borrow_mut().push(SignCall { account, payload });
        if !self.accounts.borrow().contains(&account) {
            return Err(Fault::Denied(format!(
                "MockIdentity: account {account} is not held"
            )));
        }
        self.response.borrow().clone().unwrap_or_else(|| {
            Err(Fault::Unsupported(
                "MockIdentity: no signing outcome programmed".to_string(),
            ))
        })
    }
}

impl IdentityHost for MockIdentity {
    fn accounts(&self) -> Result<Vec<Address>, Fault> {
        Ok(self.accounts.borrow().clone())
    }

    fn sign(&self, account: Address, message: &[u8]) -> Result<Signature, Fault> {
        self.dispatch(account, SignPayload::Message(message.to_vec()))
    }

    fn sign_typed_data(&self, account: Address, typed_data: &str) -> Result<Signature, Fault> {
        self.dispatch(account, SignPayload::TypedData(typed_data.to_owned()))
    }
}

// ---------------------------------------------------------------- messaging

/// One recorded [`MessagingHost::publish`] invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishRecord {
    /// Content topic published to.
    pub content_topic: String,
    /// Payload bytes, verbatim.
    pub payload: Vec<u8>,
}

/// In-memory [`MessagingHost`]: seeded messages answer queries, publishes
/// are recorded, an optional scope mirrors the `messaging_topics` grant.
/// Queries answer from seeds, never from what the guest published.
#[derive(Default)]
pub struct MockMessaging {
    history: RefCell<Vec<Message>>,
    published: RefCell<Vec<PublishRecord>>,
    scope: RefCell<Option<Vec<String>>>,
    faults: RefCell<Vec<(String, Fault)>>,
}

impl MockMessaging {
    /// Seed one message into the queryable history.
    pub fn seed(&self, message: Message) {
        self.history.borrow_mut().push(message);
    }

    /// Seed a payload on `content_topic` at `timestamp` (ms since the
    /// Unix epoch, UTC), no sender.
    pub fn seed_payload(
        &self,
        content_topic: impl Into<String>,
        payload: impl Into<Vec<u8>>,
        timestamp: u64,
    ) {
        self.seed(Message {
            content_topic: content_topic.into(),
            payload: payload.into(),
            timestamp,
            sender: None,
        });
    }

    /// Confine the mock to `topics`, mirroring the `messaging_topics`
    /// grant: a topic is admitted if it equals an entry or descends from
    /// one as a `/`-bounded prefix, else [`Fault::Denied`]. An empty
    /// grant is unscoped.
    pub fn scope_topics(&self, topics: impl IntoIterator<Item = impl Into<String>>) {
        *self.scope.borrow_mut() = Some(topics.into_iter().map(Into::into).collect());
    }

    /// Inject a fault for any operation on a topic starting with
    /// `prefix`; first registered match fires.
    pub fn fail_on(&self, prefix: impl Into<String>, fault: Fault) {
        self.faults.borrow_mut().push((prefix.into(), fault));
    }

    /// All publishes received, in arrival order.
    pub fn published(&self) -> Vec<PublishRecord> {
        self.published.borrow().clone()
    }

    /// Last publish received, if any.
    pub fn last_published(&self) -> Option<PublishRecord> {
        self.published.borrow().last().cloned()
    }

    /// Total publish count.
    pub fn publish_count(&self) -> usize {
        self.published.borrow().len()
    }

    fn admit(&self, content_topic: &str) -> Result<(), Fault> {
        for (prefix, fault) in self.faults.borrow().iter() {
            if content_topic.starts_with(prefix.as_str()) {
                return Err(fault.clone());
            }
        }
        if let Some(scope) = self.scope.borrow().as_ref()
            && !topic_in_scope(content_topic, scope)
        {
            return Err(Fault::Denied(format!(
                "MockMessaging: {content_topic} is outside the scoped topics"
            )));
        }
        Ok(())
    }
}

/// Grant matching: empty scope admits all; else a topic must equal an
/// entry or descend from one as a `/`-bounded prefix.
fn topic_in_scope(topic: &str, scope: &[String]) -> bool {
    if scope.is_empty() {
        return true;
    }
    scope.iter().any(|allowed| {
        if topic == allowed {
            return true;
        }
        let prefix = allowed.strip_suffix('/').unwrap_or(allowed);
        topic
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
    })
}

impl MessagingHost for MockMessaging {
    fn publish(&self, content_topic: &str, payload: &[u8]) -> Result<(), Fault> {
        self.admit(content_topic)?;
        self.published.borrow_mut().push(PublishRecord {
            content_topic: content_topic.to_owned(),
            payload: payload.to_vec(),
        });
        Ok(())
    }

    /// Exact-topic seeds within the inclusive `start_time..=end_time`
    /// window, in seed order; `limit` keeps the newest, the tail.
    fn query(
        &self,
        content_topic: &str,
        start_time: Option<u64>,
        end_time: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Vec<Message>, Fault> {
        self.admit(content_topic)?;
        let mut matches: Vec<Message> = self
            .history
            .borrow()
            .iter()
            .filter(|message| {
                message.content_topic == content_topic
                    && start_time.is_none_or(|start| message.timestamp >= start)
                    && end_time.is_none_or(|end| message.timestamp <= end)
            })
            .cloned()
            .collect();
        if let Some(limit) = limit {
            let keep = usize::try_from(limit).unwrap_or(usize::MAX);
            if matches.len() > keep {
                matches.drain(..matches.len() - keep);
            }
        }
        Ok(matches)
    }
}

// ---------------------------------------------------------------- remote-store

/// In-memory [`RemoteStoreHost`]: `keccak256`-addressed blobs plus
/// mutable `(owner, topic)` feeds. Feed writes land under the mock's own
/// owner ([`set_owner`](Self::set_owner), zero by default).
#[derive(Default)]
pub struct MockRemoteStore {
    blobs: RefCell<HashMap<B256, Vec<u8>>>,
    feeds: RefCell<HashMap<(Address, B256), Vec<u8>>>,
    owner: Cell<Address>,
    fault: RefCell<Option<Fault>>,
}

impl MockRemoteStore {
    /// Set the owner feed writes land under.
    pub fn set_owner(&self, owner: Address) {
        self.owner.set(owner);
    }

    /// Seed a blob directly; returns its reference.
    pub fn seed_blob(&self, data: impl Into<Vec<u8>>) -> B256 {
        let data = data.into();
        let reference = keccak256(&data);
        self.blobs.borrow_mut().insert(reference, data);
        reference
    }

    /// Seed another owner's feed.
    pub fn seed_feed(&self, owner: Address, topic: B256, data: impl Into<Vec<u8>>) {
        self.feeds.borrow_mut().insert((owner, topic), data.into());
    }

    /// Inject a fault every subsequent operation returns.
    pub fn fail_with(&self, fault: Fault) {
        *self.fault.borrow_mut() = Some(fault);
    }

    /// Number of stored blobs.
    pub fn blob_count(&self) -> usize {
        self.blobs.borrow().len()
    }

    fn check_injected_fault(&self) -> Result<(), Fault> {
        match self.fault.borrow().as_ref() {
            Some(fault) => Err(fault.clone()),
            None => Ok(()),
        }
    }
}

impl RemoteStoreHost for MockRemoteStore {
    fn upload(&self, data: &[u8]) -> Result<B256, Fault> {
        self.check_injected_fault()?;
        Ok(self.seed_blob(data))
    }

    fn download(&self, reference: B256) -> Result<Vec<u8>, Fault> {
        self.check_injected_fault()?;
        self.blobs
            .borrow()
            .get(&reference)
            .cloned()
            .ok_or_else(|| Fault::Unavailable(format!("MockRemoteStore: no blob at {reference}")))
    }

    fn read_feed(&self, owner: Address, topic: B256) -> Result<Option<Vec<u8>>, Fault> {
        self.check_injected_fault()?;
        Ok(self.feeds.borrow().get(&(owner, topic)).cloned())
    }

    fn write_feed(&self, topic: B256, data: &[u8]) -> Result<B256, Fault> {
        self.check_injected_fault()?;
        let reference = self.seed_blob(data);
        self.feeds
            .borrow_mut()
            .insert((self.owner.get(), topic), data.to_vec());
        Ok(reference)
    }
}

// ---------------------------------------------------------------- local-store

/// In-memory [`LocalStoreHost`]: namespaced views over one shared row
/// map, with store-wide entry and byte limits.
/// [`namespaced`](Self::namespaced) derives a sibling view over the same
/// rows; identical keys in different namespaces never collide, and limits
/// are shared across namespaces. Every `set` commits immediately, with no
/// transaction rollback on trap.
#[derive(Default)]
pub struct MockLocalStore {
    shared: Rc<SharedRows>,
    namespace: String,
    /// Key patterns that trigger injected faults on any operation.
    error_patterns: RefCell<Vec<(String, Fault)>>,
}

/// Backing rows and limits shared by every namespaced view.
#[derive(Default)]
struct SharedRows {
    /// Rows keyed by `(namespace, key)`.
    rows: RefCell<HashMap<(String, String), Vec<u8>>>,
    /// Total stored bytes (key + value) across all namespaces.
    bytes: Cell<usize>,
    /// When set, `set` on a new key fails once the store holds this
    /// many rows.
    max_entries: Cell<Option<usize>>,
    /// When set, `set` fails once stored bytes would exceed this.
    max_bytes: Cell<Option<usize>>,
}

impl MockLocalStore {
    /// A view over the same rows under `namespace`; same-namespace views
    /// alias, different namespaces isolate identical keys.
    ///
    /// # Panics
    ///
    /// On an empty namespace.
    pub fn namespaced(&self, namespace: impl Into<String>) -> MockLocalStore {
        let namespace = namespace.into();
        assert!(
            !namespace.is_empty(),
            "MockLocalStore: namespace must not be empty",
        );
        MockLocalStore {
            shared: Rc::clone(&self.shared),
            namespace,
            error_patterns: RefCell::new(Vec::new()),
        }
    }

    /// Number of rows in this view's namespace.
    pub fn len(&self) -> usize {
        self.shared
            .rows
            .borrow()
            .keys()
            .filter(|(ns, _)| *ns == self.namespace)
            .count()
    }

    /// Whether this view's namespace holds no rows.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Direct read of this view's namespace, for assertions.
    pub fn snapshot(&self) -> HashMap<String, Vec<u8>> {
        self.shared
            .rows
            .borrow()
            .iter()
            .filter(|((ns, _), _)| *ns == self.namespace)
            .map(|((_, key), value)| (key.clone(), value.clone()))
            .collect()
    }

    /// Cap row count across all namespaces; `set` on a new key then
    /// fails, overwrites still succeed.
    pub fn set_max_entries(&self, limit: usize) {
        self.shared.max_entries.set(Some(limit));
    }

    /// Cap total stored bytes (key + value, all namespaces); an over-cap
    /// `set` fails, deletes and overwrites release displaced bytes.
    pub fn set_max_bytes(&self, limit: usize) {
        self.shared.max_bytes.set(Some(limit));
    }

    /// Inject a fault for any operation whose key starts with `prefix`;
    /// first registered match fires.
    pub fn fail_on(&self, prefix: impl Into<String>, fault: Fault) {
        self.error_patterns
            .borrow_mut()
            .push((prefix.into(), fault));
    }

    fn check_injected_error(&self, key: &str) -> Result<(), Fault> {
        for (pattern, fault) in self.error_patterns.borrow().iter() {
            if key.starts_with(pattern) {
                return Err(fault.clone());
            }
        }
        Ok(())
    }
}

impl LocalStoreHost for MockLocalStore {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Fault> {
        self.check_injected_error(key)?;
        Ok(self
            .shared
            .rows
            .borrow()
            .get(&(self.namespace.clone(), key.to_string()))
            .cloned())
    }
    fn set(&self, key: &str, value: &[u8]) -> Result<(), Fault> {
        self.check_injected_error(key)?;
        let mut rows = self.shared.rows.borrow_mut();
        let compound = (self.namespace.clone(), key.to_string());
        let existing = rows.get(&compound).map(Vec::len);
        if existing.is_none()
            && let Some(limit) = self.shared.max_entries.get()
            && rows.len() >= limit
        {
            return Err(Fault::Internal(format!(
                "MockLocalStore: max entries ({limit}) reached"
            )));
        }
        // Same-key overwrites release the displaced bytes before the
        // new row is charged.
        let displaced = existing.map_or(0, |len| key.len() + len);
        let total = self.shared.bytes.get() - displaced + key.len() + value.len();
        if let Some(budget) = self.shared.max_bytes.get()
            && total > budget
        {
            return Err(Fault::Internal(format!(
                "MockLocalStore: max bytes ({budget}) reached"
            )));
        }
        rows.insert(compound, value.to_vec());
        self.shared.bytes.set(total);
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<(), Fault> {
        self.check_injected_error(key)?;
        if let Some(value) = self
            .shared
            .rows
            .borrow_mut()
            .remove(&(self.namespace.clone(), key.to_string()))
        {
            self.shared
                .bytes
                .set(self.shared.bytes.get() - key.len() - value.len());
        }
        Ok(())
    }
    fn list_keys(&self, prefix: &str) -> Result<Vec<String>, Fault> {
        self.check_injected_error(prefix)?;
        let mut keys: Vec<String> = self
            .shared
            .rows
            .borrow()
            .keys()
            .filter(|(ns, key)| *ns == self.namespace && key.starts_with(prefix))
            .map(|(_, key)| key.clone())
            .collect();
        keys.sort();
        Ok(keys)
    }
    fn contains(&self, key: &str) -> Result<bool, Fault> {
        self.check_injected_error(key)?;
        Ok(self
            .shared
            .rows
            .borrow()
            .contains_key(&(self.namespace.clone(), key.to_string())))
    }
    fn len(&self, key: &str) -> Result<Option<u64>, Fault> {
        self.check_injected_error(key)?;
        Ok(self
            .shared
            .rows
            .borrow()
            .get(&(self.namespace.clone(), key.to_string()))
            .map(|v| v.len() as u64))
    }
    fn count(&self, prefix: &str) -> Result<u64, Fault> {
        self.check_injected_error(prefix)?;
        Ok(self
            .shared
            .rows
            .borrow()
            .keys()
            .filter(|(ns, key)| *ns == self.namespace && key.starts_with(prefix))
            .count() as u64)
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
    /// The capture buffer active on this thread, if any.
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

/// Events-only subscriber recording each event into the thread's active
/// buffer; spans are inert.
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

/// Splits an event into its `message` field and a name-keyed map of the rest.
#[derive(Default)]
struct FieldVisitor {
    message: String,
    fields: BTreeMap<String, FieldValue>,
}

impl Visit for FieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            // tracing delivers `message` as the `format_args!` result, whose
            // `Debug` renders unquoted; keep the raw text, do not re-quote it.
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

/// Run `f`, returning its value and every `tracing` event it emitted on
/// the calling thread. Capture is thread-scoped; events emitted outside
/// any `capture_tracing` call are dropped.
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
    use nexum_sdk::prelude::U256;

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
        let ChainError::Fault(Fault::Unsupported(msg)) = err else {
            panic!("expected Unsupported fault, got {err:?}");
        };
        assert!(msg.contains("MockChain"));
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
    fn local_store_metadata_queries() {
        let store = MockLocalStore::default();
        store.set("watch:a", b"abc").unwrap();
        store.set("watch:b", b"").unwrap();
        store.set("posted:1", b"x").unwrap();

        assert!(store.contains("watch:a").unwrap());
        assert!(!store.contains("missing").unwrap());
        assert_eq!(LocalStoreHost::len(&store, "watch:a").unwrap(), Some(3));
        assert_eq!(LocalStoreHost::len(&store, "watch:b").unwrap(), Some(0));
        assert_eq!(LocalStoreHost::len(&store, "missing").unwrap(), None);
        assert_eq!(store.count("watch:").unwrap(), 2);
        assert_eq!(store.count("").unwrap(), 3);

        // Metadata queries stay namespace-scoped.
        let other = store.namespaced("other");
        assert_eq!(other.count("").unwrap(), 0);
        assert!(!other.contains("watch:a").unwrap());

        // And respect fault injection.
        store.fail_on("bad:", Fault::Internal("injected".into()));
        assert!(store.contains("bad:k").is_err());
        assert!(LocalStoreHost::len(&store, "bad:k").is_err());
        assert!(store.count("bad:").is_err());
    }

    #[test]
    fn local_store_error_injection() {
        let store = MockLocalStore::default();
        store.fail_on("bad:", Fault::Internal("injected".into()));
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
        assert!(matches!(err, Fault::Internal(ref m) if m.contains("max entries")));
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn local_store_namespaces_isolate_identical_keys() {
        let store = MockLocalStore::default();
        let other = store.namespaced("other-module");
        store.set("watch:a", b"mine").unwrap();
        other.set("watch:a", b"theirs").unwrap();

        assert_eq!(store.get("watch:a").unwrap().as_deref(), Some(&b"mine"[..]));
        assert_eq!(
            other.get("watch:a").unwrap().as_deref(),
            Some(&b"theirs"[..]),
        );

        // Scans, counts, and snapshots stay view-scoped.
        assert_eq!(store.len(), 1);
        assert_eq!(other.len(), 1);
        assert_eq!(store.list_keys("").unwrap(), vec!["watch:a"]);
        assert_eq!(store.snapshot().get("watch:a").unwrap(), b"mine");

        // Deletes never reach across the namespace boundary.
        other.delete("watch:a").unwrap();
        assert!(other.is_empty());
        assert_eq!(store.get("watch:a").unwrap().as_deref(), Some(&b"mine"[..]));
    }

    #[test]
    fn local_store_same_namespace_views_alias_the_same_rows() {
        let store = MockLocalStore::default();
        let one = store.namespaced("mod");
        let two = store.namespaced("mod");
        one.set("k", b"v").unwrap();
        assert_eq!(two.get("k").unwrap().as_deref(), Some(&b"v"[..]));
    }

    #[test]
    #[should_panic(expected = "namespace must not be empty")]
    fn local_store_empty_namespace_panics() {
        let _ = MockLocalStore::default().namespaced("");
    }

    #[test]
    fn local_store_entry_limit_spans_namespaces() {
        let store = MockLocalStore::default();
        store.set_max_entries(2);
        let other = store.namespaced("other-module");
        store.set("a", b"1").unwrap();
        other.set("b", b"2").unwrap();
        // The store is one shared file: a sibling namespace's rows
        // consume the same headroom.
        let err = store.set("c", b"3").unwrap_err();
        assert!(matches!(err, Fault::Internal(ref m) if m.contains("max entries")));
    }

    #[test]
    fn local_store_byte_budget_enforced_and_released() {
        let store = MockLocalStore::default();
        store.set_max_bytes(8);
        store.set("abcd", b"1234").unwrap(); // 4 + 4 = 8, exactly at budget
        let err = store.set("x", b"y").unwrap_err();
        assert!(matches!(err, Fault::Internal(ref m) if m.contains("max bytes")));

        // A same-key overwrite releases the displaced value first.
        store.set("abcd", b"12").unwrap();
        store.set("x", b"y").unwrap();

        // Deleting releases the whole row's bytes.
        store.delete("abcd").unwrap();
        store.set("ab", b"12").unwrap();
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn identity_roster_and_programmed_outcome() {
        let identity = MockIdentity::default();
        let account = Address::from([0xAA; 20]);
        assert!(identity.accounts().unwrap().is_empty());
        identity.add_account(account);
        assert_eq!(identity.accounts().unwrap(), vec![account]);

        // No outcome programmed: signing is unsupported, the stub posture.
        let err = identity.sign(account, b"hello").unwrap_err();
        assert!(matches!(err, Fault::Unsupported(ref m) if m.contains("MockIdentity")));

        let signature = Signature::new(U256::from(1), U256::from(2), false);
        identity.respond(Ok(signature));
        assert_eq!(identity.sign(account, b"hello").unwrap(), signature);
        assert_eq!(identity.sign_typed_data(account, "{}").unwrap(), signature);

        assert_eq!(identity.call_count(), 3);
        assert_eq!(
            identity.last_call().unwrap(),
            SignCall {
                account,
                payload: SignPayload::TypedData("{}".to_owned()),
            },
        );
    }

    #[test]
    fn identity_denies_off_roster_accounts() {
        let identity = MockIdentity::default();
        identity.respond(Ok(Signature::new(U256::from(1), U256::from(2), true)));
        let err = identity.sign(Address::from([0xBB; 20]), b"x").unwrap_err();
        assert!(matches!(err, Fault::Denied(_)));
        // The refused call is still recorded.
        assert_eq!(identity.call_count(), 1);
    }

    #[test]
    fn messaging_records_publishes_and_answers_from_seeds() {
        let messaging = MockMessaging::default();
        messaging.seed_payload("/acme/1/orders/proto", b"one".to_vec(), 10);
        messaging.seed_payload("/acme/1/orders/proto", b"two".to_vec(), 20);
        messaging.seed_payload("/acme/1/other/proto", b"noise".to_vec(), 15);

        messaging.publish("/acme/1/orders/proto", b"out").unwrap();
        assert_eq!(messaging.publish_count(), 1);
        assert_eq!(
            messaging.last_published().unwrap(),
            PublishRecord {
                content_topic: "/acme/1/orders/proto".to_owned(),
                payload: b"out".to_vec(),
            },
        );

        // Publishes never leak into query results.
        let all = messaging
            .query("/acme/1/orders/proto", None, None, None)
            .unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].payload, b"one");
        assert_eq!(all[1].payload, b"two");
    }

    #[test]
    fn messaging_query_applies_bounds_and_limit() {
        let messaging = MockMessaging::default();
        for (payload, ts) in [(b"a", 10u64), (b"b", 20), (b"c", 30), (b"d", 40)] {
            messaging.seed_payload("/t", payload.to_vec(), ts);
        }

        let window = messaging.query("/t", Some(20), Some(30), None).unwrap();
        assert_eq!(window.len(), 2);
        assert_eq!(window[0].payload, b"b");

        // A limit keeps the newest matches: the tail of the window.
        let limited = messaging.query("/t", None, None, Some(2)).unwrap();
        assert_eq!(limited.len(), 2);
        assert_eq!(limited[0].payload, b"c");
        assert_eq!(limited[1].payload, b"d");
    }

    #[test]
    fn messaging_scope_denies_off_grant_topics() {
        let messaging = MockMessaging::default();
        messaging.scope_topics(["/acme/1/orders/proto"]);

        messaging.publish("/acme/1/orders/proto", b"ok").unwrap();
        let err = messaging.publish("/other", b"no").unwrap_err();
        assert!(matches!(err, Fault::Denied(_)));
        let err = messaging.query("/other", None, None, None).unwrap_err();
        assert!(matches!(err, Fault::Denied(_)));
        // The refused publish was never recorded.
        assert_eq!(messaging.publish_count(), 1);
    }

    #[test]
    fn messaging_scope_matches_the_host_grant() {
        // A prefix grant admits the family beneath it, bounded at `/`.
        let messaging = MockMessaging::default();
        messaging.scope_topics(["/nexum/1/"]);
        messaging
            .publish("/nexum/1/acme-orders/proto", b"x")
            .unwrap();
        messaging.publish("/nexum/1/twap/proto", b"x").unwrap();
        let err = messaging.publish("/nexum/2/acme/proto", b"x").unwrap_err();
        assert!(matches!(err, Fault::Denied(_)));

        // No trailing slash still bounds on the separator: a grant never
        // leaks into a longer sibling segment.
        let messaging = MockMessaging::default();
        messaging.scope_topics(["/nexum/1/acme"]);
        messaging.publish("/nexum/1/acme", b"x").unwrap();
        messaging.publish("/nexum/1/acme/orders", b"x").unwrap();
        let err = messaging
            .publish("/nexum/1/acme-orders/proto", b"x")
            .unwrap_err();
        assert!(matches!(err, Fault::Denied(_)));

        // An empty grant is unscoped, the host's module default.
        let messaging = MockMessaging::default();
        messaging.scope_topics(Vec::<String>::new());
        messaging.publish("/anywhere/at/all", b"x").unwrap();
    }

    #[test]
    fn messaging_fault_injection_fires_by_prefix() {
        let messaging = MockMessaging::default();
        messaging.fail_on("/flaky", Fault::Timeout);
        assert!(matches!(
            messaging.publish("/flaky/topic", b"x").unwrap_err(),
            Fault::Timeout,
        ));
        messaging.publish("/steady", b"x").unwrap();
    }

    #[test]
    fn remote_store_round_trips_content_addressed_blobs() {
        let store = MockRemoteStore::default();
        let reference = store.upload(b"chunk").unwrap();
        assert_eq!(reference, keccak256(b"chunk"));
        assert_eq!(store.download(reference).unwrap(), b"chunk");
        assert_eq!(store.blob_count(), 1);

        let missing = store.download(B256::from([0xCC; 32])).unwrap_err();
        assert!(matches!(missing, Fault::Unavailable(ref m) if m.contains("MockRemoteStore")));
    }

    #[test]
    fn remote_store_feeds_are_owner_scoped() {
        let store = MockRemoteStore::default();
        let owner = Address::from([0xAA; 20]);
        let topic = B256::from([0x11; 32]);

        // Writes land under the mock's own owner and stay downloadable.
        store.set_owner(owner);
        let reference = store.write_feed(topic, b"v1").unwrap();
        assert_eq!(store.download(reference).unwrap(), b"v1");
        assert_eq!(
            store.read_feed(owner, topic).unwrap().as_deref(),
            Some(&b"v1"[..])
        );

        // Another owner's feed is a distinct slot.
        let other = Address::from([0xBB; 20]);
        assert_eq!(store.read_feed(other, topic).unwrap(), None);
        store.seed_feed(other, topic, b"theirs");
        assert_eq!(
            store.read_feed(other, topic).unwrap().as_deref(),
            Some(&b"theirs"[..]),
        );
    }

    #[test]
    fn remote_store_fault_injection_covers_every_operation() {
        let store = MockRemoteStore::default();
        store.fail_with(Fault::Timeout);
        assert!(matches!(store.upload(b"x").unwrap_err(), Fault::Timeout));
        assert!(matches!(
            store.download(B256::ZERO).unwrap_err(),
            Fault::Timeout,
        ));
        assert!(matches!(
            store.read_feed(Address::ZERO, B256::ZERO).unwrap_err(),
            Fault::Timeout,
        ));
        assert!(matches!(
            store.write_feed(B256::ZERO, b"x").unwrap_err(),
            Fault::Timeout,
        ));
    }

    #[test]
    fn mock_host_dispatches_through_supertrait() {
        let host = MockHost::new();
        host.chain
            .respond_to("eth_blockNumber", "[]", Ok("\"0x1\"".into()));
        host.messaging.seed_payload("/t", b"m".to_vec(), 1);

        // Through the `Host` supertrait: all six seams on one value.
        let _: &dyn nexum_sdk::host::Host = &host;
        host.set("key", b"val").unwrap();
        assert_eq!(host.get("key").unwrap().as_deref(), Some(&b"val"[..]));
        assert_eq!(host.request(1, "eth_blockNumber", "[]").unwrap(), "\"0x1\"");
        assert!(host.accounts().unwrap().is_empty());
        assert_eq!(host.query("/t", None, None, None).unwrap().len(), 1);
        let reference = host.upload(b"blob").unwrap();
        assert_eq!(host.download(reference).unwrap(), b"blob");
        host.log(Level::INFO, "happy path");

        assert_eq!(host.chain.call_count(), 1);
        assert_eq!(host.logging.lines().len(), 1);
        assert_eq!(host.store.len(), 1);
        assert_eq!(host.remote_store.blob_count(), 1);
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
