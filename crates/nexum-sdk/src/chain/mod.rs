//! Chain access for guest strategies.
//!
//! Typed identity ([`Chain`], [`ChainId`]), the closed JSON-RPC read
//! surface ([`ChainMethod`]), and the alloy provider seam: a
//! [`HostTransport`] over `ChainHost::request` fronted by
//! [`ProviderHost::provider`], driven with [`block_on`]. Plus the
//! `eth_call` JSON plumbing helpers for modules that keep their own
//! `chain::request` shim.

pub mod chainlink;
pub mod eth_call;
pub mod id;
pub mod method;
pub mod provider;
pub mod transport;

pub use eth_call::{eth_call_params, parse_eth_call_result};
pub use id::{Chain, ChainId};
pub use method::ChainMethod;
pub use provider::{ProviderHost, block_on};
pub use transport::HostTransport;
