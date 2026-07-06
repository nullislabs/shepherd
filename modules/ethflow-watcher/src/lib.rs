//! # ethflow-watcher (Shepherd module)
//!
//! Subscribes to `CoWSwapOnchainOrders.OrderPlacement` logs from the
//! canonical CoWSwap EthFlow contracts and verifies the orderbook's
//! native indexer caught each placement via `GET /api/v1/orders/{uid}`.
//! See `strategy.rs` for the design rationale: the orderbook
//! backend indexes EthFlow `OrderPlacement` events server-side with
//! its own dual-validTo bookkeeping, so `POST /api/v1/orders` is
//! structurally the wrong endpoint for on-chain EthFlow orders. The
//! module observes and verifies, it does not submit.
//!
//! ## Module layout
//!
//! - `strategy.rs` holds the pure logic and unit tests against
//!   `nexum_sdk::host::Host`. It does not know `wit-bindgen`
//!   exists.
//! - `lib.rs` (this file) is the per-cdylib glue: wit-bindgen import
//!   shims, the `WitBindgenHost` adapter that bridges the generated
//!   free functions to the SDK traits, and the `Guest` impl that
//!   delegates the `chain-logs` event variant to `strategy::on_chain_logs`.

// wit_bindgen::generate! expands to host-import shims whose arity
// matches the WIT signatures, which can exceed clippy's
// too-many-arguments threshold.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::too_many_arguments)]

// The wit-bindgen-generated import shims only resolve against the
// engine's wasm component host - they have no native-target
// equivalent. Cfg-gate the entire glue layer so the `rlib` artefact
// (consumed by `shepherd-backtest`) carries just the
// strategy code without dangling `extern "C"` imports. The
// `use wit_bindgen as _` line below silences the unused-crate
// lint on native targets where the macro never expands.
#[cfg(not(target_arch = "wasm32"))]
use wit_bindgen as _;

#[cfg(target_arch = "wasm32")]
wit_bindgen::generate!({
    path: ["../../wit/nexum-host", "../../wit/shepherd-cow"],
    world: "shepherd:cow/shepherd",
    generate_all,
});

pub mod strategy;

// `WitBindgenHost`, `sdk_fault_into_wit`, `convert_level`
// are generated below. Single source of truth in `nexum-sdk` + `shepherd-sdk`.
// Gated on `wasm32` so the strategy can be reused in native targets
// (e.g. the backtest replay harness in `crates/shepherd-backtest`).
#[cfg(target_arch = "wasm32")]
use nexum::host::types;

#[cfg(target_arch = "wasm32")]
shepherd_sdk::bind_cow_host_via_wit_bindgen!();

#[cfg(target_arch = "wasm32")]
struct EthFlowWatcher;

#[cfg(target_arch = "wasm32")]
impl Guest for EthFlowWatcher {
    fn init(_config: Vec<(String, String)>) -> Result<(), Fault> {
        install_tracing();
        tracing::info!("ethflow-watcher init");
        Ok(())
    }

    fn on_event(event: types::Event) -> Result<(), Fault> {
        if let types::Event::ChainLogs(batch) = event {
            let logs: Vec<nexum_sdk::events::Log> =
                batch.logs.into_iter().map(Into::into).collect();
            strategy::on_chain_logs(&WitBindgenHost, batch.chain_id, &logs)
                .map_err(sdk_fault_into_wit)?;
        }
        // Block / Tick / Message are not used by this module.
        Ok(())
    }
}

#[cfg(target_arch = "wasm32")]
export!(EthFlowWatcher);
