//! # cow-venue
//!
//! The CoW venue, staged as a crate of feature slices: the orderbook
//! and nothing else. The default [`body`] slice carries the
//! venue-neutral order intent body types and the borsh `IntentBody`
//! codec over them; conditional-order keeper machinery lives in its
//! own crate and never here.
//!
//! The body slice is dependency-light on purpose. It links only the
//! venue SDK (for the [`IntentBody`](videre_sdk::IntentBody) derive)
//! and borsh, so a venue adapter component or a strategy module can carry
//! the body types and codec without dragging in the host-side CoW
//! machinery. The crate is `#![no_std]` (tests and the `adapter` slice
//! aside): the derive's generated code reaches `alloc` through the
//! venue SDK re-export, never `::std`.
//!
//! With `--no-default-features` the slice drops out entirely and the
//! crate compiles empty, so a consumer can depend on a single slice
//! without pulling the codec transitively.
//!
//! The `client` slice layers on top: a typed [`CowClient`] bound to the
//! CoW venue, the deterministic [`intent_id`] journal key, and the
//! table-driven retry [`classification`] generated at
//! build time from the shipped `data/classification.toml` (the TOML
//! parser stays a build-time dependency, off the guest). It links the
//! strategy keeper (for the retry action type) and is off by default,
//! so an adapter or a module that wants only the body types stays
//! dependency-light.
//!
//! The `assembly` slice carries the chain-edge order projections and
//! orderbook submission bodies; the `adapter` slice on top of it is
//! the venue-adapter component itself (`CowAdapter` under
//! `#[videre_sdk::venue]`), built as the cdylib for wasm32-wasip2 and
//! never linked by a keeper module (linking it would export the
//! adapter face).

#![cfg_attr(not(any(test, feature = "adapter")), no_std)]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![warn(missing_docs)]
// wit_bindgen::generate! expands to host-import shims whose arity can
// exceed clippy's too-many-arguments threshold.
#![cfg_attr(feature = "adapter", allow(clippy::too_many_arguments))]

#[cfg(feature = "body")]
extern crate alloc;

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
