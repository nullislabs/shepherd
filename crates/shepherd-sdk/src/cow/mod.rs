//! CoW Protocol bridging.
//!
//! Type conversions and ABI decoding helpers that translate between
//! the on-chain shape (`GPv2OrderData`, `IConditionalOrder` reverts,
//! orderbook JSON) and the typed Rust surface (`OrderData`,
//! `PollOutcome`, `RetryAction`), plus [`materialise`] - the
//! poll/submit composition over the chassis stores.
//!
//! The codec submodules stay purely host-neutral: helpers take
//! primitive arguments (`&[u8]`, `Option<&str>`, slices) so they can
//! be unit-tested without wit-bindgen scaffolding and re-used
//! unchanged by TWAP, EthFlow, and future strategy modules. The
//! materialiser is generic over the host traits alone.

pub mod composable;
pub mod error;
pub mod materialise;
pub mod order;

pub use composable::{IConditionalOrder, PollOutcome, classify_poll_error, decode_revert};
pub use error::{
    CowApiError, HttpFailure, OrderRejection, RetryAction, classify_api_error,
    classify_submit_error, is_already_submitted,
};
pub use materialise::materialise;
pub use order::{gpv2_to_order_data, order_uid_hex};

use nexum_sdk::host::Host;

/// `shepherd:cow/cow-api` - orderbook submission path. The CoW-domain
/// sibling of the core host traits in [`nexum_sdk::host`].
pub trait CowApiHost {
    /// Submit an `OrderCreation` JSON body. The host returns the
    /// canonical order UID on success. A rejection surfaces as a typed
    /// [`CowApiError::Rejected`]; classify it with
    /// [`classify_api_error`].
    fn submit_order(&self, chain_id: u64, body: &[u8]) -> Result<String, CowApiError>;

    /// REST-style request against the CoW Protocol orderbook for the
    /// given chain. The host routes to the correct base URL
    /// (`https://api.cow.fi/<chain>/api/v1/...`). Returns the raw
    /// response body. Strategies that need a typed surface should
    /// wrap this in an SDK helper.
    ///
    /// `method` is `"GET" | "POST" | "PUT" | "DELETE"`.
    /// `path` is the absolute orderbook path beginning with `/api/v1`.
    /// `body` is an optional JSON request body (only used for POST/PUT).
    ///
    /// A non-2xx reply surfaces as [`CowApiError::Http`]; callers
    /// distinguish "orderbook does not know this resource" from a
    /// genuine upstream failure by matching `http.status == 404`.
    fn cow_api_request(
        &self,
        chain_id: u64,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<String, CowApiError>;
}

/// Host bound for strategies that reach the CoW Protocol orderbook.
pub trait CowHost: Host + CowApiHost {}
impl<T: Host + CowApiHost> CowHost for T {}
