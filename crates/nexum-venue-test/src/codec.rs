//! Codec conformance vectors: the file format that publishes a venue's
//! `IntentBody` wire bytes, and the check that holds a codec to them.
//!
//! A vector file is the venue's codec contract in portable form: JSON,
//! a leading format version (unknown versions fail closed), bytes as
//! lowercase hex, one entry per published body, never zero. A non-Rust
//! adapter author proves byte-exactness by decoding and re-encoding
//! each `round-trip` vector in their own language and comparing bytes;
//! a Rust author runs [`CodecVectors::assert_conforms`] against the
//! derived enum. The failure vectors pin the typed error contract:
//! empty, unknown-version, and malformed bodies must fail exactly as
//! [`BodyError`] names them, not garble into a decoded value.

use std::path::Path;

use nexum_venue_sdk::{BodyError, IntentBody};
use serde::{Deserialize, Serialize};

use crate::fixture::{self, FixtureError, FormatVersion, hex_bytes};
use crate::report::{ConformanceReport, Violation, settle};

/// A published set of codec vectors for one venue body schema.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodecVectors {
    /// File-format discriminator; an unknown version fails to parse.
    pub version: FormatVersion,
    /// Name of the body schema the vectors bind, e.g.
    /// `acme-dex/order-body`. Informational: the check never reads it.
    pub schema: String,
    /// The vectors, in publication order. Never empty in a parsed
    /// file: an empty set would conform vacuously.
    #[serde(deserialize_with = "fixture::non_empty")]
    pub vectors: Vec<CodecVector>,
}

/// One published wire body and the outcome its bytes must produce.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodecVector {
    /// Stable name a violation is reported under.
    pub name: String,
    /// The wire bytes, lowercase hex in the file.
    #[serde(with = "hex_bytes")]
    pub bytes: Vec<u8>,
    /// What a conforming codec does with the bytes.
    pub expect: Expectation,
    /// Optional prose for readers of the file; the check ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// The outcome a vector demands of a conforming codec. The failure
/// cases mirror [`BodyError`] minus its free-text detail: the detail
/// wording is the Rust implementation's, not part of the contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Expectation {
    /// The bytes decode, and re-encoding the decoded value reproduces
    /// them exactly.
    RoundTrip,
    /// Decoding fails: no version tag at all.
    Empty,
    /// Decoding fails: the tag names no published version.
    UnknownVersion {
        /// The unknown wire tag.
        version: u8,
    },
    /// Decoding fails: a known tag whose payload does not parse.
    Malformed {
        /// The wire tag whose payload is broken.
        version: u8,
    },
}

impl std::fmt::Display for Expectation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Expectation::RoundTrip => f.write_str("round-trip"),
            Expectation::Empty => f.write_str("empty"),
            Expectation::UnknownVersion { version } => write!(f, "unknown-version {version}"),
            Expectation::Malformed { version } => write!(f, "malformed {version}"),
        }
    }
}

impl CodecVectors {
    /// An empty vector set for `schema`. Push at least one vector
    /// before publishing: a parsed file is never empty.
    pub fn new(schema: impl Into<String>) -> Self {
        Self {
            version: FormatVersion,
            schema: schema.into(),
            vectors: Vec::new(),
        }
    }

    /// Append a round-trip vector by encoding `body` through the codec
    /// under publication. Returns the pushed vector so the caller can
    /// attach [`notes`](CodecVector::notes).
    pub fn push_round_trip<B: IntentBody>(
        &mut self,
        name: impl Into<String>,
        body: &B,
    ) -> Result<&mut CodecVector, BodyError> {
        let bytes = body.to_bytes()?;
        self.vectors.push(CodecVector {
            name: name.into(),
            bytes,
            expect: Expectation::RoundTrip,
            notes: None,
        });
        Ok(self.vectors.last_mut().expect("vector was just pushed"))
    }

    /// Append a failure vector: raw bytes plus the typed decode error
    /// they must produce.
    ///
    /// # Panics
    ///
    /// On [`Expectation::RoundTrip`]; round-trip vectors are encoded
    /// from a typed body via [`push_round_trip`](Self::push_round_trip)
    /// so their bytes are canonical by construction.
    pub fn push_failure(
        &mut self,
        name: impl Into<String>,
        bytes: Vec<u8>,
        expect: Expectation,
    ) -> &mut CodecVector {
        assert!(
            expect != Expectation::RoundTrip,
            "push_failure takes a failure expectation; use push_round_trip",
        );
        self.vectors.push(CodecVector {
            name: name.into(),
            bytes,
            expect,
            notes: None,
        });
        self.vectors.last_mut().expect("vector was just pushed")
    }

    /// Parse a vector set from its JSON text.
    pub fn from_json(json: &str) -> Result<Self, FixtureError> {
        fixture::from_json(json)
    }

    /// The canonical published form: pretty JSON, trailing newline.
    pub fn to_json(&self) -> String {
        fixture::to_json(self)
    }

    /// Load a vector file from disk.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, FixtureError> {
        fixture::load(path.as_ref())
    }

    /// Write the vector file in its canonical published form.
    pub fn write(&self, path: impl AsRef<Path>) -> Result<(), FixtureError> {
        fixture::write(path.as_ref(), self)
    }

    /// Check a codec against every vector, collecting all violations
    /// rather than stopping at the first.
    ///
    /// A `round-trip` vector must decode and re-encode to the exact
    /// published bytes; a failure vector must produce the matching
    /// [`BodyError`] case (the free-text detail is not compared).
    pub fn check<B: IntentBody>(&self) -> Result<(), ConformanceReport> {
        let mut violations = Vec::new();
        for vector in &self.vectors {
            if let Err(detail) = vector.check::<B>() {
                violations.push(Violation {
                    vector: vector.name.clone(),
                    detail,
                });
            }
        }
        settle(violations)
    }

    /// [`check`](Self::check), panicking with the full report on any
    /// violation. The assertion form for adapter test suites.
    pub fn assert_conforms<B: IntentBody>(&self) {
        if let Err(report) = self.check::<B>() {
            panic!("codec does not conform to {}:\n{report}", self.schema);
        }
    }
}

impl CodecVector {
    /// Check one vector, returning the violation detail on divergence.
    fn check<B: IntentBody>(&self) -> Result<(), String> {
        let decoded = B::from_bytes(&self.bytes);
        match (&self.expect, decoded) {
            (Expectation::RoundTrip, Ok(body)) => {
                let reencoded = body
                    .to_bytes()
                    .map_err(|err| format!("re-encode failed: {err}"))?;
                if reencoded == self.bytes {
                    Ok(())
                } else {
                    Err(format!(
                        "re-encoded bytes diverge from the published vector: published {}, re-encoded {}",
                        hex::encode(&self.bytes),
                        hex::encode(&reencoded),
                    ))
                }
            }
            (Expectation::RoundTrip, Err(err)) => {
                Err(format!("expected a round trip, decode failed: {err}"))
            }
            (expect, Ok(_)) => Err(format!("expected {expect}, decode succeeded")),
            (expect, Err(err)) => {
                let matches = match (expect, &err) {
                    (Expectation::Empty, BodyError::Empty) => true,
                    (
                        Expectation::UnknownVersion { version },
                        BodyError::UnknownVersion { version: got },
                    ) => version == got,
                    (
                        Expectation::Malformed { version },
                        BodyError::Malformed { version: got, .. },
                    ) => version == got,
                    _ => false,
                };
                if matches {
                    Ok(())
                } else {
                    Err(format!("expected {expect}, got: {err}"))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use borsh::{BorshDeserialize, BorshSerialize};
    use nexum_venue_sdk::IntentBody;

    use super::*;

    #[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
    struct PayloadV1 {
        amount: u64,
        memo: String,
    }

    #[derive(IntentBody, Clone, Debug, PartialEq, Eq)]
    enum Body {
        V1(PayloadV1),
    }

    /// A codec with a diverging payload layout for the same tag.
    #[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
    struct NarrowPayload {
        amount: u32,
        memo: String,
    }

    #[derive(IntentBody, Clone, Debug, PartialEq, Eq)]
    enum NarrowBody {
        V1(NarrowPayload),
    }

    fn published() -> CodecVectors {
        let mut vectors = CodecVectors::new("test/body");
        vectors
            .push_round_trip(
                "v1",
                &Body::V1(PayloadV1 {
                    amount: 7,
                    memo: "gm".to_owned(),
                }),
            )
            .unwrap();
        vectors.push_failure("empty", Vec::new(), Expectation::Empty);
        vectors.push_failure(
            "unknown-version",
            vec![9, 0, 0],
            Expectation::UnknownVersion { version: 9 },
        );
        vectors.push_failure(
            "truncated",
            vec![0, 7],
            Expectation::Malformed { version: 0 },
        );
        vectors
    }

    #[test]
    fn conforming_codec_passes_every_vector() {
        published().check::<Body>().unwrap();
    }

    #[test]
    fn diverging_codec_fails_with_named_vectors() {
        let report = published().check::<NarrowBody>().unwrap_err();
        // The v1 payload no longer parses (u32 vs u64 layout); the
        // failure vectors still fail as published, so the report names
        // exactly the diverging vector.
        assert_eq!(report.violations.len(), 1, "violations: {report}");
        assert_eq!(report.violations[0].vector, "v1");
        assert!(report.violations[0].detail.contains("decode failed"));
    }

    #[test]
    #[should_panic(expected = "codec does not conform")]
    fn assert_conforms_panics_with_the_report() {
        published().assert_conforms::<NarrowBody>();
    }

    #[test]
    #[should_panic(expected = "push_failure takes a failure expectation")]
    fn push_failure_rejects_round_trip() {
        CodecVectors::new("test/body").push_failure("bad", Vec::new(), Expectation::RoundTrip);
    }

    #[test]
    fn json_form_is_stable_and_round_trips() {
        let mut vectors = published();
        vectors.vectors[0].notes = Some("first published body".to_owned());
        let json = vectors.to_json();
        assert_eq!(CodecVectors::from_json(&json).unwrap(), vectors);
        // The wire spellings are the contract for non-Rust readers.
        assert!(json.contains("\"version\": 1"));
        assert!(json.contains("\"round-trip\""));
        assert!(json.contains("\"unknown-version\""));
        assert!(json.contains("\"notes\": \"first published body\""));
        assert!(!json.contains("null"), "absent notes are omitted: {json}");
    }

    #[test]
    fn files_round_trip_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vectors.json");
        let vectors = published();
        vectors.write(&path).unwrap();
        assert_eq!(CodecVectors::load(&path).unwrap(), vectors);
    }

    #[test]
    fn malformed_file_fails_typedly() {
        assert!(matches!(
            CodecVectors::from_json("{"),
            Err(FixtureError::Format(_)),
        ));
        assert!(matches!(
            CodecVectors::load("/nonexistent/vectors.json"),
            Err(FixtureError::Read { .. }),
        ));
    }

    #[test]
    fn unknown_format_version_fails_closed() {
        let json = published().to_json().replace("\"version\": 1", "\"version\": 2");
        let Err(FixtureError::Format(detail)) = CodecVectors::from_json(&json) else {
            panic!("version 2 must not parse");
        };
        assert!(detail.contains("unknown fixture format version 2"));
    }

    #[test]
    fn missing_format_version_fails() {
        let json = published().to_json().replace("  \"version\": 1,\n", "");
        assert!(matches!(
            CodecVectors::from_json(&json),
            Err(FixtureError::Format(_)),
        ));
    }

    #[test]
    fn empty_vector_set_fails_to_parse() {
        let json = CodecVectors::new("test/body").to_json();
        let Err(FixtureError::Format(detail)) = CodecVectors::from_json(&json) else {
            panic!("an empty set must not parse");
        };
        assert!(detail.contains("never empty"));
    }
}
