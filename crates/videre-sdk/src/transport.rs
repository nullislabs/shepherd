//! Typed wrappers over the adapter world's scoped transport imports:
//! chain RPC, messaging, and outbound wasi:http.
//!
//! Each wrapper adapts this crate's bindgen import shims to the
//! SDK-neutral vocabulary (`nexum_sdk::host`), so adapter logic written
//! against the seams is unit-testable host-free and reuses the
//! `nexum-sdk` chain helpers unchanged. The wrappers only translate;
//! scoping is the host's: chain methods pass through the host's
//! permitted read-only surface, messaging is confined to the adapter's
//! `messaging_topics`, and HTTP to its `http_allow` list, each refusal
//! surfacing as a typed `denied`.

use nexum_sdk::host::{ChainError, ChainHost, Fault, RpcError};

use crate::bindings::nexum::host::{chain, messaging};
use crate::faults::fault_into_sdk;

/// Outbound HTTP for adapters: the SDK's wasi:http surface re-exported.
/// [`fetch`](nexum_sdk::http::Fetch::fetch) speaks the standard `http`
/// crate's request/response types; an off-allowlist request fails as
/// [`FetchError::Denied`](nexum_sdk::http::FetchError::Denied), which
/// converts into [`VenueError`](crate::VenueError) via `?`.
pub use nexum_sdk::http;

/// The adapter's `nexum:host/chain` import behind the SDK's
/// [`ChainHost`] seam. Unit-struct handle: hold it where strategy logic
/// takes `&impl ChainHost` and slot a mock in host-side tests.
#[derive(Clone, Copy, Debug, Default)]
pub struct HostChain;

impl ChainHost for HostChain {
    fn request(&self, chain_id: u64, method: &str, params: &str) -> Result<String, ChainError> {
        chain::request(chain_id, method, params).map_err(chain_error_into_sdk)
    }
}

impl HostChain {
    /// Execute several JSON-RPC requests against one chain in a single
    /// round trip where the host transport supports it. Entries are
    /// independent: the outer error is the batch failing to execute at
    /// all, the per-entry results carry each call's own outcome, in
    /// request order.
    pub fn request_batch(
        &self,
        chain_id: u64,
        requests: &[RpcRequest],
    ) -> Result<Vec<Result<String, ChainError>>, ChainError> {
        let wire: Vec<chain::RpcRequest> = requests
            .iter()
            .map(|req| chain::RpcRequest {
                method: req.method.clone(),
                params: req.params.clone(),
            })
            .collect();
        let results = chain::request_batch(chain_id, &wire).map_err(chain_error_into_sdk)?;
        Ok(results
            .into_iter()
            .map(|result| match result {
                chain::RpcResult::Ok(value) => Ok(value),
                chain::RpcResult::Err(err) => Err(chain_error_into_sdk(err)),
            })
            .collect())
    }
}

/// One JSON-RPC call inside a [`HostChain::request_batch`], mirrored
/// from `nexum:host/chain.rpc-request`. `method` carries its namespace
/// prefix (`eth_call`); `params` is the JSON-encoded positional array.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RpcRequest {
    /// JSON-RPC method, namespace prefix included.
    pub method: String,
    /// JSON-encoded params array.
    pub params: String,
}

/// Lift the wire chain error into the SDK-neutral [`ChainError`].
/// Exhaustive on both the fault vocabulary and the rpc-error shape.
fn chain_error_into_sdk(err: chain::ChainError) -> ChainError {
    match err {
        chain::ChainError::Fault(fault) => ChainError::Fault(fault_into_sdk(fault)),
        chain::ChainError::Rpc(rpc) => ChainError::Rpc(RpcError {
            code: rpc.code,
            message: rpc.message,
            data: rpc.data.map(Into::into),
        }),
    }
}

/// The messaging seam and its message mirror, canonical in the module
/// SDK; [`HostMessaging`] is this crate's bound impl.
pub use nexum_sdk::host::{Message, MessagingHost};

/// The adapter's `nexum:host/messaging` import behind the
/// [`MessagingHost`] seam.
#[derive(Clone, Copy, Debug, Default)]
pub struct HostMessaging;

impl MessagingHost for HostMessaging {
    fn publish(&self, content_topic: &str, payload: &[u8]) -> Result<(), Fault> {
        messaging::publish(content_topic, payload).map_err(fault_into_sdk)
    }

    fn query(
        &self,
        content_topic: &str,
        start_time: Option<u64>,
        end_time: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Vec<Message>, Fault> {
        let messages =
            messaging::query(content_topic, start_time, end_time, limit).map_err(fault_into_sdk)?;
        Ok(messages.into_iter().map(Message::from).collect())
    }
}

impl From<crate::bindings::nexum::host::types::Message> for Message {
    fn from(message: crate::bindings::nexum::host::types::Message) -> Self {
        Self {
            content_topic: message.content_topic,
            payload: message.payload,
            timestamp: message.timestamp,
            sender: message.sender,
        }
    }
}
