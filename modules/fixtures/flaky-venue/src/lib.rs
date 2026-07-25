//! # flaky-venue (test fixture)
//!
//! Venue adapter whose `submit` panics (traps the store) while the chain
//! head reads the poison sentinel `0xdead`, and accepts once it moves
//! on. The test drives recovery: the supervisor's sweep must
//! reinstantiate the adapter before a submit succeeds. Test-only.

// wit_bindgen::generate! expands to host-import shims whose arity matches
// the WIT signatures, which can exceed clippy's too-many-arguments threshold.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::too_many_arguments)]

// `Config` and `Fault` come from the macro's world bindgen at the crate
// root: aliases of the SDK types, so the trait impl lines up.
use nexum::host::chain;
use videre_sdk::value_flow::{Asset, AssetAmount};
use videre_sdk::{
    AuthScheme, IntentHeader, IntentStatus, Quotation, Settlement, SubmitOutcome, VenueAdapter,
    VenueError,
};

/// The chain-head response that detonates `submit`.
const POISON_HEAD: &str = "0xdead";

struct FlakyVenue;

#[videre_sdk::venue]
impl VenueAdapter for FlakyVenue {
    fn init(_config: Config) -> Result<(), Fault> {
        Ok(())
    }

    fn derive_header(_body: Vec<u8>) -> Result<IntentHeader, VenueError> {
        Ok(IntentHeader {
            gives: zero_native(),
            wants: zero_native(),
            settlement: Settlement { chain: 1 },
            authorisation: AuthScheme::Eip1271,
        })
    }

    fn quote(_body: Vec<u8>) -> Result<Quotation, VenueError> {
        Ok(Quotation {
            gives: zero_native(),
            wants: zero_native(),
            fee: zero_native(),
            valid_until_ms: u64::MAX,
        })
    }

    fn submit(body: Vec<u8>) -> Result<SubmitOutcome, VenueError> {
        let head = chain::request(1, "eth_blockNumber", "[]")
            .map_err(|_| VenueError::Unavailable("chain read failed".into()))?;
        // The sentinel detonates the fixture: a guest panic traps the
        // store, which is what the sweep under test must recover from.
        assert!(!head.contains(POISON_HEAD), "flaky-venue poison head");
        Ok(SubmitOutcome::Accepted(body))
    }

    fn status(_receipt: Vec<u8>) -> Result<IntentStatus, VenueError> {
        Ok(IntentStatus::Open)
    }

    fn cancel(_receipt: Vec<u8>) -> Result<(), VenueError> {
        Ok(())
    }
}

/// A zero native amount.
fn zero_native() -> AssetAmount {
    AssetAmount {
        asset: Asset::Native,
        amount: Vec::new(),
    }
}
