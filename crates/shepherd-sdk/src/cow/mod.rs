//! CoW Protocol bridging.
//!
//! Type conversions and ABI decoding helpers that translate between
//! the on-chain shape (`GPv2OrderData`, `IConditionalOrder` reverts,
//! orderbook JSON) and the typed Rust surface (`OrderData`,
//! `PollOutcome`, `RetryAction`).
//!
//! Each submodule stays purely host-neutral: helpers take primitive
//! arguments (`&[u8]`, `Option<&str>`, slices) so they can be unit-
//! tested without wit-bindgen scaffolding and re-used unchanged by
//! TWAP, EthFlow, and future strategy modules.

pub mod app_data;
pub mod composable;
pub mod error;
pub mod order;

pub use app_data::resolve_app_data;
pub use composable::{IConditionalOrder, PollOutcome, decode_revert};
pub use error::{RetryAction, classify_api_error, try_decode_api_error};
pub use order::gpv2_to_order_data;

use crate::host::{CowApiHost, Host};

/// Host bound for strategies that reach the CoW Protocol orderbook.
pub trait CowHost: Host + CowApiHost {}
impl<T: Host + CowApiHost> CowHost for T {}
