//! The versioned intent-body codec: [`IntentBody`] and its typed
//! [`BodyError`].
//!
//! A body crosses the pool and adapter boundaries as opaque bytes; typing
//! is recovered guest-side against the venue's schema, an outer version
//! enum whose wire form is a one-byte version tag (the variant's
//! declaration index) plus the borsh payload. `#[derive(IntentBody)]`
//! (re-exported at the crate root) is the intended impl.
//!
//! Invariant: the tag order is the schema. Venues append new versions and
//! never reorder or remove variants.

use strum::IntoStaticStr;

use crate::VenueError;

/// The codec between a venue's typed body enum and the opaque wire bytes.
/// Sealed to `#[derive(IntentBody)]`, which owns the tag rules.
pub trait IntentBody: Sized + __private::Derived {
    /// Encode as the one-byte version tag plus the borsh payload.
    fn to_bytes(&self) -> Result<Vec<u8>, BodyError>;

    /// Decode, failing typedly on an empty body, an unknown version
    /// tag, or a payload that does not parse as the tagged version
    /// (including trailing bytes).
    fn from_bytes(bytes: &[u8]) -> Result<Self, BodyError>;
}

/// Why a body failed to cross the [`IntentBody`] codec. `IntoStaticStr`
/// yields a snake_case label per case.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum BodyError {
    /// No bytes at all: not even a version tag.
    #[error("empty body: missing the version tag")]
    Empty,
    /// The version tag names no published version of this body.
    #[error("unknown body version {version}")]
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
    /// A payload failed to encode; only reachable through a fallible
    /// custom `BorshSerialize` impl.
    #[error("version {version} payload failed to encode: {detail}")]
    Encode {
        /// The wire tag whose payload failed.
        version: u8,
        /// Borsh's encode failure detail.
        detail: String,
    },
}

/// Fold a codec failure into the wire error an adapter returns: decode
/// failures are the caller's `invalid-body`; an encode failure is the
/// adapter's own bug, reported retryable `unavailable`.
impl From<BodyError> for VenueError {
    fn from(err: BodyError) -> Self {
        match err {
            BodyError::Empty | BodyError::UnknownVersion { .. } | BodyError::Malformed { .. } => {
                VenueError::InvalidBody(err.to_string())
            }
            BodyError::Encode { .. } => VenueError::Unavailable(err.to_string()),
        }
    }
}

/// Re-exports for `#[derive(IntentBody)]` generated code only. `alloc`
/// rides along so the expansion resolves in a `#![no_std]` consumer.
#[doc(hidden)]
pub mod __private {
    pub extern crate alloc;

    pub use borsh;

    /// The [`IntentBody`](super::IntentBody) seal: implemented only by
    /// `#[derive(IntentBody)]` expansions.
    pub trait Derived {}
}
