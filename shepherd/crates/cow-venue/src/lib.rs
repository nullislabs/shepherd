//! # cow-venue
//!
//! The CoW venue, staged as feature slices. `body` (default) carries the
//! venue-neutral order body types and their borsh
//! [`IntentBody`](videre_sdk::IntentBody) codec. `client` adds the typed
//! `CowClient`, the deterministic `intent_id` journal key, and the
//! table-driven retry `classification` generated at build time from
//! `data/classification.toml`. `assembly` carries the chain-edge order
//! projections; `adapter` is the `venue-adapter` component
//! (`CowAdapter`) built for wasm32-wasip2, never linked by a keeper
//! module.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![warn(missing_docs)]
// wit_bindgen::generate! expands to host-import shims whose arity can
// exceed clippy's too-many-arguments threshold.
#![cfg_attr(feature = "adapter", allow(clippy::too_many_arguments))]

#[cfg(feature = "body")]
pub mod body;

#[cfg(feature = "body")]
pub mod order;

#[cfg(feature = "adapter")]
pub mod adapter;

#[cfg(feature = "assembly")]
pub mod assembly;

#[cfg(feature = "client")]
pub mod classification;

// The shared TOML parse and table invariants. `build.rs` includes this
// file to generate the classification table; the crate links it only in
// tests, to re-parse the shipped data and check parity. It never reaches
// a guest.
#[cfg(all(feature = "client", test))]
mod classification_data;

#[cfg(feature = "client")]
pub mod client;

#[cfg(feature = "body")]
pub use body::{CowIntent, CowIntentBody};
#[cfg(feature = "body")]
pub use order::{
    BuyToken, BuyTokenDestination, OrderBody, OrderBuilder, OrderKind, OrderUid, SellToken,
    SellTokenSource, SignedOrder,
};

#[cfg(feature = "adapter")]
pub use adapter::CowAdapter;

#[cfg(feature = "client")]
pub use classification::{ClassificationTable, classify, classify_denied, is_already_submitted};
#[cfg(feature = "client")]
pub use client::{CowClient, CowVenue, intent_id};
