//! # example (reference Shepherd module)
//!
//! Minimal reference module: one handler per event, each logging a
//! one-line summary. The smallest demonstration of
//! `#[nexum_sdk::module]`, which supplies the wit-bindgen call, host
//! adapter, dispatch, and `export!`.

// wit_bindgen::generate! expands to host-import shims whose arity matches
// the WIT signatures, which can exceed clippy's too-many-arguments threshold.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::too_many_arguments)]

use nexum::host::{logging, types};

struct ExampleModule;

#[nexum_sdk::module]
impl ExampleModule {
    fn init(config: Vec<(String, String)>) -> Result<(), Fault> {
        let name = config
            .iter()
            .find(|(k, _)| k == "name")
            .map(|(_, v)| v.as_str())
            .unwrap_or("unknown");
        logging::log(
            logging::Level::Info,
            &format!("example module init (name={name})"),
        );
        Ok(())
    }

    fn on_block(block: types::Block) -> Result<(), Fault> {
        logging::log(
            logging::Level::Info,
            &format!(
                "block {} on chain {} (ts={}ms)",
                block.number, block.chain_id, block.timestamp
            ),
        );
        Ok(())
    }

    fn on_chain_logs(batch: types::ChainLogs) -> Result<(), Fault> {
        logging::log(
            logging::Level::Info,
            &format!("received {} chain-log entries", batch.logs.len()),
        );
        Ok(())
    }

    fn on_tick(tick: types::Tick) -> Result<(), Fault> {
        logging::log(
            logging::Level::Info,
            &format!("tick fired at {}ms", tick.fired_at),
        );
        Ok(())
    }

    fn on_message(msg: types::Message) -> Result<(), Fault> {
        logging::log(
            logging::Level::Info,
            &format!("message on topic {}", msg.content_topic),
        );
        Ok(())
    }

    fn on_custom(event: types::CustomEvent) -> Result<(), Fault> {
        logging::log(
            logging::Level::Info,
            &format!(
                "custom event kind {} ({} payload bytes)",
                event.kind,
                event.payload.len(),
            ),
        );
        Ok(())
    }
}
