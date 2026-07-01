//! Bulk-imports the protocol primitives every Shepherd module uses on
//! every other line. `use shepherd_sdk::prelude::*` is a one-liner that
//! covers alloy address / hash / numeric types plus cowprotocol's
//! order, signing, and orderbook-error surface.
//!
//! The wit-bindgen-generated types (`Guest`, `HostError`, `Event`, …)
//! are **not** re-exported here because they live in each module's own
//! crate (one `wit_bindgen::generate!` call per cdylib). The prelude
//! covers only the host-neutral protocol layer that the SDK helpers
//! consume by value.

pub use alloy_primitives::{Address, B256, Bytes, U256, address, b256, hex, keccak256};

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
/// guest-side helpers read this back out of host-error JSON
/// to drive the `RetryAction` dispatch.
pub use cowprotocol::error::ApiError;
