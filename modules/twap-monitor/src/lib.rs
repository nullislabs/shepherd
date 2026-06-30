//! # twap-monitor (Shepherd module)
//!
//! Indexes `ComposableCoW.ConditionalOrderCreated` logs and polls each
//! watched conditional order on every block, submitting tranches to
//! the CoW orderbook as they go live.
//!
//! ## Module layout (BLEU-854)
//!
//! - `strategy.rs` holds the pure logic and unit tests against
//!   `shepherd_sdk::host::Host`. It does not know `wit-bindgen`
//!   exists.
//! - `lib.rs` (this file) is the per-cdylib glue: wit-bindgen import
//!   shims, the `WitBindgenHost` adapter that bridges the generated
//!   free functions to the SDK traits, and the `Guest` impl that
//!   delegates each event variant to `strategy`.
//!
//! Same recipe as `modules/examples/price-alert` (BLEU-851) and
//! `modules/examples/stop-loss` (BLEU-852).

// wit_bindgen::generate! expands to host-import shims whose arity
// matches the WIT signatures, which can exceed clippy's
// too-many-arguments threshold.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: ["../../wit/nexum-host", "../../wit/shepherd-cow"],
    world: "shepherd:cow/shepherd",
    generate_all,
});

mod strategy;

use nexum::host::{logging, types};

// `WitBindgenHost`, `convert_err`, `sdk_err_into_wit`, `convert_level`
// are generated below. Single source of truth in `shepherd-sdk`.
shepherd_sdk::bind_host_via_wit_bindgen!();

struct TwapMonitor;

impl Guest for TwapMonitor {
    fn init(_config: Vec<(String, String)>) -> Result<(), HostError> {
        logging::log(logging::Level::Info, "twap-monitor init");
        Ok(())
    }

    fn on_event(event: types::Event) -> Result<(), HostError> {
        match event {
            types::Event::Logs(logs) => {
                let views: Vec<strategy::LogView<'_>> = logs
                    .iter()
                    .map(|log| strategy::LogView {
                        topics: &log.topics,
                        data: &log.data,
                    })
                    .collect();
                strategy::on_logs(&WitBindgenHost, &views).map_err(sdk_err_into_wit)?;
            }
            types::Event::Block(block) => {
                let info = strategy::BlockInfo {
                    chain_id: block.chain_id,
                    number: block.number,
                    timestamp: block.timestamp,
                };
                strategy::on_block(&WitBindgenHost, info).map_err(sdk_err_into_wit)?;
            }
            // Tick / Message are not used by this module.
            _ => {}
        }
        Ok(())
    }
}

export!(TwapMonitor);
