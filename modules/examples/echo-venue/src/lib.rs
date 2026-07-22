//! # echo-venue (reference Shepherd venue adapter)
//!
//! The minimal reference venue adapter: it accepts any body, echoes it back
//! as the receipt, and settles instantly (every receipt it issued reports
//! `fulfilled`). It carries no real venue protocol, so it doubles as the
//! smallest end-to-end demonstration of `#[videre_sdk::venue]` - the
//! attribute takes the `impl VenueAdapter` block and supplies the
//! per-cdylib wit-bindgen for a world derived from `module.toml` plus the
//! export glue - and as the `nexum-venue-test` conformance target (see the
//! tests below).
//!
//! It declares one capability (`chain`), so the built component imports
//! `nexum:host/chain` and nothing else: the per-component world matches
//! the manifest by construction.

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

struct EchoVenue;

#[videre_sdk::venue]
impl VenueAdapter for EchoVenue {
    fn init(_config: Config) -> Result<(), Fault> {
        Ok(())
    }

    fn body_versions() -> Vec<u32> {
        // Must equal the manifest `[venue] body_versions`; install
        // asserts it.
        vec![1]
    }

    fn derive_header(body: Vec<u8>) -> Result<IntentHeader, VenueError> {
        // The echo venue gives back exactly the bytes handed to it, so the
        // header's `gives` amount is the body length: enough to exercise
        // the value-flow vocabulary without a real schema. Wants nothing,
        // spelled as a zero native amount.
        Ok(IntentHeader {
            gives: AssetAmount {
                asset: Asset::Native,
                amount: minimal_be(body.len() as u64),
            },
            wants: zero_native(),
            settlement: Settlement { chain: 1 },
            authorisation: AuthScheme::Eip1271,
        })
    }

    fn quote(body: Vec<u8>) -> Result<Quotation, VenueError> {
        // Echo pricing mirrors the header: gives the body length, wants
        // nothing, charges no fee, and the quote never expires.
        Ok(Quotation {
            gives: AssetAmount {
                asset: Asset::Native,
                amount: minimal_be(body.len() as u64),
            },
            wants: zero_native(),
            fee: zero_native(),
            valid_until_ms: u64::MAX,
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

    fn status(_receipt: Vec<u8>) -> Result<IntentStatus, VenueError> {
        // Settles instantly: the intent reaches a terminal state on the
        // first status poll.
        Ok(IntentStatus::Fulfilled)
    }

    fn cancel(_receipt: Vec<u8>) -> Result<(), VenueError> {
        Ok(())
    }
}

/// A zero native amount: the venue's spelling of "nothing".
fn zero_native() -> AssetAmount {
    AssetAmount {
        asset: Asset::Native,
        amount: Vec::new(),
    }
}

/// Big-endian bytes with leading zeros trimmed: the minimal `uint`
/// spelling, where an empty list is zero.
fn minimal_be(value: u64) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    let first = bytes.iter().position(|byte| *byte != 0);
    first.map_or(Vec::new(), |index| bytes[index..].to_vec())
}

/// echo-venue as the `nexum-venue-test` conformance target: the adapter's
/// pure header derivation is held to a hand-written golden. The macro
/// remaps the type interfaces onto the SDK bindings, so the derivation
/// feeds the kit directly through its `From<IntentHeader>` mirror.
#[cfg(test)]
mod conformance {
    use super::*;
    use nexum_venue_test::{
        FormatVersion, GoldenAsset, GoldenAssetAmount, GoldenAuthScheme, GoldenHeader,
        GoldenSettlement, HeaderGolden, HeaderGoldens,
    };

    fn zero_native() -> GoldenAssetAmount {
        GoldenAssetAmount {
            asset: GoldenAsset::Native,
            amount: Vec::new(),
        }
    }

    #[test]
    fn derive_header_conforms_to_the_published_golden() {
        // The echo contract: gives chain-1 native token whose amount is the
        // body length in minimal big-endian bytes, wants zero native, and
        // authorises via EIP-1271. A conforming adapter reproduces this
        // exactly.
        let golden = HeaderGolden {
            name: "four-byte-body".to_owned(),
            body: vec![1, 2, 3, 4],
            header: GoldenHeader {
                gives: GoldenAssetAmount {
                    asset: GoldenAsset::Native,
                    amount: vec![4],
                },
                wants: zero_native(),
                settlement: GoldenSettlement { chain: 1 },
                authorisation: GoldenAuthScheme::Eip1271,
            },
            notes: Some("amount is the minimal big-endian body length".to_owned()),
        };
        let goldens = HeaderGoldens {
            version: FormatVersion,
            venue: "echo-venue".to_owned(),
            goldens: vec![golden],
        };
        goldens.assert_conforms(EchoVenue::derive_header);
    }

    #[test]
    fn divergent_derivation_is_caught_by_the_golden() {
        // A non-minimal amount is the classic uint bug; the golden must
        // reject it, proving the check has teeth on echo-venue.
        let goldens = HeaderGoldens {
            version: FormatVersion,
            venue: "echo-venue".to_owned(),
            goldens: vec![HeaderGolden {
                name: "four-byte-body".to_owned(),
                body: vec![1, 2, 3, 4],
                header: GoldenHeader {
                    gives: GoldenAssetAmount {
                        asset: GoldenAsset::Native,
                        amount: 4u64.to_be_bytes().to_vec(),
                    },
                    wants: zero_native(),
                    settlement: GoldenSettlement { chain: 1 },
                    authorisation: GoldenAuthScheme::Eip1271,
                },
                notes: None,
            }],
        };
        assert!(goldens.check(EchoVenue::derive_header).is_err());
    }
}
