//! `no_std` derive-hygiene probe for `#[derive(IntentBody)]`. The claim
//! is scoped to the derive, not the whole SDK: the expansion names only
//! `::core` and the SDK's `__private` re-exports (borsh, `alloc`), so it
//! compiles without the consumer's std prelude or an `extern crate
//! alloc`. The crate is `#![no_std]`; the `tests` module (std-exempt)
//! exercises an actual encode/decode round-trip, so the generated codec
//! is verified correct, not merely expanded.

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
