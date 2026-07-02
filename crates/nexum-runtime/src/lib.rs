//! Nexum runtime: a wasmtime-based host for WASM Component Model
//! modules, usable as an embeddable library. The bundled binary is a
//! thin consumer of the same public surface.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

// alloy split its API across multiple crates; we depend on the
// transports directly so cargo resolves the right feature set, but
// the runtime code only names them through the `alloy_provider`
// re-exports. Silence `unused_crate_dependencies` with `as _`.
use alloy_rpc_client as _;
use alloy_transport as _;
use alloy_transport_ws as _;

// Consumed by the bin target only; named here so the lib target
// passes `unused_crate_dependencies`.
use metrics_exporter_prometheus as _;
use tracing_subscriber as _;

pub mod bindings;
pub mod cli;
pub mod engine_config;
pub mod host;
pub mod manifest;
pub mod runtime;
pub mod supervisor;
