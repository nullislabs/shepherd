//! # echo-venue (reference Shepherd venue adapter)
//!
//! The minimal reference venue adapter: it echoes an intent body back as
//! its receipt and reports every receipt it issued as `open`. It carries
//! no real venue protocol, so it doubles as the smallest end-to-end
//! demonstration of `#[nexum_venue_sdk::venue]` - the attribute supplies
//! the per-cdylib wit-bindgen call for a world derived from `module.toml`,
//! the `Guest` export glue, and `export!`, leaving only the adapter face.
//!
//! It declares one capability (`chain`), so the built component imports
//! `nexum:host/chain` and nothing else: the per-component world matches
//! the manifest by construction.

// wit_bindgen::generate! expands to host-import shims whose arity matches
// the WIT signatures, which can exceed clippy's too-many-arguments threshold.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::too_many_arguments)]

use nexum::host::chain;
use nexum::intent::types::{IntentHeader, IntentStatus, SubmitOutcome, VenueError};
use nexum::value_flow::types::{Asset, AssetAmount, Settlement};

struct EchoVenue;

#[nexum_venue_sdk::venue]
impl EchoVenue {
    fn init(_config: Config) -> Result<(), Fault> {
        Ok(())
    }

    fn derive_header(body: Vec<u8>) -> Result<IntentHeader, VenueError> {
        // The echo venue gives back exactly the bytes handed to it, so the
        // header's `gives` amount is the body length: enough to exercise
        // the value-flow vocabulary without a real schema.
        Ok(IntentHeader {
            gives: vec![AssetAmount {
                asset: Asset::NativeToken(Settlement::EvmChain(1)),
                amount: (body.len() as u64).to_be_bytes().to_vec(),
            }],
            wants: Vec::new(),
            valid_until: None,
            settlement: Settlement::EvmChain(1),
            authorisation: nexum::intent::types::AuthScheme::Unsigned,
        })
    }

    fn submit(body: Vec<u8>) -> Result<SubmitOutcome, VenueError> {
        // Reading chain state on submit is what justifies the declared
        // `chain` capability; the block height is discarded, the point is
        // the scoped transport import the manifest declares.
        let _ = chain::request(1, "eth_blockNumber", "[]")
            .map_err(|_| VenueError::Unavailable("chain read failed".into()))?;
        Ok(SubmitOutcome::Accepted(body))
    }

    fn status(receipt: Vec<u8>) -> Result<IntentStatus, VenueError> {
        if receipt.is_empty() {
            Err(VenueError::InvalidReceipt)
        } else {
            Ok(IntentStatus::Open)
        }
    }

    fn cancel(receipt: Vec<u8>) -> Result<(), VenueError> {
        if receipt.is_empty() {
            Err(VenueError::InvalidReceipt)
        } else {
            Ok(())
        }
    }
}
