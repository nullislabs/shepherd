//! # shepherd-sdk
//!
//! CoW-domain SDK for Shepherd modules, layered on the generic
//! [`nexum_sdk`]. Everything host-neutral (the host trait seam, config
//! and address parsing, `eth_call` plumbing, HTTP, the tracing facade)
//! lives in `nexum-sdk`; this crate carries only the CoW Protocol
//! surface. Modules import both crates directly.
//!
//! ## What lives here
//!
//! - [`prelude`] - `use shepherd_sdk::prelude::*` imports cowprotocol's
//!   order / signing surface ([`OrderCreation`],
//!   [`OrderData`], [`OrderUid`], [`OrderKind`], [`Signature`],
//!   [`Chain`], [`GPv2OrderData`], [`EMPTY_APP_DATA_JSON`]).
//!
//! - [`cow`] - [`run`], the poll -> outcome -> gate/journal/submit
//!   composition over the keeper stores, dispatching the structured
//!   [`Verdict`] from the `composable-cow` keeper crate and submitting
//!   through the typed [`CowClient`] on the `videre:venue/client`
//!   seam; `GPv2OrderData` -> `OrderData` bridging
//!   ([`gpv2_to_order_data`]) and the classifiers mapping submit
//!   failures into the keeper [`RetryAction`]. The legacy
//!   [`CowApiHost`] trait for `shepherd:cow/cow-api` (and the
//!   [`CowHost`] bound over the core [`Host`]) stays for the read
//!   paths and the transitional [`CowApiTransport`] bridge.
//!
//! - [`bind_cow_host_via_wit_bindgen!`](bind_cow_host_via_wit_bindgen) -
//!   the CoW layering of `nexum_sdk::bind_host_via_wit_bindgen!`:
//!   the generic `WitBindgenHost` adapter plus the `CowApiHost` impl.
//!
//! ## Why no `wit_bindgen::generate!` here
//!
//! The macro emits types into the calling crate (the module's
//! cdylib). Re-exporting wit-bindgen output from a library crate
//! would duplicate symbols and break the component-export contract.
//! Helpers in this SDK therefore take primitive types (`&[u8]`,
//! `Option<&str>`, slices) rather than the per-module `Fault`
//! type; modules unpack their `Fault` on the way in. Trade-off
//! documented in ADR-0006 / ADR-0007 - the SDK stays on the guest
//! side, neutral to which world the module exports.
//!
//! [`OrderCreation`]: cowprotocol::OrderCreation
//! [`OrderData`]: cowprotocol::OrderData
//! [`OrderUid`]: cowprotocol::OrderUid
//! [`OrderKind`]: cowprotocol::OrderKind
//! [`Signature`]: cowprotocol::Signature
//! [`Chain`]: cowprotocol::Chain
//! [`GPv2OrderData`]: cowprotocol::GPv2OrderData
//! [`EMPTY_APP_DATA_JSON`]: cowprotocol::EMPTY_APP_DATA_JSON
//! [`CowApiHost`]: cow::CowApiHost
//! [`CowHost`]: cow::CowHost
//! [`CowClient`]: cow::CowClient
//! [`CowApiTransport`]: cow::CowApiTransport
//! [`Host`]: nexum_sdk::host::Host
//! [`gpv2_to_order_data`]: cow::gpv2_to_order_data
//! [`Verdict`]: composable_cow::Verdict
//! [`RetryAction`]: cow::RetryAction
//! [`run`]: cow::run()

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![warn(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod cow;
pub mod prelude;
pub mod wit_bindgen_macro;

#[cfg(test)]
mod proptests;

#[cfg(test)]
mod tests {
    //! Locks the prelude's surface - the build itself proves the
    //! re-exports compile against both `wasm32-wasip2` and the
    //! host target.

    use crate::prelude::*;

    #[test]
    fn prelude_re_exports_resolve() {
        let _kind: OrderKind = OrderKind::Sell;
        let _chain: Chain = Chain::Sepolia;
        assert_eq!(EMPTY_APP_DATA_JSON, "{}");
    }
}
