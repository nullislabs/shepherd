//! The kit's reference venue: a published body schema, its codec
//! vector file, and its header golden file.
//!
//! The reference exists so the fixture formats ship with a worked,
//! machine-checked example. Its payloads exercise every borsh
//! primitive a body schema is likely to carry (fixed-width integers,
//! length-prefixed strings and byte vectors, options, bools), so a
//! non-Rust author can prove their borsh implementation byte-exact
//! against [`CODEC_VECTORS_JSON`] before touching their own schema.
//! The published files are pinned by this crate's tests: regeneration
//! must reproduce them byte for byte.

use borsh::{BorshDeserialize, BorshSerialize};
use nexum_venue_sdk::value_flow::{Asset, AssetAmount, Erc20};
use nexum_venue_sdk::{AuthScheme, IntentBody, IntentHeader, Settlement, VenueError};

/// The published codec vector file, verbatim.
pub const CODEC_VECTORS_JSON: &str = include_str!("../vectors/reference-body.json");

/// The published header golden file, verbatim.
pub const HEADER_GOLDENS_JSON: &str = include_str!("../goldens/reference-header.json");

/// First published version: a fixed-price quote.
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, Eq, PartialEq)]
pub struct ReferenceV1 {
    /// Amount in wei; borsh encodes it as 8 little-endian bytes.
    pub amount_wei: u64,
    /// Free text; borsh encodes a u32 little-endian byte length then
    /// the UTF-8 bytes.
    pub memo: String,
}

/// Second published version: v1 plus an expiry, a recipient, and a
/// priority flag.
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, Eq, PartialEq)]
pub struct ReferenceV2 {
    /// Amount in wei.
    pub amount_wei: u64,
    /// Free text.
    pub memo: String,
    /// Expiry in ms since the Unix epoch, UTC; borsh encodes a one-byte
    /// presence tag (0 absent, 1 present) then the payload.
    pub valid_until_ms: Option<u64>,
    /// 20-byte recipient address; borsh encodes a u32 little-endian
    /// element count then the bytes.
    pub recipient: Vec<u8>,
    /// Priority flag; borsh encodes one byte (0 false, 1 true).
    pub urgent: bool,
}

/// The reference venue's outer version enum. Tag order is the schema:
/// versions append, never reorder.
#[derive(IntentBody, Clone, Debug, Eq, PartialEq)]
pub enum ReferenceBody {
    /// Version 1, wire tag 0.
    V1(ReferenceV1),
    /// Version 2, wire tag 1.
    V2(ReferenceV2),
}

/// The reference venue's pure header derivation, the subject the
/// published goldens pin. Gives the amount as the chain's native token,
/// wants (for v2) the same amount as an ERC-20 at the recipient token
/// address, and authorises via EIP-712. V1 wants nothing, spelled as a
/// zero native amount.
pub fn derive_reference_header(body: Vec<u8>) -> Result<IntentHeader, VenueError> {
    let (amount_wei, wants) = match ReferenceBody::from_bytes(&body)? {
        ReferenceBody::V1(quote) => (
            quote.amount_wei,
            AssetAmount {
                asset: Asset::Native,
                amount: Vec::new(),
            },
        ),
        ReferenceBody::V2(quote) => (
            quote.amount_wei,
            AssetAmount {
                asset: Asset::Erc20(Erc20 {
                    token: quote.recipient,
                }),
                amount: minimal_be(quote.amount_wei),
            },
        ),
    };
    Ok(IntentHeader {
        gives: AssetAmount {
            asset: Asset::Native,
            amount: minimal_be(amount_wei),
        },
        wants,
        settlement: Settlement { chain: 1 },
        authorisation: AuthScheme::Eip712,
    })
}

/// Big-endian bytes with leading zeros trimmed: the minimal spelling
/// of a wire amount, where an empty list is zero.
fn minimal_be(value: u64) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    let first = bytes.iter().position(|byte| *byte != 0);
    first.map_or(Vec::new(), |index| bytes[index..].to_vec())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::codec::{CodecVectors, Expectation};
    use crate::header::HeaderGoldens;

    use super::*;

    fn v1_small() -> ReferenceBody {
        ReferenceBody::V1(ReferenceV1 {
            amount_wei: 1,
            memo: "gm".to_owned(),
        })
    }

    fn v2_full() -> ReferenceBody {
        ReferenceBody::V2(ReferenceV2 {
            amount_wei: 1_000_000,
            memo: "two coffees".to_owned(),
            valid_until_ms: Some(1_700_000_000_000),
            recipient: (1..=20).collect(),
            urgent: true,
        })
    }

    /// Rebuild the published codec vectors from the reference schema.
    fn build_codec_vectors() -> CodecVectors {
        let mut vectors = CodecVectors::new("nexum-venue-test/reference-body");

        vectors
            .push_round_trip("v1-small", &v1_small())
            .unwrap()
            .notes = Some(
            "tag 0x00, amount_wei 1 as 8 little-endian bytes, memo as u32 \
             little-endian length then utf-8 bytes"
                .to_owned(),
        );
        vectors
            .push_round_trip(
                "v1-zero-and-empty",
                &ReferenceBody::V1(ReferenceV1 {
                    amount_wei: 0,
                    memo: String::new(),
                }),
            )
            .unwrap()
            .notes = Some("zero integer and zero-length string".to_owned());
        vectors
            .push_round_trip(
                "v1-max-amount",
                &ReferenceBody::V1(ReferenceV1 {
                    amount_wei: u64::MAX,
                    memo: "max".to_owned(),
                }),
            )
            .unwrap()
            .notes = Some("endianness proof: u64::MAX is eight 0xff bytes".to_owned());
        vectors
            .push_round_trip("v2-full", &v2_full())
            .unwrap()
            .notes = Some(
            "tag 0x01; option present is 0x01 then the payload, vec is u32 \
             little-endian element count then bytes, bool true is 0x01"
                .to_owned(),
        );
        vectors
            .push_round_trip(
                "v2-no-expiry",
                &ReferenceBody::V2(ReferenceV2 {
                    amount_wei: 5,
                    memo: "later".to_owned(),
                    valid_until_ms: None,
                    recipient: vec![0xAA; 20],
                    urgent: false,
                }),
            )
            .unwrap()
            .notes = Some("option absent is a bare 0x00, bool false is 0x00".to_owned());

        vectors
            .push_failure("empty-body", Vec::new(), Expectation::Empty)
            .notes = Some("no version tag at all".to_owned());
        let mut unknown = v1_small().to_bytes().unwrap();
        unknown[0] = 9;
        vectors
            .push_failure(
                "unknown-version",
                unknown,
                Expectation::UnknownVersion { version: 9 },
            )
            .notes = Some("tag 0x09 names no published version".to_owned());
        let mut truncated = v2_full().to_bytes().unwrap();
        truncated.truncate(truncated.len() - 1);
        vectors
            .push_failure(
                "truncated-payload",
                truncated,
                Expectation::Malformed { version: 1 },
            )
            .notes = Some("known tag, payload cut one byte short".to_owned());
        let mut trailing = v1_small().to_bytes().unwrap();
        trailing.push(0);
        vectors
            .push_failure(
                "trailing-bytes",
                trailing,
                Expectation::Malformed { version: 0 },
            )
            .notes = Some("decoding must consume the payload exactly".to_owned());

        vectors
    }

    /// Rebuild the published header goldens from the reference
    /// derivation.
    fn build_header_goldens() -> HeaderGoldens {
        let mut goldens = HeaderGoldens::new("nexum-venue-test/reference");
        goldens
            .record(
                "v1-small",
                v1_small().to_bytes().unwrap(),
                derive_reference_header,
            )
            .unwrap()
            .notes = Some("gives chain-1 native token, minimal big-endian amount".to_owned());
        goldens
            .record(
                "v2-full",
                v2_full().to_bytes().unwrap(),
                derive_reference_header,
            )
            .unwrap()
            .notes = Some("v2 adds an erc20 want at the recipient token address".to_owned());
        goldens
    }

    #[test]
    fn published_codec_vectors_match_regeneration() {
        assert_eq!(
            CODEC_VECTORS_JSON,
            build_codec_vectors().to_json(),
            "vectors/reference-body.json has drifted; run the ignored \
             regenerate_reference_fixtures test and commit the result",
        );
    }

    #[test]
    fn published_header_goldens_match_regeneration() {
        assert_eq!(
            HEADER_GOLDENS_JSON,
            build_header_goldens().to_json(),
            "goldens/reference-header.json has drifted; run the ignored \
             regenerate_reference_fixtures test and commit the result",
        );
    }

    /// Rewrite the published files from the reference schema. Run with
    /// `cargo test -p nexum-venue-test -- --ignored regenerate` after a
    /// deliberate schema change, then commit the diff; the tests above
    /// compare against the compiled-in copy, so they go green on the
    /// next build.
    #[test]
    #[ignore = "writes the published fixture files in place"]
    fn regenerate_reference_fixtures() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        build_codec_vectors()
            .write(root.join("vectors/reference-body.json"))
            .unwrap();
        build_header_goldens()
            .write(root.join("goldens/reference-header.json"))
            .unwrap();
    }
}
