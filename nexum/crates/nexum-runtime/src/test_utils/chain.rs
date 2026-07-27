//! In-process [`ChainProvider`] fake: programmable JSON-RPC responses,
//! recorded calls, and block / chain-log streams driven from the test.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy_chains::Chain;
use alloy_rpc_types_eth::{Filter, Header, Log};
use futures::StreamExt as _;
use futures::channel::mpsc::{self, UnboundedSender};

use crate::host::component::{ChainMethod, ChainProvider};
use crate::host::provider_pool::{BlockStream, CanonicalLogStream, ProviderError};

/// One dispatched [`ChainProvider::request`], captured in call order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedRequest {
    /// Target chain.
    pub chain: Chain,
    /// Requested read-surface method.
    pub method: ChainMethod,
    /// Raw JSON params array the caller passed.
    pub params_json: String,
}

type BlockItem = Result<Header, ProviderError>;
type LogItem = Result<Log, ProviderError>;

/// One subscription kind's channel pair; the receiver is taken by the first
/// subscribe. A second subscribe before any [`close`](Self::close) parks on
/// a pending stream. [`close`](Self::close) ends the open stream and re-arms
/// the slot, so the next subscribe (the reconnect path) resumes delivery.
struct StreamSlot<T> {
    tx: UnboundedSender<T>,
    rx: Option<mpsc::UnboundedReceiver<T>>,
}

impl<T> StreamSlot<T> {
    fn new() -> Self {
        let (tx, rx) = mpsc::unbounded();
        Self { tx, rx: Some(rx) }
    }

    fn send(&self, item: T) {
        let _ = self.tx.unbounded_send(item);
    }

    fn close(&mut self) {
        // Close is the reconnect drop: the already-taken receiver drains its
        // buffered items then ends. If the receiver was never taken, the
        // reassignment below drops it and those items are lost, so that is a
        // misuse worth catching in debug builds.
        debug_assert!(
            self.rx.is_none(),
            "close on a slot whose receiver was never taken; buffered items are lost",
        );
        self.tx.close_channel();
        *self = Self::new();
    }

    fn take(&mut self) -> Option<mpsc::UnboundedReceiver<T>> {
        self.rx.take()
    }
}

struct Inner {
    // (method wire name, exact params) -> response body.
    exact: HashMap<(&'static str, String), String>,
    // method wire name -> response body for any params.
    wildcard: HashMap<&'static str, String>,
    recorded: Vec<RecordedRequest>,
    blocks: StreamSlot<BlockItem>,
    logs: StreamSlot<LogItem>,
    // Head returned by `block_number` (the poller's start block).
    head_block: u64,
    // One-shot delay applied to the next `request` call, consumed when
    // that call begins. Models a provider that parks a request (a hung
    // node, a server that never answers). Consumed before the sleep, so a
    // caller that drops the request future mid-park still clears it: the
    // following request answers promptly.
    next_request_delay: Option<Duration>,
}

/// Mock chain backend. Program `request` responses with [`on_method`] /
/// [`on_request`], drive subscriptions with [`push_block`] /
/// [`push_chain_log`] (and the `_err` / `close_*` variants), and read
/// dispatched calls with [`recorded_requests`]. Cheap `Arc` clone shares one
/// backing state, so a test keeps a clone to program and assert.
///
/// [`on_method`]: MockChainProvider::on_method
/// [`on_request`]: MockChainProvider::on_request
/// [`push_block`]: MockChainProvider::push_block
/// [`push_chain_log`]: MockChainProvider::push_chain_log
/// [`recorded_requests`]: MockChainProvider::recorded_requests
#[derive(Clone)]
pub struct MockChainProvider {
    inner: Arc<Mutex<Inner>>,
}

impl Default for MockChainProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl MockChainProvider {
    /// Fresh mock with no programmed responses and empty streams.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                exact: HashMap::new(),
                wildcard: HashMap::new(),
                recorded: Vec::new(),
                blocks: StreamSlot::new(),
                logs: StreamSlot::new(),
                head_block: 0,
                next_request_delay: None,
            })),
        }
    }

    /// Program the response body for `method` with any params.
    pub fn on_method(&self, method: ChainMethod, response: impl Into<String>) -> &Self {
        self.lock()
            .wildcard
            .insert(method.as_str(), response.into());
        self
    }

    /// Program the response body for an exact `(method, params_json)` pair.
    /// Takes precedence over a [`on_method`](Self::on_method) wildcard.
    pub fn on_request(
        &self,
        method: ChainMethod,
        params_json: impl Into<String>,
        response: impl Into<String>,
    ) -> &Self {
        self.lock()
            .exact
            .insert((method.as_str(), params_json.into()), response.into());
        self
    }

    /// Deliver a block header to the open block subscription; items sent with
    /// no open subscription buffer and drain into the next.
    pub fn push_block(&self, header: Header) {
        self.lock().blocks.send(Ok(header));
    }

    /// Deliver a log to the open chain-log subscription.
    pub fn push_chain_log(&self, log: Log) {
        self.lock().logs.send(Ok(log));
    }

    /// Deliver an error item to the open block subscription.
    pub fn push_block_err(&self, err: ProviderError) {
        self.lock().blocks.send(Err(err));
    }

    /// Deliver an error item to the open chain-log subscription.
    pub fn push_chain_log_err(&self, err: ProviderError) {
        self.lock().logs.send(Err(err));
    }

    /// End the block subscription (modelling a dropped connection): buffered
    /// items drain, the stream terminates, and the slot re-arms so a later
    /// `subscribe_blocks` resumes delivery of subsequently pushed items.
    pub fn close_block_stream(&self) {
        self.lock().blocks.close();
    }

    /// End the chain-log subscription the same way as
    /// [`close_block_stream`](Self::close_block_stream).
    pub fn close_chain_log_stream(&self) {
        self.lock().logs.close();
    }

    /// Park the next [`ChainProvider::request`] for `delay`. One-shot,
    /// consumed when the request begins, so a caller that drops the request
    /// future mid-park leaves the following request prompt.
    pub fn delay_next_request(&self, delay: Duration) -> &Self {
        self.lock().next_request_delay = Some(delay);
        self
    }

    /// Every [`ChainProvider::request`] dispatched so far, in call order.
    pub fn recorded_requests(&self) -> Vec<RecordedRequest> {
        self.lock().recorded.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().expect("mock chain mutex poisoned")
    }
}

impl ChainProvider for MockChainProvider {
    fn subscribe_blocks(
        &self,
        _chain: Chain,
    ) -> impl Future<Output = Result<BlockStream, ProviderError>> + Send {
        let inner = self.inner.clone();
        async move {
            let stream: BlockStream = match inner.lock().expect("mock chain mutex").blocks.take() {
                Some(rx) => Box::pin(rx),
                None => Box::pin(futures::stream::pending::<BlockItem>()),
            };
            Ok(stream)
        }
    }

    fn block_number(
        &self,
        _chain: Chain,
    ) -> impl Future<Output = Result<u64, ProviderError>> + Send {
        let inner = self.inner.clone();
        async move { Ok(inner.lock().expect("mock chain mutex").head_block) }
    }

    fn watch_chain_logs(
        &self,
        _chain: Chain,
        _filter: Filter,
        _start_block: u64,
    ) -> Result<CanonicalLogStream, ProviderError> {
        // The programmable `logs` slot yields individual logs; project
        // each into a single-log canonical batch so the poller-shaped
        // stream contract (`Vec<Log>` per block) is satisfied without
        // reworking every test that pushes logs one at a time.
        let stream: CanonicalLogStream =
            match self.inner.lock().expect("mock chain mutex").logs.take() {
                Some(rx) => Box::pin(rx.map(|item| item.map(|log| vec![log]))),
                None => Box::pin(futures::stream::pending::<Result<Vec<Log>, ProviderError>>()),
            };
        Ok(stream)
    }

    fn request(
        &self,
        chain: Chain,
        method: ChainMethod,
        params_json: String,
    ) -> impl Future<Output = Result<String, ProviderError>> + Send {
        let inner = self.inner.clone();
        async move {
            // Record the call and take any one-shot park delay, then drop
            // the guard before awaiting: a std `Mutex` must not be held
            // across an await, and taking the delay here (not after the
            // sleep) is what makes it survive a dropped future.
            let delay = {
                let mut guard = inner.lock().expect("mock chain mutex");
                guard.recorded.push(RecordedRequest {
                    chain,
                    method,
                    params_json: params_json.clone(),
                });
                guard.next_request_delay.take()
            };
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            let guard = inner.lock().expect("mock chain mutex");
            let name = method.as_str();
            if let Some(body) = guard.exact.get(&(name, params_json)) {
                Ok(body.clone())
            } else if let Some(body) = guard.wildcard.get(name) {
                Ok(body.clone())
            } else {
                // No response programmed: mirror the empty pool's shape so a
                // caller sees a normal provider error rather than a panic.
                // Caveat: this conflates "chain present but method not
                // scripted" with "chain absent", so a test must not read
                // UnknownChain here as a chain-presence signal.
                Err(ProviderError::UnknownChain(chain))
            }
        }
    }
}
