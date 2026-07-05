//! # nexum-sdk
//!
//! Guest-side SDK for nexum runtime modules. The helpers here are
//! host-neutral and domain-free: any module targeting the runtime can
//! use them regardless of which world it exports. Domain layers such as
//! the CoW SDK depend on this crate and add their own surface on top.
//!
//! The crate is the shared companion to the per-module
//! `wit_bindgen::generate!` invocation: modules keep their own
//! wit-bindgen call (which emits the world-specific `Guest` trait,
//! `HostError` shape, and host import shims into the module's own
//! crate) and pull helpers and canonical primitive types from here.
//!
//! ## What lives here
//!
//! - [`prelude`] - `use nexum_sdk::prelude::*` imports the alloy
//!   primitives ([`Address`], [`B256`], [`Bytes`], [`U256`],
//!   [`keccak256`]).
//!
//! - [`host`] - host trait seam ([`Host`] / [`ChainHost`] /
//!   [`LocalStoreHost`] / [`LoggingHost`]) plus a host-neutral
//!   [`HostError`]. Modules that want host-free tests structure their
//!   strategy logic against these traits and slot in the
//!   `nexum-sdk-test` mocks. See the host module docs for the
//!   wit-bindgen adapter pattern.
//!
//! - [`bind_host_via_wit_bindgen!`](crate::bind_host_via_wit_bindgen) -
//!   generates the per-module `WitBindgenHost` adapter over the
//!   wit-bindgen import shims.
//!
//! - [`chain`] - `eth_call` JSON plumbing ([`eth_call_params`],
//!   [`parse_eth_call_result`]) and the Chainlink AggregatorV3 reader
//!   ([`read_latest_answer`]).
//!
//! - [`config`] - `(key, value)` config-table lookups and decimal
//!   scaling ([`get_required`], [`get_optional`], [`scale_decimal`]).
//!
//! - [`address`] - EVM address parsing with typed errors
//!   ([`parse_address`], [`parse_address_list`]).
//!
//! - [`http`] - outbound HTTP over wasi:http in the standard `http`
//!   crate's request/response vocabulary: a synchronous [`fetch`]
//!   helper (guest target only), the [`Fetch`] trait seam for host-free
//!   strategy tests, and a [`FetchError`] that distinguishes allowlist
//!   denials from transport failures.
//!
//! - [`tracing`] - guest-side `tracing` facade: an events-only
//!   subscriber plus panic hook that forward through a [`LogSink`]
//!   seam. The bind macro wires the bound host logging call into the
//!   sink so module authors emit `tracing::info!(...)` with no host
//!   parameter to thread.
//!
//! ## Why no `wit_bindgen::generate!` here
//!
//! The macro emits types into the calling crate (the module's
//! cdylib). Re-exporting wit-bindgen output from a library crate
//! would duplicate symbols and break the component-export contract.
//! Helpers in this SDK therefore take primitive types (`&[u8]`,
//! `Option<&str>`, slices) rather than the per-module `HostError`
//! struct; modules unpack their `HostError` on the way in.
//!
//! [`Address`]: alloy_primitives::Address
//! [`B256`]: alloy_primitives::B256
//! [`Bytes`]: alloy_primitives::Bytes
//! [`U256`]: alloy_primitives::U256
//! [`keccak256`]: alloy_primitives::keccak256
//! [`Host`]: host::Host
//! [`ChainHost`]: host::ChainHost
//! [`LocalStoreHost`]: host::LocalStoreHost
//! [`LoggingHost`]: host::LoggingHost
//! [`HostError`]: host::HostError
//! [`eth_call_params`]: chain::eth_call_params
//! [`parse_eth_call_result`]: chain::parse_eth_call_result
//! [`read_latest_answer`]: chain::chainlink::read_latest_answer
//! [`get_required`]: config::get_required
//! [`get_optional`]: config::get_optional
//! [`scale_decimal`]: config::scale_decimal
//! [`parse_address`]: address::parse_address
//! [`parse_address_list`]: address::parse_address_list
//! [`fetch`]: http::Fetch::fetch
//! [`Fetch`]: http::Fetch
//! [`FetchError`]: http::FetchError
//! [`LogSink`]: tracing::LogSink

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![warn(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod address;
pub mod chain;
pub mod config;
pub mod host;
pub mod http;
pub mod prelude;
pub mod tracing;
pub mod wit_bindgen_macro;

#[cfg(test)]
mod proptests;
