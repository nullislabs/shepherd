//! Header-derivation goldens: the file format that publishes what
//! `derive-header` must project from each published body, and the
//! check that holds an adapter to it.
//!
//! A golden file pairs wire bodies with the intent header a conforming
//! adapter derives from them, spelled in the golden mirror types below
//! (JSON, kebab-case case names matching the WIT, bytes as lowercase
//! hex). The mirrors exist because wit-bindgen types carry no serde;
//! [`GoldenHeader`] converts from the venue SDK's `IntentHeader`, and a
//! macro-built adapter whose bindgen mints its own header type bridges
//! with a field-for-field `From` impl on its crate boundary, the same
//! pattern `nexum-sdk-test` documents for `Fault`.

use std::fmt;
use std::path::Path;

use nexum_venue_sdk::value_flow::{Asset, AssetAmount, Settlement};
use nexum_venue_sdk::{AuthScheme, IntentHeader};
use serde::{Deserialize, Serialize};

use crate::fixture::{self, FixtureError, hex_bytes};
use crate::report::{ConformanceReport, Violation, settle};

/// A published set of header goldens for one venue.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeaderGoldens {
    /// The venue the goldens bind. Informational: the check never
    /// reads it.
    pub venue: String,
    /// The goldens, in publication order.
    pub goldens: Vec<HeaderGolden>,
}

/// One wire body and the header a conforming adapter derives from it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeaderGolden {
    /// Stable name a violation is reported under.
    pub name: String,
    /// The intent body, lowercase hex in the file.
    #[serde(with = "hex_bytes")]
    pub body: Vec<u8>,
    /// The header `derive-header` must produce for the body.
    pub header: GoldenHeader,
    /// Optional prose for readers of the file; the check ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// Serde mirror of the wire `intent-header`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct GoldenHeader {
    /// Value leaving the user's control.
    pub gives: Vec<GoldenAssetAmount>,
    /// Value expected in return.
    pub wants: Vec<GoldenAssetAmount>,
    /// Expiry in milliseconds since the Unix epoch, UTC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<u64>,
    /// Where the deal settles.
    pub settlement: GoldenSettlement,
    /// How the venue authorizes the intent.
    pub authorisation: GoldenAuthScheme,
}

/// Serde mirror of the wire `asset-amount`. `amount` is big-endian
/// unsigned, hex in the file; an empty string is zero.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenAssetAmount {
    /// The asset moving.
    pub asset: GoldenAsset,
    /// Big-endian unsigned amount bytes.
    #[serde(with = "hex_bytes")]
    pub amount: Vec<u8>,
}

/// Serde mirror of the wire `settlement`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum GoldenSettlement {
    /// Settles on an EVM chain, by chain id.
    EvmChain(u64),
    /// Settles off-chain in the named domain.
    Offchain(String),
}

/// Serde mirror of the wire `asset`. Token addresses and ids are hex
/// in the file.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    rename_all = "kebab-case",
    rename_all_fields = "kebab-case",
    deny_unknown_fields
)]
pub enum GoldenAsset {
    /// The settlement domain's own gas token.
    NativeToken(GoldenSettlement),
    /// An ERC-20 token.
    Erc20 {
        /// Chain the token lives on.
        chain_id: u64,
        /// 20-byte contract address.
        #[serde(with = "hex_bytes")]
        address: Vec<u8>,
    },
    /// An ERC-721 NFT.
    Erc721 {
        /// Chain the token lives on.
        chain_id: u64,
        /// 20-byte contract address.
        #[serde(with = "hex_bytes")]
        address: Vec<u8>,
        /// Token id, big-endian, arbitrary width.
        #[serde(with = "hex_bytes")]
        token_id: Vec<u8>,
    },
    /// An ERC-1155 token.
    Erc1155 {
        /// Chain the token lives on.
        chain_id: u64,
        /// 20-byte contract address.
        #[serde(with = "hex_bytes")]
        address: Vec<u8>,
        /// Token id, big-endian, arbitrary width.
        #[serde(with = "hex_bytes")]
        token_id: Vec<u8>,
    },
    /// A non-token service obligation.
    Service {
        /// Namespaced service kind, e.g. `swarm:postage`.
        kind: String,
        /// Human-readable description for the consent sheet.
        summary: String,
    },
    /// A real-world asset settled off-chain.
    Offchain {
        /// Jurisdiction or registry domain.
        domain: String,
        /// Human-readable description for the consent sheet.
        summary: String,
    },
}

/// Serde mirror of the wire `auth-scheme`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GoldenAuthScheme {
    /// EIP-712 typed-data signature by host-held keys.
    Eip712,
    /// EIP-1271 contract signature.
    Eip1271,
    /// Pre-signed authorization at the settlement contract.
    Presign,
    /// Venue-defined off-chain signature scheme.
    OffchainSig,
    /// No authorization travels with the body.
    Unsigned,
}

impl From<IntentHeader> for GoldenHeader {
    fn from(header: IntentHeader) -> Self {
        Self {
            gives: header.gives.into_iter().map(Into::into).collect(),
            wants: header.wants.into_iter().map(Into::into).collect(),
            valid_until: header.valid_until,
            settlement: header.settlement.into(),
            authorisation: header.authorisation.into(),
        }
    }
}

impl From<AssetAmount> for GoldenAssetAmount {
    fn from(amount: AssetAmount) -> Self {
        Self {
            asset: amount.asset.into(),
            amount: amount.amount,
        }
    }
}

impl From<Settlement> for GoldenSettlement {
    fn from(settlement: Settlement) -> Self {
        match settlement {
            Settlement::EvmChain(chain_id) => GoldenSettlement::EvmChain(chain_id),
            Settlement::Offchain(domain) => GoldenSettlement::Offchain(domain),
        }
    }
}

impl From<Asset> for GoldenAsset {
    fn from(asset: Asset) -> Self {
        match asset {
            Asset::NativeToken(settlement) => GoldenAsset::NativeToken(settlement.into()),
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
}

impl From<AuthScheme> for GoldenAuthScheme {
    fn from(scheme: AuthScheme) -> Self {
        match scheme {
            AuthScheme::Eip712 => GoldenAuthScheme::Eip712,
            AuthScheme::Eip1271 => GoldenAuthScheme::Eip1271,
            AuthScheme::Presign => GoldenAuthScheme::Presign,
            AuthScheme::OffchainSig => GoldenAuthScheme::OffchainSig,
            AuthScheme::Unsigned => GoldenAuthScheme::Unsigned,
        }
    }
}

impl HeaderGoldens {
    /// An empty golden set for `venue`.
    pub fn new(venue: impl Into<String>) -> Self {
        Self {
            venue: venue.into(),
            goldens: Vec::new(),
        }
    }

    /// Append a golden by running the publishing adapter's own
    /// `derive-header` on `body`. Returns the pushed golden so the
    /// caller can attach [`notes`](HeaderGolden::notes).
    pub fn record<H, E>(
        &mut self,
        name: impl Into<String>,
        body: Vec<u8>,
        derive: impl FnOnce(Vec<u8>) -> Result<H, E>,
    ) -> Result<&mut HeaderGolden, E>
    where
        H: Into<GoldenHeader>,
    {
        let header = derive(body.clone())?.into();
        self.goldens.push(HeaderGolden {
            name: name.into(),
            body,
            header,
            notes: None,
        });
        Ok(self.goldens.last_mut().expect("golden was just pushed"))
    }

    /// Parse a golden set from its JSON text.
    pub fn from_json(json: &str) -> Result<Self, FixtureError> {
        fixture::from_json(json)
    }

    /// The canonical published form: pretty JSON, trailing newline.
    pub fn to_json(&self) -> String {
        fixture::to_json(self)
    }

    /// Load a golden file from disk.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, FixtureError> {
        fixture::load(path.as_ref())
    }

    /// Write the golden file in its canonical published form.
    pub fn write(&self, path: impl AsRef<Path>) -> Result<(), FixtureError> {
        fixture::write(path.as_ref(), self)
    }

    /// Check an adapter's `derive-header` against every golden,
    /// collecting all violations rather than stopping at the first.
    ///
    /// `derive` is the adapter's derivation; a trait-based adapter
    /// passes `MyAdapter::derive_header` directly.
    pub fn check<H, E>(
        &self,
        mut derive: impl FnMut(Vec<u8>) -> Result<H, E>,
    ) -> Result<(), ConformanceReport>
    where
        H: Into<GoldenHeader>,
        E: fmt::Debug,
    {
        let mut violations = Vec::new();
        for golden in &self.goldens {
            match derive(golden.body.clone()) {
                Ok(header) => {
                    let derived: GoldenHeader = header.into();
                    if derived != golden.header {
                        violations.push(Violation {
                            vector: golden.name.clone(),
                            detail: format!(
                                "derived header diverges from the golden: expected {:?}, derived {derived:?}",
                                golden.header,
                            ),
                        });
                    }
                }
                Err(err) => violations.push(Violation {
                    vector: golden.name.clone(),
                    detail: format!("derive-header failed: {err:?}"),
                }),
            }
        }
        settle(violations)
    }

    /// [`check`](Self::check), panicking with the full report on any
    /// violation. The assertion form for adapter test suites.
    pub fn assert_conforms<H, E>(&self, derive: impl FnMut(Vec<u8>) -> Result<H, E>)
    where
        H: Into<GoldenHeader>,
        E: fmt::Debug,
    {
        if let Err(report) = self.check(derive) {
            panic!(
                "derive-header does not conform to the {} goldens:\n{report}",
                self.venue,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use nexum_venue_sdk::VenueError;
    use nexum_venue_sdk::value_flow::{OffchainDesc, ServiceDesc};

    use super::*;

    fn wire_header() -> IntentHeader {
        IntentHeader {
            gives: vec![
                AssetAmount {
                    asset: Asset::NativeToken(Settlement::EvmChain(100)),
                    amount: vec![0x0d, 0xe0, 0xb6],
                },
                AssetAmount {
                    asset: Asset::Erc20((1, vec![0xAA; 20])),
                    amount: vec![1, 0],
                },
                AssetAmount {
                    asset: Asset::Erc721((1, vec![0xBB; 20], vec![7])),
                    amount: vec![1],
                },
                AssetAmount {
                    asset: Asset::Erc1155((1, vec![0xCC; 20], vec![8])),
                    amount: vec![2],
                },
                AssetAmount {
                    asset: Asset::Service(ServiceDesc {
                        kind: "swarm:postage".to_owned(),
                        summary: "storage for 30 days".to_owned(),
                    }),
                    amount: Vec::new(),
                },
            ],
            wants: vec![AssetAmount {
                asset: Asset::Offchain(OffchainDesc {
                    domain: "iso:AU".to_owned(),
                    summary: "a deed".to_owned(),
                }),
                amount: Vec::new(),
            }],
            valid_until: Some(1_700_000_000_000),
            settlement: Settlement::Offchain("acme".to_owned()),
            authorisation: AuthScheme::OffchainSig,
        }
    }

    #[test]
    fn golden_mirror_covers_every_wire_case_and_round_trips_as_json() {
        let golden: GoldenHeader = wire_header().into();
        let goldens = HeaderGoldens {
            venue: "acme".to_owned(),
            goldens: vec![HeaderGolden {
                name: "kitchen-sink".to_owned(),
                body: vec![0],
                header: golden,
                notes: None,
            }],
        };
        let json = goldens.to_json();
        assert_eq!(HeaderGoldens::from_json(&json).unwrap(), goldens);
        // The wire spellings are the contract for non-Rust readers.
        assert!(json.contains("\"native-token\""));
        assert!(json.contains("\"chain-id\""));
        assert!(json.contains("\"token-id\""));
        assert!(json.contains("\"valid-until\""));
        assert!(json.contains("\"offchain-sig\""));
        assert!(json.contains("\"evm-chain\""));
    }

    #[test]
    fn conforming_derivation_passes() {
        let mut goldens = HeaderGoldens::new("acme");
        goldens
            .record("kitchen-sink", vec![1, 2, 3], |_| {
                Ok::<_, VenueError>(wire_header())
            })
            .unwrap();
        goldens
            .check(|_| Ok::<_, VenueError>(wire_header()))
            .unwrap();
    }

    #[test]
    fn diverging_derivation_and_failure_are_both_violations() {
        let mut goldens = HeaderGoldens::new("acme");
        goldens
            .record("a", vec![1], |_| Ok::<_, VenueError>(wire_header()))
            .unwrap();
        goldens
            .record("b", vec![2], |_| Ok::<_, VenueError>(wire_header()))
            .unwrap();

        let mut calls = 0;
        let report = goldens
            .check(|_| {
                calls += 1;
                if calls == 1 {
                    let mut header = wire_header();
                    header.valid_until = None;
                    Ok(header)
                } else {
                    Err(VenueError::InvalidBody("nope".to_owned()))
                }
            })
            .unwrap_err();

        assert_eq!(report.violations.len(), 2);
        assert_eq!(report.violations[0].vector, "a");
        assert!(report.violations[0].detail.contains("diverges"));
        assert_eq!(report.violations[1].vector, "b");
        assert!(report.violations[1].detail.contains("derive-header failed"));
    }

    #[test]
    #[should_panic(expected = "derive-header does not conform")]
    fn assert_conforms_panics_with_the_report() {
        let mut goldens = HeaderGoldens::new("acme");
        goldens
            .record("a", vec![1], |_| Ok::<_, VenueError>(wire_header()))
            .unwrap();
        goldens.assert_conforms(|_| Err::<IntentHeader, _>(VenueError::InvalidReceipt));
    }
}
