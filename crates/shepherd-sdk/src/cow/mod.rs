//! CoW Protocol bridging.
//!
//! ABI decoding helpers, the orderbook error surface, and [`run()`] -
//! the poll/submit composition over the keeper stores. The chain-edge
//! order projections live in the `cow-venue` `assembly` slice (the
//! venue adapter owns them) and are re-exported here.
//!
//! The poll seam is the structured
//! [`Verdict`](composable_cow::Verdict), carried by the
//! `composable-cow` keeper crate together with the quarantined revert
//! decoding; only orderbook concerns live here.
//!
//! The codec submodules stay purely host-neutral: helpers take
//! primitive arguments (`&[u8]`, `Option<&str>`, slices) so they can
//! be unit-tested without wit-bindgen scaffolding and re-used
//! unchanged by TWAP, EthFlow, and future strategy modules. The
//! keeper run is generic over the host traits alone.

pub mod error;
pub mod events;
pub mod run;

/// Chain-edge order assembly, re-exported from the `cow-venue`
/// `assembly` slice the venue adapter owns.
pub use cow_venue::assembly::{gpv2_to_order_data, order_data_to_body, order_uid_hex};
pub use error::{
    CowApiError, HttpFailure, OrderRejection, RetryAction, classify_api_error,
    classify_submit_error, is_already_submitted,
};
pub use run::run;

/// The venue-neutral intent body types and their borsh `IntentBody`
/// codec, re-exported from the `cow-venue` default slice. The shim keeps
/// this path stable while the module ports move off the legacy surface.
pub use cow_venue::{
    BuyToken, BuyTokenDestination, CowIntent, CowIntentBody, OrderBody, OrderBuilder, OrderKind,
    OrderUid, SellToken, SellTokenSource, SignedOrder, intent_id,
};

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
