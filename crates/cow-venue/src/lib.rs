//! # cow-venue
//!
//! The CoW venue, staged as feature slices. `body` (default) carries the
//! venue-neutral order body types and their borsh
//! [`IntentBody`](videre_sdk::IntentBody) codec. `client` adds the typed
//! `CowClient`, the deterministic `intent_id` journal key, and the
//! table-driven retry `classification` generated at build time from
//! `data/classification.toml`. `assembly` carries the chain-edge order
//! projections; `venue` is the native venue itself: `CowAdapter` behind
//! `videre_host::VenueInvoker`, plus the [`register`] helper a composition
//! root calls. A keeper module never links the `venue` slice.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![warn(missing_docs)]

#[cfg(feature = "body")]
pub mod body;

#[cfg(feature = "body")]
pub mod order;

#[cfg(feature = "venue")]
pub mod adapter;

#[cfg(feature = "venue")]
pub mod transport;

#[cfg(feature = "assembly")]
pub mod assembly;

#[cfg(feature = "client")]
pub mod classification;

// The shared TOML parse and table invariants. `build.rs` includes this
// file to generate the classification table; the crate links it only in
// tests, to re-parse the shipped data and check parity.
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

#[cfg(feature = "venue")]
pub use adapter::{
    BODY_VERSIONS, CowAdapter, CowConfig, DEFAULT_TIMEOUT, body_versions, register, venue_id,
};
/// The chain vocabulary [`CowConfig`] is built over, so a composition root
/// names a chain without linking cowprotocol itself.
#[cfg(feature = "venue")]
pub use cowprotocol::Chain;
#[cfg(feature = "venue")]
pub use transport::{OrderbookHttp, Transport};

#[cfg(feature = "client")]
pub use classification::{ClassificationTable, classify, classify_denied, is_already_submitted};
#[cfg(feature = "client")]
pub use client::{CowClient, CowVenue, VENUE_ID, intent_id};
