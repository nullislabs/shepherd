//! Typed recovery of videre events from the core `custom` channel.
//!
//! A venue transition reaches a module as a `custom` event; this module
//! decodes it back into the typed [`IntentStatusUpdate`] a keeper handler
//! works with. The `#[keeper]` macro calls it for `on_intent_status`.

use crate::IntentStatusUpdate;

/// The `custom-event.kind` an intent-status transition rides on.
pub use crate::status_body::INTENT_STATUS_KIND;

/// Why an intent-status `custom` payload did not decode.
pub use crate::status_body::EnvelopeError;

/// Recover an [`IntentStatusUpdate`] from a `custom` event, keyed by its
/// `kind` and `payload`. `None` when the kind is another extension's;
/// `Some(Err)` when the payload is malformed.
pub fn intent_status_update(
    kind: &str,
    payload: &[u8],
) -> Option<Result<IntentStatusUpdate, EnvelopeError>> {
    if kind != INTENT_STATUS_KIND {
        return None;
    }
    Some(IntentStatusUpdate::decode(payload))
}

#[cfg(test)]
mod tests {
    use crate::status_body::{IntentStatus, StatusBody};

    use super::*;

    fn envelope() -> Vec<u8> {
        IntentStatusUpdate {
            venue: "cow".to_owned(),
            receipt: b"receipt".to_vec(),
            status: StatusBody {
                status: IntentStatus::Fulfilled,
                proof: None,
                reason: None,
            }
            .encode()
            .expect("encode body"),
        }
        .encode()
        .expect("encode envelope")
    }

    #[test]
    fn recovers_a_matching_kind() {
        let recovered = intent_status_update(INTENT_STATUS_KIND, &envelope())
            .expect("kind matches")
            .expect("payload decodes");
        assert_eq!(recovered.venue, "cow");
        assert_eq!(recovered.receipt, b"receipt");
    }

    #[test]
    fn ignores_a_foreign_kind() {
        assert!(intent_status_update("other-kind", &envelope()).is_none());
    }

    #[test]
    fn reports_a_malformed_payload() {
        assert!(
            intent_status_update(INTENT_STATUS_KIND, b"\xff")
                .expect("kind matches")
                .is_err()
        );
    }
}
