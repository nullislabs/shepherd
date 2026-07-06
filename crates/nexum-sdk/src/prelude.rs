//! Bulk-imports the primitives every module uses on every other line.
//! `use nexum_sdk::prelude::*` covers the alloy address / hash /
//! numeric types the chain helpers consume.
//!
//! The wit-bindgen-generated types (`Guest`, `Fault`, `Event`, ...)
//! are **not** re-exported here because they live in each module's own
//! crate (one `wit_bindgen::generate!` call per cdylib). Domain SDKs
//! ship their own prelude for their protocol surface.

pub use alloy_primitives::{Address, B256, Bytes, U256, address, b256, hex, keccak256};
