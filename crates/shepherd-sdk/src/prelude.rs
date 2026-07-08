//! Bulk-imports the CoW Protocol primitives every Shepherd module uses
//! on every other line. `use shepherd_sdk::prelude::*` covers
//! cowprotocol's order, signing, and orderbook-error surface; the
//! alloy address / hash / numeric types come from
//! `nexum_sdk::prelude::*` alongside it.
//!
//! The wit-bindgen-generated types (`Guest`, `Fault`, `Event`, …)
//! are **not** re-exported here because they live in each module's own
//! crate (one `wit_bindgen::generate!` call per cdylib). The prelude
//! covers only the host-neutral protocol layer that the SDK helpers
//! consume by value.

pub use cowprotocol::{
    BuyTokenDestination,
    // App-data + chain + domain identity.
    Chain,
    DomainSeparator,
    EMPTY_APP_DATA_HASH,
    EMPTY_APP_DATA_JSON,
    // Settlement primitives carried in event payloads and order bodies.
    GPv2OrderData,
    // Orderbook submission body + the parts every assembly path touches.
    OrderCreation,
    OrderData,
    OrderKind,
    // Order identity.
    OrderUid,
    SellTokenSource,
    // Signing.
    Signature,
    SigningScheme,
};

/// Re-exported `ApiError` typed error surface from the orderbook;
/// guest-side helpers read this back out of the orderbook rejection
/// envelope to drive the `RetryAction` dispatch.
pub use cowprotocol::error::ApiError;
