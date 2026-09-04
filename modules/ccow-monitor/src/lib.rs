//! # ccow-monitor (Shepherd keeper module)
//!
//! Indexes `ComposableCoW.ConditionalOrderCreated` and v2
//! `ConditionalOrderRemoved` logs and polls each registered conditional
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
    fn init(config: Vec<(String, String)>) -> Result<(), Fault> {
        // The host log sink is wasm-only; native unit tests skip it.
        if cfg!(not(test)) {
            install_tracing();
        }
        keeper::store_config(keeper::KeeperConfig::parse(&config)?)?;
        tracing::info!("ccow-monitor init");
        Ok(())
    }

    fn on_event(log: types::Log) -> Result<(), Fault> {
        keeper::on_event(&WitBindgenHost, &log.into())?;
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

#[cfg(test)]
mod tests {
    use alloy_primitives::address;

    use super::*;

    fn config_pairs(registry: &str) -> Vec<(String, String)> {
        vec![("registry".to_owned(), registry.to_owned())]
    }

    #[test]
    fn init_stores_the_configured_registry() {
        let registry = address!("abababababababababababababababababababab");
        TwapMonitor::init(config_pairs(&format!("{registry:#x}"))).expect("init succeeds");
        assert_eq!(
            keeper::stored_config(),
            Some(keeper::KeeperConfig { registry }),
        );
    }

    #[test]
    fn init_without_a_registry_is_a_hard_error() {
        let err = TwapMonitor::init(vec![]).expect_err("missing registry refuses init");
        assert!(matches!(err, Fault::InvalidInput(_)));
    }

    #[test]
    fn init_with_a_malformed_registry_is_a_hard_error() {
        let err =
            TwapMonitor::init(config_pairs("0xnope")).expect_err("malformed registry refuses init");
        assert!(matches!(err, Fault::InvalidInput(_)));
    }
}
