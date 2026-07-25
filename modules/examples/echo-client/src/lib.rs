//! # echo-client (reference Shepherd intent module)
//!
//! The keeper half of the echo pair. On every chain-1 block it quotes
//! and submits an opaque body through the raw `videre:venue/client`
//! import to the `echo-venue` adapter, logs the receipt, and logs each
//! `intent-status` transition the registry fans back. The smallest
//! demonstration of the intent core: module -> registry -> adapter and
//! the status event back.

// wit_bindgen::generate! expands to host-import shims whose arity matches
// the WIT signatures, which can exceed clippy's too-many-arguments threshold.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::too_many_arguments)]

use nexum::host::{logging, types};
use videre::types::types::SubmitOutcome;
use videre::venue::client;

/// Venue id the paired echo-venue adapter answers for.
const ECHO_VENUE: &str = "echo-venue";

struct EchoClient;

#[nexum_sdk::module]
impl EchoClient {
    fn on_block(block: types::Block) -> Result<(), Fault> {
        // The echo venue accepts any bytes and hands them back as the
        // receipt, so the body content is immaterial; the block number keeps
        // it non-empty and legible in the logs.
        let body = block.number.to_be_bytes().to_vec();
        match client::quote(ECHO_VENUE, &body) {
            Ok(quotation) => logging::log(
                logging::Level::Info,
                &format!(
                    "quoted {} bytes at {ECHO_VENUE}: gives {} amount bytes",
                    body.len(),
                    quotation.gives.amount.len(),
                ),
            ),
            Err(_) => logging::log(
                logging::Level::Warn,
                &format!("quote at {ECHO_VENUE} was refused"),
            ),
        }
        match client::submit(ECHO_VENUE, &body) {
            Ok(SubmitOutcome::Accepted(receipt)) => logging::log(
                logging::Level::Info,
                &format!(
                    "submitted {} bytes to {ECHO_VENUE}, receipt {} bytes",
                    body.len(),
                    receipt.len(),
                ),
            ),
            Ok(SubmitOutcome::RequiresSigning(_)) => logging::log(
                logging::Level::Warn,
                &format!("{ECHO_VENUE} unexpectedly asked for a signature"),
            ),
            Err(_) => logging::log(
                logging::Level::Warn,
                &format!("submit to {ECHO_VENUE} was refused"),
            ),
        }
        Ok(())
    }

    fn on_custom(event: types::CustomEvent) -> Result<(), Fault> {
        if event.kind != videre_sdk::status_body::INTENT_STATUS_KIND {
            return Ok(());
        }
        let update = videre_sdk::status_body::IntentStatusUpdate::decode(&event.payload)
            .map_err(|err| Fault::InvalidInput(err.to_string()))?;
        let body = videre_sdk::status_body::StatusBody::decode(&update.status)
            .map_err(|err| Fault::InvalidInput(err.to_string()))?;
        logging::log(
            logging::Level::Info,
            &format!(
                "intent status from venue {}: {:?} ({} receipt bytes)",
                update.venue,
                body.status,
                update.receipt.len(),
            ),
        );
        Ok(())
    }
}
