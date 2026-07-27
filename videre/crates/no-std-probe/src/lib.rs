//! `no_std` derive-hygiene probe for `#[derive(IntentBody)]`: the
//! expansion names only `::core` and the SDK's `__private` re-exports,
//! so it compiles under `#![no_std]` without an `extern crate alloc`.
//! The `tests` module round-trips the generated codec.

#![no_std]
#![warn(missing_docs)]

use videre_sdk::IntentBody;

/// The probe schema: one published version over a bare byte payload.
#[derive(IntentBody, Clone, Debug, PartialEq, Eq)]
pub enum ProbeBody {
    /// First published version.
    V1(u8),
}

#[cfg(test)]
mod tests {
    use super::*;
    use videre_sdk::BodyError;

    #[test]
    fn round_trip() {
        let body = ProbeBody::V1(7);
        let bytes = body.to_bytes().expect("encode");
        // One-byte version tag (0) then the borsh u8 payload.
        assert_eq!(bytes, [0u8, 7u8]);
        assert_eq!(ProbeBody::from_bytes(&bytes).expect("decode"), body);
    }

    #[test]
    fn unknown_version() {
        assert!(matches!(
            ProbeBody::from_bytes(&[9, 7]),
            Err(BodyError::UnknownVersion { version: 9 }),
        ));
    }

    #[test]
    fn empty() {
        assert!(matches!(ProbeBody::from_bytes(&[]), Err(BodyError::Empty)));
    }
}
