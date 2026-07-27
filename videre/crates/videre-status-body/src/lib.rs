//! Versioned opaque status-body codec.
//!
//! The host `event` stream carries an intent-status transition as opaque
//! bytes: a leading `u8` version tag, then that version's borsh payload.
//! An unknown tag fails closed; a body is never empty.
//!
//! v1 body: `0x01`, the [`IntentStatus`] discriminant, then borsh `option`
//! encodings of `proof` and `reason`.
//!
//! The [`IntentStatusUpdate`] envelope is tagged and fails closed on its
//! own version line, independent of the body it carries. v1 envelope:
//! `0x01`, then borsh `{venue, receipt, status}`.

#![warn(missing_docs)]

use borsh::{BorshDeserialize, BorshSerialize};

/// Wire tag of the v1 payload.
pub const VERSION_V1: u8 = 1;

/// Wire tag of the v1 [`IntentStatusUpdate`] envelope; independent of
/// [`VERSION_V1`].
pub const ENVELOPE_VERSION_V1: u8 = 1;

/// The `custom-event.kind` an intent-status transition rides on, matched
/// by subscribing modules.
pub const INTENT_STATUS_KIND: &str = "intent-status";

/// Envelope an intent-status `custom` event carries:
/// [`ENVELOPE_VERSION_V1`], then borsh `{venue, receipt, status}`, where
/// `status` is a [`StatusBody`]-encoded body.
#[derive(BorshDeserialize, BorshSerialize, Clone, Debug, Eq, PartialEq)]
pub struct IntentStatusUpdate {
    /// Venue id the receipt was issued by.
    pub venue: String,
    /// The venue-scoped intent identifier, opaque to the host.
    pub receipt: Vec<u8>,
    /// The [`StatusBody`]-encoded status body.
    pub status: Vec<u8>,
}

impl IntentStatusUpdate {
    /// Encode as the envelope version tag plus the borsh payload.
    pub fn encode(&self) -> Result<Vec<u8>, EncodeError> {
        let mut out = vec![ENVELOPE_VERSION_V1];
        borsh::to_writer(&mut out, self).map_err(|err| EncodeError {
            detail: err.to_string(),
        })?;
        Ok(out)
    }

    /// Decode; fails on empty, an unknown tag (fail-closed), or a payload
    /// that does not parse as the tagged version.
    pub fn decode(bytes: &[u8]) -> Result<Self, EnvelopeError> {
        match bytes {
            [] => Err(EnvelopeError::Empty),
            [ENVELOPE_VERSION_V1, payload @ ..] => {
                borsh::from_slice(payload).map_err(|err| EnvelopeError::Malformed {
                    version: ENVELOPE_VERSION_V1,
                    detail: err.to_string(),
                })
            }
            [version, ..] => Err(EnvelopeError::UnknownVersion { version: *version }),
        }
    }
}

/// Why bytes failed to decode as an [`IntentStatusUpdate`] envelope.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum EnvelopeError {
    /// No bytes at all: not even a version tag.
    #[error("empty intent-status envelope: missing the version tag")]
    Empty,
    /// The tag names no published envelope version (fail-closed).
    #[error("unknown intent-status envelope version {version}")]
    UnknownVersion {
        /// The unrecognised wire tag.
        version: u8,
    },
    /// The tag named a known version but its payload did not decode
    /// (malformed borsh or trailing bytes).
    #[error("malformed version {version} intent-status envelope: {detail}")]
    Malformed {
        /// The wire tag whose payload failed.
        version: u8,
        /// Borsh's decode failure detail.
        detail: String,
    },
}

/// Where an intent is in its life at the venue; the borsh discriminant is
/// wire form, so append new states, never reorder.
#[derive(BorshDeserialize, BorshSerialize, Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntentStatus {
    /// Accepted for processing but not yet live at the venue.
    Pending,
    /// Live at the venue and eligible for settlement.
    Open,
    /// Settled.
    Fulfilled,
    /// Withdrawn or terminally refused before settlement.
    Cancelled,
    /// Reached its expiry without settling.
    Expired,
}

/// Why an intent failed terminally, as reported by the venue.
#[derive(BorshDeserialize, BorshSerialize, Clone, Debug, Eq, PartialEq)]
pub struct FailReason {
    /// Venue-scoped machine-readable code, stable enough to match on.
    pub code: String,
    /// Human-readable detail for logs and the consent surface.
    pub detail: String,
}

/// One decoded status body. There is no `failed` status: a terminal
/// failure reads as a non-[`Fulfilled`] terminal `status` plus a `reason`.
///
/// [`Fulfilled`]: IntentStatus::Fulfilled
#[derive(BorshDeserialize, BorshSerialize, Clone, Debug, Eq, PartialEq)]
pub struct StatusBody {
    /// Where the intent is in its life at the venue.
    pub status: IntentStatus,
    /// Venue-defined settlement proof.
    pub proof: Option<Vec<u8>>,
    /// Terminal-failure reason.
    pub reason: Option<FailReason>,
}

impl StatusBody {
    /// Encode as the version tag plus the borsh payload; never empty.
    pub fn encode(&self) -> Result<Vec<u8>, EncodeError> {
        let mut out = vec![VERSION_V1];
        borsh::to_writer(&mut out, self).map_err(|err| EncodeError {
            detail: err.to_string(),
        })?;
        Ok(out)
    }

    /// Decode; fails on empty, an unknown tag (fail-closed), or a payload
    /// that does not parse as the tagged version.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        match bytes {
            [] => Err(DecodeError::Empty),
            [VERSION_V1, payload @ ..] => {
                borsh::from_slice(payload).map_err(|err| DecodeError::Malformed {
                    version: VERSION_V1,
                    detail: err.to_string(),
                })
            }
            [version, ..] => Err(DecodeError::UnknownVersion { version: *version }),
        }
    }
}

/// A payload failed to encode. Only reachable when a field's length
/// exceeds the wire's `u32` bound.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("status body failed to encode: {detail}")]
pub struct EncodeError {
    /// Borsh's encode failure detail.
    pub detail: String,
}

/// Why bytes failed to decode as a status body.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum DecodeError {
    /// No bytes at all: not even a version tag.
    #[error("empty status body: missing the version tag")]
    Empty,
    /// The tag names no published version (fail-closed).
    #[error("unknown status-body version {version}")]
    UnknownVersion {
        /// The unrecognised wire tag.
        version: u8,
    },
    /// The tag named a known version but its payload did not decode
    /// (malformed borsh or trailing bytes).
    #[error("malformed version {version} payload: {detail}")]
    Malformed {
        /// The wire tag whose payload failed.
        version: u8,
        /// Borsh's decode failure detail.
        detail: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(status: IntentStatus) -> StatusBody {
        StatusBody {
            status,
            proof: None,
            reason: None,
        }
    }

    fn envelope(venue: &str) -> IntentStatusUpdate {
        IntentStatusUpdate {
            venue: venue.to_owned(),
            receipt: b"receipt".to_vec(),
            status: body(IntentStatus::Open).encode().expect("encode body"),
        }
    }

    #[test]
    fn envelope_leads_with_its_version_tag() {
        let encoded = envelope("cow").encode().expect("encode");
        assert_eq!(encoded[0], ENVELOPE_VERSION_V1);
    }

    /// Pins the whole v1 envelope framing, not just the tag position.
    #[test]
    fn golden_envelope() {
        let encoded = envelope("cow").encode().expect("encode");
        let expected = [
            &[ENVELOPE_VERSION_V1, 3, 0, 0, 0][..],
            b"cow",
            &[7, 0, 0, 0],
            b"receipt",
            &[4, 0, 0, 0, VERSION_V1, 1, 0, 0],
        ]
        .concat();
        assert_eq!(encoded, expected);
    }

    #[test]
    fn envelope_round_trips() {
        let original = envelope("cow");
        let decoded =
            IntentStatusUpdate::decode(&original.encode().expect("encode")).expect("decode");
        assert_eq!(decoded, original);
    }

    #[test]
    fn empty_envelope_fails_typedly() {
        assert_eq!(IntentStatusUpdate::decode(&[]), Err(EnvelopeError::Empty));
    }

    /// A future envelope version is refused, not misparsed.
    #[test]
    fn future_envelope_version_fails_closed() {
        let mut skewed = envelope("cow").encode().expect("encode");
        skewed[0] = ENVELOPE_VERSION_V1 + 1;
        assert_eq!(
            IntentStatusUpdate::decode(&skewed),
            Err(EnvelopeError::UnknownVersion {
                version: ENVELOPE_VERSION_V1 + 1,
            }),
        );
    }

    /// Bare borsh with no leading tag must never decode.
    #[test]
    fn untagged_envelope_never_decodes() {
        for venue in ["c", "cow", "a longer venue id"] {
            let mut untagged = Vec::new();
            borsh::to_writer(&mut untagged, &envelope(venue)).expect("encode");
            assert!(
                IntentStatusUpdate::decode(&untagged).is_err(),
                "untagged {venue} envelope decoded",
            );
        }
    }

    #[test]
    fn envelope_trailing_bytes_are_malformed() {
        let mut encoded = envelope("cow").encode().expect("encode");
        encoded.push(0);
        assert!(matches!(
            IntentStatusUpdate::decode(&encoded),
            Err(EnvelopeError::Malformed {
                version: ENVELOPE_VERSION_V1,
                ..
            }),
        ));
    }

    #[test]
    fn truncated_envelope_is_malformed() {
        let encoded = envelope("cow").encode().expect("encode");
        assert!(matches!(
            IntentStatusUpdate::decode(&encoded[..encoded.len() - 1]),
            Err(EnvelopeError::Malformed {
                version: ENVELOPE_VERSION_V1,
                ..
            }),
        ));
    }

    /// Envelope and body tags are separate wire lines: the envelope
    /// decodes even when its body version is refused.
    #[test]
    fn envelope_and_body_versions_are_independent() {
        let mut update = envelope("cow");
        update.status[0] = VERSION_V1 + 1;
        let decoded =
            IntentStatusUpdate::decode(&update.encode().expect("encode")).expect("decode");
        assert_eq!(
            StatusBody::decode(&decoded.status),
            Err(DecodeError::UnknownVersion {
                version: VERSION_V1 + 1,
            }),
        );
    }

    #[test]
    fn golden_minimal_open() {
        let encoded = body(IntentStatus::Open).encode().expect("encode");
        assert_eq!(encoded, [VERSION_V1, 1, 0, 0]);
    }

    #[test]
    fn golden_fulfilled_with_proof() {
        let encoded = StatusBody {
            status: IntentStatus::Fulfilled,
            proof: Some(vec![0xaa, 0xbb]),
            reason: None,
        }
        .encode()
        .expect("encode");
        assert_eq!(encoded, [VERSION_V1, 2, 1, 2, 0, 0, 0, 0xaa, 0xbb, 0]);
    }

    #[test]
    fn golden_terminal_failure() {
        let encoded = StatusBody {
            status: IntentStatus::Cancelled,
            proof: None,
            reason: Some(FailReason {
                code: "oc".into(),
                detail: "od".into(),
            }),
        }
        .encode()
        .expect("encode");
        assert_eq!(
            encoded,
            [
                VERSION_V1, 3, 0, 1, 2, 0, 0, 0, b'o', b'c', 2, 0, 0, 0, b'o', b'd'
            ],
        );
    }

    #[test]
    fn round_trips_every_status() {
        for status in [
            IntentStatus::Pending,
            IntentStatus::Open,
            IntentStatus::Fulfilled,
            IntentStatus::Cancelled,
            IntentStatus::Expired,
        ] {
            let original = StatusBody {
                status,
                proof: Some(b"proof".to_vec()),
                reason: Some(FailReason {
                    code: "code".into(),
                    detail: "detail".into(),
                }),
            };
            let decoded = StatusBody::decode(&original.encode().expect("encode")).expect("decode");
            assert_eq!(decoded, original);
        }
    }

    #[test]
    fn a_body_is_never_empty() {
        let encoded = body(IntentStatus::Pending).encode().expect("encode");
        assert!(encoded.len() >= 2, "at minimum the tag and the status");
    }

    #[test]
    fn empty_bytes_fail_typedly() {
        assert_eq!(StatusBody::decode(&[]), Err(DecodeError::Empty));
    }

    #[test]
    fn unknown_version_fails_closed() {
        assert_eq!(
            StatusBody::decode(&[2, 1, 0, 0]),
            Err(DecodeError::UnknownVersion { version: 2 }),
        );
    }

    #[test]
    fn unknown_status_discriminant_is_malformed() {
        assert!(matches!(
            StatusBody::decode(&[VERSION_V1, 5, 0, 0]),
            Err(DecodeError::Malformed {
                version: VERSION_V1,
                ..
            }),
        ));
    }

    #[test]
    fn trailing_bytes_are_malformed() {
        let mut encoded = body(IntentStatus::Open).encode().expect("encode");
        encoded.push(0);
        assert!(matches!(
            StatusBody::decode(&encoded),
            Err(DecodeError::Malformed {
                version: VERSION_V1,
                ..
            }),
        ));
    }

    #[test]
    fn truncated_payload_is_malformed() {
        assert!(matches!(
            StatusBody::decode(&[VERSION_V1, 1, 0]),
            Err(DecodeError::Malformed {
                version: VERSION_V1,
                ..
            }),
        ));
    }
}
