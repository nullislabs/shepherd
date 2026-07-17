//! # stop-loss (example Shepherd module)
//!
//! Watches a Chainlink price oracle on every block. When the price
//! drops at or below `trigger_price`, the module submits a CoW order
//! intent through the pool using the parameters from
//! `module.toml::[config]` and persists a `submitted:` marker to dedup
//! re-poll attempts. The cow adapter posts the unsigned order
//! pre-sign; the owner is expected to call
//! `GPv2Signing.setPreSignature` on-chain ahead of the trigger so the
//! orderbook activates the submission.
//!
//! ## Module layout
//!
//! - `strategy.rs` holds the pure logic and unit tests against the
//!   `nexum_sdk::host` trait seams and the videre `VenueTransport`
//!   seam. It does not know `wit-bindgen` exists.
//! - `lib.rs` (this file) is the `#[videre_sdk::keeper]` glue: the
//!   macro derives the component world from `module.toml`, emits the
//!   `WitBindgenHost` adapter, and dispatches each event variant to
//!   `strategy` with the typed [`CowClient`] over the module's own
//!   `videre:venue/client` import.

// wit_bindgen::generate! expands to host-import shims whose arity
// matches the WIT signatures, which can exceed clippy's
// too-many-arguments threshold.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::too_many_arguments)]

mod strategy;

use std::sync::OnceLock;

use cow_venue::CowClient;

static SETTINGS: OnceLock<strategy::Settings> = OnceLock::new();

struct StopLoss;

#[videre_sdk::keeper]
impl StopLoss {
    fn init(config: Vec<(String, String)>) -> Result<(), Fault> {
        install_tracing();
        let cfg = strategy::parse_config(&config)?;
        tracing::info!(
            "stop-loss init: owner={:#x} trigger={} sell={:#x} buy={:#x}",
            cfg.owner,
            cfg.trigger_price_scaled,
            cfg.sell_token,
            cfg.buy_token,
        );
        let _ = SETTINGS.set(cfg);
        Ok(())
    }

    fn on_block(block: nexum::host::types::Block) -> Result<(), Fault> {
        let Some(cfg) = SETTINGS.get() else {
            return Ok(());
        };
        strategy::on_block(&WitBindgenHost, &CowClient::new(), block.chain_id, cfg)?;
        Ok(())
    }
}
