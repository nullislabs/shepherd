//! `chain::request` JSON plumbing.
//!
//! Build the `[{to, data}, "latest"]` params array for `eth_call` and
//! parse the `"0x..."` hex result string. Pure-logic helpers so a
//! module can plumb its own `chain::request` shim around them.

pub mod chainlink;
pub mod eth_call;

pub use eth_call::{eth_call_params, parse_eth_call_result};
