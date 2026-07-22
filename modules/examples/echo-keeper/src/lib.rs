//! # echo-keeper (reference videre keeper module)
//!
//! The blessed keeper half of the echo pair: on every chain-1 block it
//! drives the echo-venue adapter through the typed
//! `VenueClient<EchoVenue>` - quote, submit, status, cancel, all with a
//! typed body - and logs each `intent-status` transition the registry
//! fans back. Where echo-client hand-writes byte marshalling over the
//! raw `videre:venue/client` import, this module is
//! `#[videre_sdk::keeper]`: the macro wires the world and the client
//! import, and the author never sees wire bytes.
//!
//! It declares two capabilities (`client`, `logging`), so the built
//! component imports `videre:venue/client` and `nexum:host/logging` and
//! nothing else: the per-module world matches the manifest by
//! construction.

// wit_bindgen::generate! expands to host-import shims whose arity matches
// the WIT signatures, which can exceed clippy's too-many-arguments threshold.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::too_many_arguments)]

use nexum::host::{logging, types};
use videre_sdk::{SubmitOutcome, Venue, VenueClient, VenueId};

/// The echo venue as this keeper types it: the id the paired adapter
/// answers for and the body schema below.
struct EchoVenue;

impl Venue for EchoVenue {
    const ID: VenueId = VenueId::from_static("echo-venue");
    type Body = EchoBody;
}

/// The keeper's published body schema. The echo venue accepts any
/// bytes, so v1 is just the block number: enough to exercise the typed
/// codec end to end.
#[derive(videre_sdk::IntentBody)]
enum EchoBody {
    V1(u64),
}

struct EchoKeeper;

#[videre_sdk::keeper]
impl EchoKeeper {
    async fn on_block(block: types::Block) -> Result<(), Fault> {
        let venue = VenueClient::<EchoVenue>::new();
        let body = EchoBody::V1(block.number);

        // Quote-then-submit through the typestate: the venue prices
        // exactly the bytes it is later handed. ClientError folds into
        // the wire fault, so `?` applies throughout.
        let quoted = venue.quote(&body).await?;
        logging::log(
            logging::Level::Info,
            &format!(
                "quoted at {}: gives {} amount bytes",
                EchoVenue::ID,
                quoted.quotation().gives.amount.len(),
            ),
        );
        let receipt = match quoted.submit().await? {
            SubmitOutcome::Accepted(receipt) => receipt,
            SubmitOutcome::RequiresSigning(_) => {
                logging::log(
                    logging::Level::Warn,
                    &format!("{} unexpectedly asked for a signature", EchoVenue::ID),
                );
                return Ok(());
            }
        };
        logging::log(
            logging::Level::Info,
            &format!(
                "submitted to {}: receipt {} bytes",
                EchoVenue::ID,
                receipt.len(),
            ),
        );

        let status = venue.status(&receipt).await?;
        logging::log(
            logging::Level::Info,
            &format!("status at {}: {status:?}", EchoVenue::ID),
        );

        venue.cancel(&receipt).await?;
        logging::log(
            logging::Level::Info,
            &format!("cancelled at {}", EchoVenue::ID),
        );
        Ok(())
    }

    fn on_intent_status(update: videre_sdk::IntentStatusUpdate) -> Result<(), Fault> {
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
