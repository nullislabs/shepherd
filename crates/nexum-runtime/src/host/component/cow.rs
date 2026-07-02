//! CoW orderbook seam: REST passthrough plus typed order submission,
//! mirroring the inherent `OrderBookPool` API.

use std::future::Future;

use alloy_chains::Chain;
use cowprotocol::OrderUid;

use crate::host::cow_orderbook::{CowApiError, OrderBookPool};

/// Async CoW orderbook backend. `get` (concrete client lookup) is
/// deliberately not part of the seam; it leaks `OrderBookApi`.
pub trait CowApi {
    /// REST passthrough against the chain's orderbook base URL.
    fn request(
        &self,
        chain: Chain,
        method: http::Method,
        path: &str,
        body: Option<&str>,
    ) -> impl Future<Output = Result<String, CowApiError>> + Send;

    /// Typed submission of a JSON-encoded `OrderCreation`.
    fn submit_order_json(
        &self,
        chain: Chain,
        body: &[u8],
    ) -> impl Future<Output = Result<OrderUid, CowApiError>> + Send;
}

impl CowApi for OrderBookPool {
    fn request(
        &self,
        chain: Chain,
        method: http::Method,
        path: &str,
        body: Option<&str>,
    ) -> impl Future<Output = Result<String, CowApiError>> + Send {
        OrderBookPool::request(self, chain, method, path, body)
    }

    fn submit_order_json(
        &self,
        chain: Chain,
        body: &[u8],
    ) -> impl Future<Output = Result<OrderUid, CowApiError>> + Send {
        OrderBookPool::submit_order_json(self, chain, body)
    }
}
