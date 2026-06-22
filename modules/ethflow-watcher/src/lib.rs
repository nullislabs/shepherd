//! # ethflow-watcher (Shepherd module)
//!
//! Subscribes to `CoWSwapOnchainOrders.OrderPlacement` logs from the
//! CoWSwap EthFlow contracts and resubmits each placed order through
//! the orderbook API with `Signature::Eip1271`. The EthFlow contract
//! is the EIP-1271 verifier, so the `from` field on the resubmission
//! is the contract address (not the original native-token seller).
//!
//! ## Module layout (BLEU-855)
//!
//! - `strategy.rs` holds the pure logic and unit tests against
//!   `shepherd_sdk::host::Host`. It does not know `wit-bindgen`
//!   exists.
//! - `lib.rs` (this file) is the per-cdylib glue: wit-bindgen import
//!   shims, the `WitBindgenHost` adapter that bridges the generated
//!   free functions to the SDK traits, and the `Guest` impl that
//!   delegates the `Logs` event variant to `strategy::on_logs`.

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

struct EthFlowWatcher;

impl Guest for EthFlowWatcher {
    fn init(_config: Vec<(String, String)>) -> Result<(), HostError> {
        logging::log(logging::Level::Info, "ethflow-watcher init");
        Ok(())
    }

    fn on_event(event: types::Event) -> Result<(), HostError> {
        if let types::Event::Logs(logs) = event {
            let views: Vec<strategy::LogView<'_>> = logs
                .iter()
                .map(|log| strategy::LogView {
                    chain_id: log.chain_id,
                    address: &log.address,
                    topics: &log.topics,
                    data: &log.data,
                })
                .collect();
            strategy::on_logs(&WitBindgenHost, &views).map_err(sdk_err_into_wit)?;
        }
        // Block / Tick / Message are not used by this module.
        Ok(())
    }
}

export!(EthFlowWatcher);
