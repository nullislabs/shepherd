//! # twap-monitor (Shepherd keeper module)
//!
//! Indexes `ComposableCoW.ConditionalOrderCreated` and v2
//! `ConditionalOrderRemoved` logs and polls each watched conditional
//! order on every block, submitting tranches to the CoW venue as they
//! go live. Pure logic lives in `keeper`; `lib.rs` is the
//! `#[videre_sdk::keeper]` glue.

// wit_bindgen::generate! expands to host-import shims whose arity
// matches the WIT signatures, which can exceed clippy's
// too-many-arguments threshold.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::too_many_arguments)]

mod keeper;

use cow_venue::CowClient;
use nexum::host::types;

struct TwapMonitor;

#[videre_sdk::keeper]
impl TwapMonitor {
    fn init(_config: Vec<(String, String)>) -> Result<(), Fault> {
        install_tracing();
        tracing::info!("twap-monitor init");
        Ok(())
    }

    fn on_chain_logs(batch: types::ChainLogs) -> Result<(), Fault> {
        let logs: Vec<nexum_sdk::events::Log> = batch.logs.into_iter().map(Into::into).collect();
        keeper::on_chain_logs(&WitBindgenHost, &logs)?;
        Ok(())
    }

    fn on_block(block: types::Block) -> Result<(), Fault> {
        let info = keeper::BlockInfo {
            chain_id: block.chain_id,
            number: block.number,
            timestamp: block.timestamp,
        };
        keeper::on_block(&WitBindgenHost, &CowClient::new(), info)?;
        Ok(())
    }

    fn on_intent_status(update: videre_sdk::IntentStatusUpdate) -> Result<(), Fault> {
        let body = videre_sdk::status_body::StatusBody::decode(&update.status)
            .map_err(|err| Fault::InvalidInput(err.to_string()))?;
        tracing::info!(
            "cow intent status {:?} ({} receipt bytes)",
            body.status,
            update.receipt.len(),
        );
        Ok(())
    }
}
