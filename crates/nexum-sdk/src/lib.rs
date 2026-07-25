//! Guest-side SDK for nexum runtime modules: host-neutral, domain-free
//! helpers usable by any module regardless of the world it exports.
//! Domain layers such as the CoW SDK build on top.
//!
//! Modules keep their own `wit_bindgen::generate!` call and pull helpers
//! and canonical primitive types from here; this crate takes primitive
//! types (`&[u8]`, slices) rather than the per-module `Fault`, so it
//! emits no wit-bindgen output of its own.
//!
//! Modules:
//! - [`prelude`] - alloy primitive re-exports.
//! - [`host`] - the [`Host`](host::Host) seam over the six core host interfaces, plus the [`Fault`](host::Fault) vocabulary.
//! - [`keeper`] - keeper stores ([`WatchSet`](keeper::WatchSet), [`Gates`](keeper::Gates), [`Journal`](keeper::Journal)), the [`Poller`](keeper::Poller) seam, and the [`Retrier`](keeper::Retrier).
//! - [`chain`] - typed chain access and the alloy provider seam.
//! - [`events`] - chain-log delivery.
//! - [`config`] - config-table lookups and decimal scaling.
//! - [`address`] - EVM address parsing.
//! - [`http`] - outbound HTTP over wasi:http.
//! - [`tracing`] - guest-side `tracing` facade.
//! - [`module`] and [`bind_host_via_wit_bindgen!`](crate::bind_host_via_wit_bindgen) generate the per-cdylib glue.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![warn(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

/// Generate the per-cdylib module glue from an `impl` block of named
/// handlers. See [`nexum_module_macros::module`].
pub use nexum_module_macros::module;

pub mod address;
pub mod chain;
pub mod config;
pub mod events;
pub mod host;
pub mod http;
pub mod keeper;
pub mod prelude;
pub mod tracing;
pub mod wit_bindgen_macro;

/// Shared log-level vocabulary for every SDK log path. `Ord` is
/// filter-oriented (`ERROR` is least verbose), not severity-ordered.
pub use tracing_core::Level;

#[cfg(test)]
mod proptests;
