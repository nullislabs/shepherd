//! The versioned intent-body codec: [`IntentBody`] and its typed
//! [`BodyError`].
//!
//! An intent body crosses the pool and adapter boundaries as opaque
//! bytes; typing is recovered guest-side against the venue's published
//! schema. That schema is an outer version enum whose wire form is the
//! borsh enum layout: a one-byte version tag (the variant's declaration
//! index) followed by the borsh-encoded payload. `#[derive(IntentBody)]`
//! (re-exported at the crate root) implements the codec over such an
//! enum and is the intended way to get an impl; the derive owns the tag
//! handling, so an unknown version fails as the typed
//! [`BodyError::UnknownVersion`] instead of a stringly borsh error.
//!
//! The one non-obvious invariant: the tag order is the schema. Venues
//! append new versions at the end and never reorder or remove variants.

use strum::IntoStaticStr;

use crate::VenueError;

/// The codec between a venue's typed body enum and the opaque bytes the
/// pool and adapter boundaries carry. Sealed to
/// `#[derive(IntentBody)]` on the outer version enum: the derive owns
/// the tag rules, so no hand impl can break them.
pub trait IntentBody: Sized + __private::Derived {
    /// Encode as the one-byte version tag plus the borsh payload.
    fn to_bytes(&self) -> Result<Vec<u8>, BodyError>;

    /// Decode, failing typedly on an empty body, an unknown version
    /// tag, or a payload that does not parse as the tagged version
    /// (including trailing bytes).
    fn from_bytes(bytes: &[u8]) -> Result<Self, BodyError>;
}

/// Why a body failed to cross the [`IntentBody`] codec.
///
/// `IntoStaticStr` yields a snake_case label per case for log and
/// metric fields.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum BodyError {
    /// No bytes at all: not even a version tag.
    #[error("empty body: missing the version tag")]
    Empty,
    /// The version tag names no published version of this body. The
    /// decodable-future-versions story lives here: a v1 adapter handed
    /// a v2 body reports the exact unknown tag instead of garbling the
    /// payload.
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
    /// A payload failed to encode. Only reachable through a fallible
    /// custom `BorshSerialize` impl; derived payloads encode
    /// infallibly.
    #[error("version {version} payload failed to encode: {detail}")]
    Encode {
        /// The wire tag whose payload failed.
        version: u8,
        /// Borsh's encode failure detail.
        detail: String,
    },
}

/// Fold a codec failure into the wire error an adapter returns: decode
/// failures are the caller's malformed body (`invalid-body`); an encode
/// failure is the adapter's own bug, reported retryable (`unavailable`).
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

/// Re-exports for `#[derive(IntentBody)]` generated code only; not a
/// public surface. `alloc` rides along so the expansion resolves in a
/// `#![no_std]` consumer without its own `extern crate alloc`.
#[doc(hidden)]
pub mod __private {
    pub extern crate alloc;

    pub use borsh;

    /// The [`IntentBody`](super::IntentBody) seal: implemented only by
    /// `#[derive(IntentBody)]` expansions.
    pub trait Derived {}
}
