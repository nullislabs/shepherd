//! # stop-loss (example Shepherd module)
//!
//! Watches a Chainlink price oracle on every block. When the price
//! drops at or below `trigger_price`, the module submits a pre-signed
//! CoW order using the parameters from `module.toml::[config]` and
//! persists `submitted:{uid}` to dedup re-poll attempts. The owner is
//! expected to have called `GPv2Signing.setPreSignature` on-chain
//! ahead of the trigger so the orderbook accepts the submission.
//!
//! ## Module layout
//!
//! - `strategy.rs` holds the pure logic and tests against
//!   `shepherd_sdk::host::Host`. It does not know `wit-bindgen`
//!   exists.
//! - `lib.rs` (this file) is the per-cdylib glue: wit-bindgen import
//!   shims, the `WitBindgenHost` adapter, the `Guest` impl.
//!
//! Same recipe as `price-alert` - the wit-bindgen adapter
//! is intentionally mechanical and is a candidate for a future
//! declarative macro in `shepherd-sdk`.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: ["../../../wit/nexum-host", "../../../wit/shepherd-cow"],
    world: "shepherd:cow/shepherd",
    generate_all,
});

mod strategy;

use std::sync::OnceLock;

use nexum::host::{logging, types};

// `WitBindgenHost`, `convert_err`, `sdk_err_into_wit`, `convert_level`
// are generated below. Single source of truth in `shepherd-sdk`.
shepherd_sdk::bind_host_via_wit_bindgen!();

static SETTINGS: OnceLock<strategy::Settings> = OnceLock::new();

struct StopLoss;

impl Guest for StopLoss {
    fn init(config: Vec<(String, String)>) -> Result<(), HostError> {
        let cfg = strategy::parse_config(&config).map_err(sdk_err_into_wit)?;
        logging::log(
            logging::Level::Info,
            &format!(
                "stop-loss init: owner={:#x} trigger={} sell={:#x} buy={:#x}",
                cfg.owner, cfg.trigger_price_scaled, cfg.sell_token, cfg.buy_token,
            ),
        );
        let _ = SETTINGS.set(cfg);
        Ok(())
    }

    fn on_event(event: types::Event) -> Result<(), HostError> {
        let Some(cfg) = SETTINGS.get() else {
            return Ok(());
        };
        if let types::Event::Block(block) = event {
            strategy::on_block(&WitBindgenHost, block.chain_id, cfg).map_err(sdk_err_into_wit)?;
        }
        Ok(())
    }
}

export!(StopLoss);
