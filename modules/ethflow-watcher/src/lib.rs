//! # ethflow-watcher (Shepherd module)
//!
//! Triggers on `CoWSwapOnchainOrders.OrderPlacement` logs from the
//! canonical EthFlow contracts, computes each placement's orderbook UID,
//! and puts it under the host's status watch; the registry polls the cow
//! adapter and fans transitions back as `intent-status` events, journalled
//! as `observed:{uid}`. Observe-only, never submits. Pure logic lives in
//! `keeper`; `lib.rs` is the `#[videre_sdk::keeper]` glue.

// wit_bindgen::generate! expands to host-import shims whose arity
// matches the WIT signatures, which can exceed clippy's
// too-many-arguments threshold.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::too_many_arguments)]

// The keeper glue only resolves against the engine's wasm component
// host. Cfg-gate it so a native build of this crate carries just the
// keeper code without dangling `extern "C"` imports; the
// `use wit_bindgen as _` line silences the unused-crate lint on native
// targets where the macro never expands.
#[cfg(not(target_arch = "wasm32"))]
use wit_bindgen as _;

pub mod keeper;

#[cfg(target_arch = "wasm32")]
mod glue {
    use cow_venue::client::CowClient;

    use crate::keeper;

    struct EthFlowWatcher;

    #[videre_sdk::keeper]
    impl EthFlowWatcher {
        fn init(_config: Vec<(String, String)>) -> Result<(), Fault> {
            install_tracing();
            tracing::info!("ethflow-watcher init");
            Ok(())
        }

        async fn on_event(log: nexum::host::types::Log) -> Result<(), Fault> {
            // The alloy `Log` carries no chain id, so read it off the WIT
            // record before the conversion drops it.
            let chain_id = log.chain_id;
            let log: nexum_sdk::sol_events::Log = log.into();
            keeper::on_event(&WitBindgenHost, &CowClient::new(), chain_id, &log).await?;
            Ok(())
        }

        fn on_intent_status(update: videre_sdk::IntentStatusUpdate) -> Result<(), Fault> {
            keeper::on_intent_status(
                &WitBindgenHost,
                &update.venue,
                &update.receipt,
                &update.status,
            )?;
            Ok(())
        }
    }
}
