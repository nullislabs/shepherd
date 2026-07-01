//! # shepherd-sdk
//!
//! Guest-side SDK for Shepherd modules. The crate is the shared
//! companion to the per-module `wit_bindgen::generate!` invocation:
//! modules keep their own wit-bindgen call (which emits the world-
//! specific `Guest` trait, `HostError` shape, and host import shims
//! into the module's own crate) and pull helpers + canonical
//! primitive types from here.
//!
//! ## What lives here
//!
//! - [`prelude`] - `use shepherd_sdk::prelude::*` imports alloy
//!   primitives ([`Address`], [`B256`], [`Bytes`], [`U256`],
//!   [`keccak256`]) and cowprotocol's order / signing / orderbook
//!   surface ([`OrderCreation`], [`OrderData`], [`OrderUid`],
//!   [`OrderKind`], [`Signature`], [`Chain`], [`GPv2OrderData`],
//!   [`EMPTY_APP_DATA_JSON`], [`ApiError`]).
//!
//! - [`cow`] - `GPv2OrderData` -> `OrderData` bridging
//!   ([`gpv2_to_order_data`]), `IConditionalOrder` revert decoding
//!   ([`PollOutcome`] + [`decode_revert`]), and the
//!   [`RetryAction`] classifier driving submit-failure dispatch.
//!
//! - [`chain`] - `eth_call` JSON plumbing
//!   ([`eth_call_params`], [`parse_eth_call_result`],
//!   [`decode_revert_hex`]).
//!
//! - [`host`] - host trait seam ([`Host`] / [`ChainHost`] /
//!   [`LocalStoreHost`] / [`CowApiHost`] / [`LoggingHost`]) plus a
//!   host-neutral [`HostError`]. Modules that want host-free tests
//!   structure their strategy logic against these traits and slot
//!   in the `shepherd-sdk-test` mocks. See the host module docs for
//!   the wit-bindgen adapter pattern.
//!
//! - `store` - placeholder for `WatchSet` / `BackoffLedger`
//!   per ADR-0006. Populated when a second strategy module needs
//!   the same key conventions.
//!
//! ## Why no `wit_bindgen::generate!` here
//!
//! The macro emits types into the calling crate (the module's
//! cdylib). Re-exporting wit-bindgen output from a library crate
//! would duplicate symbols and break the component-export contract.
//! Helpers in this SDK therefore take primitive types (`&[u8]`,
//! `Option<&str>`, slices) rather than the per-module `HostError`
//! struct; modules unpack their `HostError` on the way in. Trade-off
//! documented in ADR-0006 / ADR-0007 - the SDK stays on the guest
//! side, neutral to which world the module exports.
//!
//! [`Address`]: alloy_primitives::Address
//! [`B256`]: alloy_primitives::B256
//! [`Bytes`]: alloy_primitives::Bytes
//! [`U256`]: alloy_primitives::U256
//! [`keccak256`]: alloy_primitives::keccak256
//! [`OrderCreation`]: cowprotocol::OrderCreation
//! [`OrderData`]: cowprotocol::OrderData
//! [`OrderUid`]: cowprotocol::OrderUid
//! [`OrderKind`]: cowprotocol::OrderKind
//! [`Signature`]: cowprotocol::Signature
//! [`Chain`]: cowprotocol::Chain
//! [`GPv2OrderData`]: cowprotocol::GPv2OrderData
//! [`EMPTY_APP_DATA_JSON`]: cowprotocol::EMPTY_APP_DATA_JSON
//! [`ApiError`]: cowprotocol::error::ApiError
//! [`gpv2_to_order_data`]: cow::gpv2_to_order_data
//! [`PollOutcome`]: cow::PollOutcome
//! [`decode_revert`]: cow::decode_revert
//! [`RetryAction`]: cow::RetryAction
//! [`eth_call_params`]: chain::eth_call_params
//! [`parse_eth_call_result`]: chain::parse_eth_call_result
//! [`decode_revert_hex`]: chain::decode_revert_hex
//! [`Host`]: host::Host
//! [`ChainHost`]: host::ChainHost
//! [`LocalStoreHost`]: host::LocalStoreHost
//! [`CowApiHost`]: host::CowApiHost
//! [`LoggingHost`]: host::LoggingHost
//! [`HostError`]: host::HostError

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![warn(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod address;
pub mod chain;
pub mod config;
pub mod cow;
pub mod host;
pub mod prelude;
pub mod wit_bindgen_macro;

#[cfg(test)]
mod proptests;

#[cfg(test)]
mod tests {
    //! The skeleton has no behaviour to exercise; this test just
    //! locks the prelude's surface - the build itself proves the
    //! re-exports compile against both `wasm32-wasip2` and the
    //! host target.

    use crate::prelude::*;

    #[test]
    fn prelude_re_exports_resolve() {
        let _addr: Address = Address::ZERO;
        let _hash: B256 = B256::ZERO;
        let _amt: U256 = U256::ZERO;
        let _empty: Bytes = Bytes::new();
        // cowprotocol re-exports
        let _kind: OrderKind = OrderKind::Sell;
        let _chain: Chain = Chain::Sepolia;
        assert_eq!(EMPTY_APP_DATA_JSON, "{}");
    }
}
