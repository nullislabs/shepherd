//! # twap-monitor (Shepherd module)
//!
//! Indexes `ComposableCoW.ConditionalOrderCreated` logs and polls each
//! watched conditional order on every block, submitting tranches to
//! the CoW orderbook as they go live.
//!
//! ## Module layout
//!
//! - `strategy.rs` holds the pure logic and unit tests against
//!   `nexum_sdk::host::Host`. It does not know `wit-bindgen`
//!   exists.
//! - `lib.rs` (this file) is the per-cdylib glue: wit-bindgen import
//!   shims, the `WitBindgenHost` adapter that bridges the generated
//!   free functions to the SDK traits, and the `Guest` impl that
//!   delegates each event variant to `strategy`.
//!
//! Same recipe as `modules/examples/price-alert` and
//! `modules/examples/stop-loss`.

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

use nexum::host::types;

// `WitBindgenHost`, `sdk_fault_into_wit`, `convert_level`
// are generated below. Single source of truth in `nexum-sdk` + `shepherd-sdk`.
shepherd_sdk::bind_cow_host_via_wit_bindgen!();

struct TwapMonitor;

impl Guest for TwapMonitor {
    fn init(_config: Vec<(String, String)>) -> Result<(), Fault> {
        install_tracing();
        tracing::info!("twap-monitor init");
        Ok(())
    }

    fn on_event(event: types::Event) -> Result<(), Fault> {
        match event {
            types::Event::ChainLogs(batch) => {
                let logs: Vec<nexum_sdk::events::Log> =
                    batch.logs.into_iter().map(Into::into).collect();
                strategy::on_chain_logs(&WitBindgenHost, &logs).map_err(sdk_fault_into_wit)?;
            }
            types::Event::Block(block) => {
                let info = strategy::BlockInfo {
                    chain_id: block.chain_id,
                    number: block.number,
                    timestamp: block.timestamp,
                };
                strategy::on_block(&WitBindgenHost, info).map_err(sdk_fault_into_wit)?;
            }
            // Tick / Message are not used by this module.
            _ => {}
        }
        Ok(())
    }
}

export!(TwapMonitor);
