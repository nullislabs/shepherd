//! # ethflow-watcher (Shepherd module)
//!
//! Subscribes to `CoWSwapOnchainOrders.OrderPlacement` logs from the
//! canonical CoWSwap EthFlow contracts, computes each placement's
//! orderbook UID, and puts it under the host's status watch through the
//! typed cow venue client. The registry polls the cow adapter and fans
//! transitions back as `intent-status` events; the module journals
//! `observed:{uid}` on the first one. Observe-only: it never submits
//! (see `strategy.rs`).
//!
//! ## Module layout
//!
//! - `strategy.rs` holds the pure logic and unit tests against the
//!   `nexum_sdk::host` and `videre_sdk` client seams. It does not know
//!   `wit-bindgen` exists.
//! - `lib.rs` (this file) is the per-cdylib glue: the
//!   `#[videre_sdk::keeper]` handler impl that binds the world derived
//!   from `module.toml` and delegates each event to `strategy`.

// wit_bindgen::generate! expands to host-import shims whose arity
// matches the WIT signatures, which can exceed clippy's
// too-many-arguments threshold.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::too_many_arguments)]

// The keeper glue only resolves against the engine's wasm component
// host. Cfg-gate it so the `rlib` artefact (consumed by
// `shepherd-backtest`) carries just the strategy code without dangling
// `extern "C"` imports; the `use wit_bindgen as _` line silences the
// unused-crate lint on native targets where the macro never expands.
#[cfg(not(target_arch = "wasm32"))]
use wit_bindgen as _;

pub mod strategy;

#[cfg(target_arch = "wasm32")]
mod glue {
    use cow_venue::client::CowClient;

    use crate::strategy;

    struct EthFlowWatcher;

    #[videre_sdk::keeper]
    impl EthFlowWatcher {
        fn init(_config: Vec<(String, String)>) -> Result<(), Fault> {
            install_tracing();
            tracing::info!("ethflow-watcher init");
            Ok(())
        }

        async fn on_chain_logs(batch: nexum::host::types::ChainLogs) -> Result<(), Fault> {
            let logs: Vec<nexum_sdk::events::Log> =
                batch.logs.into_iter().map(Into::into).collect();
            strategy::on_chain_logs(&WitBindgenHost, &CowClient::new(), batch.chain_id, &logs)
                .await?;
            Ok(())
        }

        fn on_intent_status(update: videre_sdk::IntentStatusUpdate) -> Result<(), Fault> {
            strategy::on_intent_status(
                &WitBindgenHost,
                &update.venue,
                &update.receipt,
                &update.status,
            )?;
            Ok(())
        }
    }
}
