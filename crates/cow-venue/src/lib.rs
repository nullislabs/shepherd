//! # cow-venue
//!
//! The CoW venue, staged as a crate of feature slices. Today only the
//! default [`body`] slice exists: the venue-neutral order and composable
//! intent body types and the borsh `IntentBody` codec over them. The
//! typed client and the adapter component are later slices.
//!
//! The body slice is dependency-light on purpose. It links only the
//! venue SDK (for the [`IntentBody`](nexum_venue_sdk::IntentBody) derive)
//! and borsh, so a venue adapter component or a strategy module can carry
//! the body types and codec without dragging in the host-side CoW
//! machinery. The one non-obvious constraint: the derive's generated code
//! names `::std`, so the slice links std and is not a bare-metal
//! `#![no_std]` crate; it is guest-consumable on the runtime's
//! std-bearing wasm target rather than target-free.
//!
//! With `--no-default-features` the slice drops out entirely and the
//! crate compiles empty, so a consumer can depend on a future slice
//! without pulling the codec transitively.
//!
//! The `client` slice layers on top: a typed [`CowClient`] bound to the
//! CoW venue plus the table-driven retry [`classification`] generated at
//! build time from the shipped `data/classification.toml` (the TOML
//! parser stays a build-time dependency, off the guest). It links the
//! strategy chassis (for the retry action type) and is off by default,
//! so an adapter or a module that wants only the body types stays
//! dependency-light.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![warn(missing_docs)]

#[cfg(feature = "body")]
pub mod body;

#[cfg(feature = "body")]
pub mod composable;

#[cfg(feature = "body")]
pub mod order;

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
pub use composable::ComposableBody;
#[cfg(feature = "body")]
pub use order::{BuyTokenDestination, OrderBody, OrderKind, SellTokenSource};

#[cfg(feature = "client")]
pub use classification::{ClassificationTable, classify, is_already_submitted};
#[cfg(feature = "client")]
pub use client::{CowClient, VENUE};
