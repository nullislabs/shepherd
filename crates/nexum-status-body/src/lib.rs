//! The versioned opaque status-body codec.
//!
//! The host `event` stream carries an intent-status transition as opaque
//! bytes: a leading `u8` version tag, then that version's borsh payload.
//! The emitter encodes, the keeper decodes, the host never inspects the
//! bytes. An unknown tag fails closed and a body is never empty.
//!
//! v1 wire form: `0x01`, the [`IntentStatus`] discriminant, then the
//! borsh `option` encodings of `proof` and `reason`.

#![warn(missing_docs)]

use borsh::{BorshDeserialize, BorshSerialize};

/// Wire tag of the v1 payload.
pub const VERSION_V1: u8 = 1;

/// Where an intent is in its life at the venue. The borsh discriminant
/// is the wire form: append new states, never reorder.
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

/// One decoded status body.
///
/// `proof` is display-grade venue bytes (for an EVM venue, typically
/// the settlement transaction hash). There is no `failed` status: a
/// venue-reported terminal failure reads as a non-[`Fulfilled`]
/// terminal `status` plus a `reason`.
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
    /// Encode as the version tag plus the borsh payload. Never empty:
    /// at minimum the tag and the status discriminant.
    pub fn encode(&self) -> Result<Vec<u8>, EncodeError> {
        let mut out = vec![VERSION_V1];
        borsh::to_writer(&mut out, self).map_err(|err| EncodeError {
            detail: err.to_string(),
        })?;
        Ok(out)
    }

    /// Decode, failing typedly on an empty body, an unknown version
    /// tag (fail-closed), or a payload that does not parse as the
    /// tagged version (including trailing bytes).
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
    /// The version tag names no published version. Fail-closed: a
    /// keeper never guesses at a future layout.
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
