//! # price-alert (example Shepherd module)
//!
//! Polls a Chainlink price oracle on every new block and emits a
//! Warn-level log when the price crosses a config-supplied
//! threshold. Demonstrates the three load-bearing patterns of a
//! Shepherd module:
//!
//! - `chain::request` + ABI decode via `alloy_sol_types`
//! - `shepherd_sdk` helpers (`prelude`, `chain::eth_call_params`,
//!   `chain::parse_eth_call_result`)
//! - `[config]` driven behaviour parsed once in `init` and read on
//!   every subsequent event
//!
//! ## Module layout
//!
//! - `strategy.rs` holds the pure logic and tests against
//!   `shepherd_sdk::host::Host`. It does not know `wit-bindgen`
//!   exists.
//! - `lib.rs` (this file) is the per-cdylib glue: wit-bindgen import
//!   shims, the `WitBindgenHost` adapter, the `Guest` impl.
//!
//! ## Settings
//!
//! ```toml
//! [config]
//! # Chainlink AggregatorV3Interface address.
//! oracle_address = "0x694AA1769357215DE4FAC081bf1f309aDC325306"  # ETH/USD on Sepolia
//! # Oracle's decimals (Chainlink USD pairs are 8; ETH pairs 18).
//! decimals = "8"
//! # Threshold in the oracle's native units (decimal string). The
//! # module multiplies by 10**decimals at init.
//! threshold = "2500.00"
//! # Either "above" or "below". Fires when the answer crosses on
//! # the configured side.
//! direction = "below"
//! # Optional throttle: poll every N blocks. Default 1.
//! every_n_blocks = "1"
//! ```

// wit_bindgen::generate! expands to host-import shims whose arity matches
// the WIT signatures, which can exceed clippy's too-many-arguments threshold.
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

struct PriceAlert;

impl Guest for PriceAlert {
    fn init(config: Vec<(String, String)>) -> Result<(), HostError> {
        let cfg = strategy::parse_config(&config).map_err(sdk_err_into_wit)?;
        logging::log(
            logging::Level::Info,
            &format!(
                "price-alert init: oracle={:#x} threshold={} direction={:?} every_n_blocks={}",
                cfg.oracle_address, cfg.threshold_scaled, cfg.direction, cfg.every_n_blocks,
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
            strategy::on_block(&WitBindgenHost, block.chain_id, cfg, block.number)
                .map_err(sdk_err_into_wit)?;
        }
        Ok(())
    }
}

export!(PriceAlert);
