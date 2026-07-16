//! # echo-venue (reference Shepherd venue adapter)
//!
//! The minimal reference venue adapter: it accepts any body, echoes it back
//! as the receipt, and settles instantly (every receipt it issued reports
//! `fulfilled`). It carries no real venue protocol, so it doubles as the
//! smallest end-to-end demonstration of `#[nexum_venue_sdk::venue]` - the
//! attribute supplies the per-cdylib wit-bindgen call for a world derived
//! from `module.toml`, the `Guest` export glue, and `export!`, leaving only
//! the adapter face - and as the `nexum-venue-test` conformance target (see
//! the tests below).
//!
//! It declares one capability (`chain`), so the built component imports
//! `nexum:host/chain` and nothing else: the per-component world matches
//! the manifest by construction.

// wit_bindgen::generate! expands to host-import shims whose arity matches
// the WIT signatures, which can exceed clippy's too-many-arguments threshold.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::too_many_arguments)]

use nexum::host::chain;
use videre::types::types::{
    AuthScheme, IntentHeader, IntentStatus, Settlement, SubmitOutcome, VenueError,
};
use videre::value_flow::types::{Asset, AssetAmount};

struct EchoVenue;

#[nexum_venue_sdk::venue]
impl EchoVenue {
    fn init(_config: Config) -> Result<(), Fault> {
        Ok(())
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
            wants: AssetAmount {
                asset: Asset::Native,
                amount: Vec::new(),
            },
            settlement: Settlement { chain: 1 },
            authorisation: AuthScheme::Eip1271,
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

/// Big-endian bytes with leading zeros trimmed: the minimal `uint`
/// spelling, where an empty list is zero.
fn minimal_be(value: u64) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    let first = bytes.iter().position(|byte| *byte != 0);
    first.map_or(Vec::new(), |index| bytes[index..].to_vec())
}

/// echo-venue as the `nexum-venue-test` conformance target: the adapter's
/// pure header derivation is held to a hand-written golden through the kit's
/// serde mirror types. The macro mints echo-venue's own bindgen
/// `IntentHeader`, so the check bridges it to [`GoldenHeader`] field for
/// field - the pattern the kit documents for macro-built adapters - rather
/// than reusing the SDK's `From`.
#[cfg(test)]
mod conformance {
    use super::*;
    use nexum_venue_test::{
        GoldenAsset, GoldenAssetAmount, GoldenAuthScheme, GoldenHeader, GoldenSettlement,
        HeaderGolden, HeaderGoldens,
    };

    fn asset_to_golden(asset: Asset) -> GoldenAsset {
        match asset {
            Asset::Native => GoldenAsset::Native,
            Asset::Erc20(erc20) => GoldenAsset::Erc20 { token: erc20.token },
        }
    }

    fn amount_to_golden(amount: AssetAmount) -> GoldenAssetAmount {
        GoldenAssetAmount {
            asset: asset_to_golden(amount.asset),
            amount: amount.amount,
        }
    }

    fn auth_to_golden(scheme: AuthScheme) -> GoldenAuthScheme {
        match scheme {
            AuthScheme::Eip1271 => GoldenAuthScheme::Eip1271,
            AuthScheme::Eip712 => GoldenAuthScheme::Eip712,
        }
    }

    fn header_to_golden(header: IntentHeader) -> GoldenHeader {
        GoldenHeader {
            gives: amount_to_golden(header.gives),
            wants: amount_to_golden(header.wants),
            settlement: GoldenSettlement {
                chain: header.settlement.chain,
            },
            authorisation: auth_to_golden(header.authorisation),
        }
    }

    /// The adapter derivation the kit checks, bridged to the golden mirror.
    fn derive_golden(body: Vec<u8>) -> Result<GoldenHeader, VenueError> {
        EchoVenue::derive_header(body).map(header_to_golden)
    }

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
            venue: "echo-venue".to_owned(),
            goldens: vec![golden],
        };
        goldens.assert_conforms(derive_golden);
    }

    #[test]
    fn divergent_derivation_is_caught_by_the_golden() {
        // A non-minimal amount is the classic uint bug; the golden must
        // reject it, proving the check has teeth on echo-venue.
        let goldens = HeaderGoldens {
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
        assert!(goldens.check(derive_golden).is_err());
    }
}
