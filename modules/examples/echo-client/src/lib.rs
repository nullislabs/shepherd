//! # echo-client (reference Shepherd intent module)
//!
//! The keeper half of the echo pair. On every chain-1 block it submits an
//! opaque body through `videre:venue/client` to the `echo-venue` adapter and
//! logs the receipt, and it logs each `intent-status` transition the
//! registry fans back from that venue. Paired with the echo-venue adapter it
//! is the smallest end-to-end demonstration of the intent core: module ->
//! host registry -> venue adapter, and the status event back.
//!
//! It declares two capabilities (`client`, `logging`), so the built
//! component imports `videre:venue/client` and `nexum:host/logging` and
//! nothing else: the per-module world matches the manifest by construction.

// wit_bindgen::generate! expands to host-import shims whose arity matches
// the WIT signatures, which can exceed clippy's too-many-arguments threshold.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::too_many_arguments)]

use nexum::host::{logging, types};
use videre::types::types::SubmitOutcome;
use videre::venue::client;

/// Venue id the paired echo-venue adapter answers for; the module submits
/// to and observes exactly this venue.
const ECHO_VENUE: &str = "echo-venue";

struct EchoClient;

#[nexum_sdk::module]
impl EchoClient {
    fn on_block(block: types::Block) -> Result<(), Fault> {
        // The echo venue accepts any bytes and hands them back as the
        // receipt, so the body content is immaterial; the block number keeps
        // it non-empty and legible in the logs.
        let body = block.number.to_be_bytes().to_vec();
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

    fn on_intent_status(update: types::IntentStatusUpdate) -> Result<(), Fault> {
        let body = nexum_sdk::status_body::StatusBody::decode(&update.status)
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
