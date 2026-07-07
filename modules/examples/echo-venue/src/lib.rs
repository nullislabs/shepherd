//! # echo-venue (reference Shepherd venue adapter)
//!
//! The minimal reference venue adapter: it accepts any body, echoes it back
//! as the receipt, and settles instantly (every receipt it issued reports
//! `settled`). It carries no real venue protocol, so it doubles as the
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
            // Settles instantly: the intent reaches a terminal state on the
            // first status poll, with no venue-side settlement proof.
            Ok(IntentStatus::Settled(None))
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

/// echo-venue as the `nexum-venue-test` conformance target: the adapter's
/// pure header derivation is held to a hand-written golden through the kit's
/// serde mirror types. The macro mints echo-venue's own bindgen
/// `IntentHeader`, so the check bridges it to [`GoldenHeader`] field for
/// field - the pattern the kit documents for macro-built adapters - rather
/// than reusing the SDK's `From`.
#[cfg(test)]
mod conformance {
    use super::*;
    use nexum::intent::types::AuthScheme;
    use nexum_venue_test::{
        GoldenAsset, GoldenAssetAmount, GoldenAuthScheme, GoldenHeader, GoldenSettlement,
        HeaderGolden, HeaderGoldens,
    };

    fn settlement_to_golden(settlement: Settlement) -> GoldenSettlement {
        match settlement {
            Settlement::EvmChain(chain_id) => GoldenSettlement::EvmChain(chain_id),
            Settlement::Offchain(domain) => GoldenSettlement::Offchain(domain),
        }
    }

    fn asset_to_golden(asset: Asset) -> GoldenAsset {
        match asset {
            Asset::NativeToken(settlement) => {
                GoldenAsset::NativeToken(settlement_to_golden(settlement))
            }
            Asset::Erc20((chain_id, address)) => GoldenAsset::Erc20 { chain_id, address },
            Asset::Erc721((chain_id, address, token_id)) => GoldenAsset::Erc721 {
                chain_id,
                address,
                token_id,
            },
            Asset::Erc1155((chain_id, address, token_id)) => GoldenAsset::Erc1155 {
                chain_id,
                address,
                token_id,
            },
            Asset::Service(desc) => GoldenAsset::Service {
                kind: desc.kind,
                summary: desc.summary,
            },
            Asset::Offchain(desc) => GoldenAsset::Offchain {
                domain: desc.domain,
                summary: desc.summary,
            },
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
            AuthScheme::Eip712 => GoldenAuthScheme::Eip712,
            AuthScheme::Eip1271 => GoldenAuthScheme::Eip1271,
            AuthScheme::Presign => GoldenAuthScheme::Presign,
            AuthScheme::OffchainSig => GoldenAuthScheme::OffchainSig,
            AuthScheme::Unsigned => GoldenAuthScheme::Unsigned,
        }
    }

    fn header_to_golden(header: IntentHeader) -> GoldenHeader {
        GoldenHeader {
            gives: header.gives.into_iter().map(amount_to_golden).collect(),
            wants: header.wants.into_iter().map(amount_to_golden).collect(),
            valid_until: header.valid_until,
            settlement: settlement_to_golden(header.settlement),
            authorisation: auth_to_golden(header.authorisation),
        }
    }

    /// The adapter derivation the kit checks, bridged to the golden mirror.
    fn derive_golden(body: Vec<u8>) -> Result<GoldenHeader, VenueError> {
        EchoVenue::derive_header(body).map(header_to_golden)
    }

    #[test]
    fn derive_header_conforms_to_the_published_golden() {
        // The echo contract: gives chain-1 native token whose amount is the
        // body length as eight big-endian bytes, wants nothing, and carries
        // no authorisation. A conforming adapter reproduces this exactly.
        let golden = HeaderGolden {
            name: "four-byte-body".to_owned(),
            body: vec![1, 2, 3, 4],
            header: GoldenHeader {
                gives: vec![GoldenAssetAmount {
                    asset: GoldenAsset::NativeToken(GoldenSettlement::EvmChain(1)),
                    amount: 4u64.to_be_bytes().to_vec(),
                }],
                wants: Vec::new(),
                valid_until: None,
                settlement: GoldenSettlement::EvmChain(1),
                authorisation: GoldenAuthScheme::Unsigned,
            },
            notes: Some("amount is the 8-byte big-endian body length".to_owned()),
        };
        let goldens = HeaderGoldens {
            venue: "echo-venue".to_owned(),
            goldens: vec![golden],
        };
        goldens.assert_conforms(derive_golden);
    }

    #[test]
    fn divergent_derivation_is_caught_by_the_golden() {
        // A little-endian amount is the classic byte-order bug; the golden
        // must reject it, proving the check has teeth on echo-venue.
        let goldens = HeaderGoldens {
            venue: "echo-venue".to_owned(),
            goldens: vec![HeaderGolden {
                name: "four-byte-body".to_owned(),
                body: vec![1, 2, 3, 4],
                header: GoldenHeader {
                    gives: vec![GoldenAssetAmount {
                        asset: GoldenAsset::NativeToken(GoldenSettlement::EvmChain(1)),
                        amount: 4u64.to_le_bytes().to_vec(),
                    }],
                    wants: Vec::new(),
                    valid_until: None,
                    settlement: GoldenSettlement::EvmChain(1),
                    authorisation: GoldenAuthScheme::Unsigned,
                },
                notes: None,
            }],
        };
        assert!(goldens.check(derive_golden).is_err());
    }
}
