//! Chain access for guest strategies.
//!
//! Chain identity (alloy [`Chain`]), the closed JSON-RPC read
//! surface ([`ChainMethod`]), and the alloy provider seam: a
//! [`HostTransport`] over `ChainHost::request` fronted by
//! [`ProviderHost::provider`], driven with [`block_on`]. Plus the
//! `eth_call` JSON plumbing helpers for modules that keep their own
//! `chain::request` shim.

pub mod chainlink;
pub mod eth_call;
pub mod provider;
pub mod transport;

pub use alloy_chains::Chain;
pub use eth_call::{eth_call_params, parse_eth_call_result};
/// The read surface is defined once in `nexum-world`; guest and host
/// re-export the same type, so the allowlist cannot drift.
pub use nexum_world::ChainMethod;
pub use provider::{ProviderHost, block_on};
pub use transport::HostTransport;
