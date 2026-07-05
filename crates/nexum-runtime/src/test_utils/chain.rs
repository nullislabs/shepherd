//! In-process [`ChainProvider`] fake: programmable JSON-RPC responses,
//! recorded calls, and block / chain-log streams driven from the test.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use alloy_chains::Chain;
use alloy_rpc_types_eth::{Filter, Header, Log};
use futures::channel::mpsc::{self, UnboundedSender};

use crate::host::component::{ChainMethod, ChainProvider};
use crate::host::provider_pool::{BlockStream, ChainLogStream, ProviderError};

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

struct Inner {
    // (method wire name, exact params) -> response body.
    exact: HashMap<(&'static str, String), String>,
    // method wire name -> response body for any params.
    wildcard: HashMap<&'static str, String>,
    recorded: Vec<RecordedRequest>,
    // Taken by the first `subscribe_blocks`; a later call parks on a
    // pending stream so a reconnect loop does not busy-spin.
    block_rx: Option<mpsc::UnboundedReceiver<BlockItem>>,
    log_rx: Option<mpsc::UnboundedReceiver<LogItem>>,
}

/// Mock chain backend. Program `request` responses with [`on_method`] /
/// [`on_request`], drive subscriptions with [`push_block`] /
/// [`push_chain_log`], script transport failures with [`push_block_err`] /
/// [`push_chain_log_err`], end a stream with [`close_block_stream`] /
/// [`close_chain_log_stream`], and read back dispatched calls with
/// [`recorded_requests`]. Cheap `Arc` clone shares one backing state, so a
/// test keeps a clone to program and assert while another clone lives inside
/// the runtime assembly.
///
/// [`on_method`]: MockChainProvider::on_method
/// [`on_request`]: MockChainProvider::on_request
/// [`push_block`]: MockChainProvider::push_block
/// [`push_chain_log`]: MockChainProvider::push_chain_log
/// [`push_block_err`]: MockChainProvider::push_block_err
/// [`push_chain_log_err`]: MockChainProvider::push_chain_log_err
/// [`close_block_stream`]: MockChainProvider::close_block_stream
/// [`close_chain_log_stream`]: MockChainProvider::close_chain_log_stream
/// [`recorded_requests`]: MockChainProvider::recorded_requests
#[derive(Clone)]
pub struct MockChainProvider {
    inner: Arc<Mutex<Inner>>,
    block_tx: UnboundedSender<BlockItem>,
    log_tx: UnboundedSender<LogItem>,
}

impl Default for MockChainProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl MockChainProvider {
    /// Fresh mock with no programmed responses and empty streams.
    pub fn new() -> Self {
        let (block_tx, block_rx) = mpsc::unbounded();
        let (log_tx, log_rx) = mpsc::unbounded();
        Self {
            inner: Arc::new(Mutex::new(Inner {
                exact: HashMap::new(),
                wildcard: HashMap::new(),
                recorded: Vec::new(),
                block_rx: Some(block_rx),
                log_rx: Some(log_rx),
            })),
            block_tx,
            log_tx,
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

    /// Deliver a block header to the open block subscription.
    pub fn push_block(&self, header: Header) {
        let _ = self.block_tx.unbounded_send(Ok(header));
    }

    /// Deliver a log to the open chain-log subscription.
    pub fn push_chain_log(&self, log: Log) {
        let _ = self.log_tx.unbounded_send(Ok(log));
    }

    /// Deliver an error item to the open block subscription, so a
    /// reconnect-and-backoff loop on the [`BlockStream`] contract can be
    /// exercised against the fake.
    pub fn push_block_err(&self, err: ProviderError) {
        let _ = self.block_tx.unbounded_send(Err(err));
    }

    /// Deliver an error item to the open chain-log subscription.
    pub fn push_chain_log_err(&self, err: ProviderError) {
        let _ = self.log_tx.unbounded_send(Err(err));
    }

    /// End the block subscription: buffered items drain, then the stream
    /// terminates (yields `None`), modelling a dropped upstream connection.
    pub fn close_block_stream(&self) {
        self.block_tx.clone().close_channel();
    }

    /// End the chain-log subscription the same way as
    /// [`close_block_stream`](Self::close_block_stream).
    pub fn close_chain_log_stream(&self) {
        self.log_tx.clone().close_channel();
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
            let stream: BlockStream = match inner.lock().expect("mock chain mutex").block_rx.take()
            {
                Some(rx) => Box::pin(rx),
                None => Box::pin(futures::stream::pending::<BlockItem>()),
            };
            Ok(stream)
        }
    }

    fn subscribe_chain_logs(
        &self,
        _chain: Chain,
        _filter: Filter,
    ) -> impl Future<Output = Result<ChainLogStream, ProviderError>> + Send {
        let inner = self.inner.clone();
        async move {
            let stream: ChainLogStream = match inner.lock().expect("mock chain mutex").log_rx.take()
            {
                Some(rx) => Box::pin(rx),
                None => Box::pin(futures::stream::pending::<LogItem>()),
            };
            Ok(stream)
        }
    }

    fn request(
        &self,
        chain: Chain,
        method: ChainMethod,
        params_json: String,
    ) -> impl Future<Output = Result<String, ProviderError>> + Send {
        let inner = self.inner.clone();
        async move {
            let mut guard = inner.lock().expect("mock chain mutex");
            guard.recorded.push(RecordedRequest {
                chain,
                method,
                params_json: params_json.clone(),
            });
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
