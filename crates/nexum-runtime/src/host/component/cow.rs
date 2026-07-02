//! CoW orderbook seam: REST passthrough plus typed order submission,
//! mirroring the inherent `OrderBookPool` API.

use std::future::Future;

use cowprotocol::OrderUid;

use crate::host::cow_orderbook::{CowApiError, OrderBookPool};

/// Async CoW orderbook backend. `get` (concrete client lookup) is
/// deliberately not part of the seam; it leaks `OrderBookApi`.
pub trait CowApi {
    /// REST passthrough against the chain's orderbook base URL.
    fn request(
        &self,
        chain_id: u64,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> impl Future<Output = Result<String, CowApiError>> + Send;

    /// Typed submission of a JSON-encoded `OrderCreation`.
    fn submit_order_json(
        &self,
        chain_id: u64,
        body: &[u8],
    ) -> impl Future<Output = Result<OrderUid, CowApiError>> + Send;
}

impl CowApi for OrderBookPool {
    fn request(
        &self,
        chain_id: u64,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> impl Future<Output = Result<String, CowApiError>> + Send {
        OrderBookPool::request(self, chain_id, method, path, body)
    }

    fn submit_order_json(
        &self,
        chain_id: u64,
        body: &[u8],
    ) -> impl Future<Output = Result<OrderUid, CowApiError>> + Send {
        OrderBookPool::submit_order_json(self, chain_id, body)
    }
}
