//! Nexum runtime: a wasmtime-based host for WASM Component Model
//! modules, usable as an embeddable library. The bundled binary is a
//! thin consumer of the same public surface.
//!
//! Zero-leak charter: this crate is settlement-domain-agnostic. It
//! carries no domain symbol or WIT reference, `nexum:host` stays a
//! leaf WIT package, and no crate edge reaches a domain crate. The
//! zero-leak script under `scripts/` enforces this in CI.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

// alloy split its API across multiple crates; we depend on the
// transports directly so cargo resolves the right feature set, but
// the runtime code only names them through the `alloy_provider`
// re-exports. Silence `unused_crate_dependencies` with `as _`.
use alloy_rpc_client as _;
use alloy_transport as _;
use alloy_transport_ws as _;

/// Sealing markers for [`preset::Runtime`] and
/// [`host::component::RuntimeTypes`]: implement alongside the trait.
#[doc(hidden)]
pub mod sealed {
    pub trait SealedRuntimeTypes {}
    pub trait SealedRuntime {}
}

pub mod addons;
pub mod bindings;
pub mod bootstrap;
pub mod builder;
pub mod engine_config;
pub mod host;
pub mod manifest;
pub mod preset;
pub mod runtime;
pub mod supervisor;

#[cfg(feature = "test-utils")]
pub mod test_utils;
