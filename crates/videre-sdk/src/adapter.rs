//! The [`VenueAdapter`] trait and the internal export codegen that turns
//! an impl of it into the component's `venue-adapter` world surface.
//!
//! The trait mirrors the world's export face one to one: `init` from the
//! world itself, the intent functions and the body-version declaration
//! from `videre:venue/adapter`.
//! Functions are associated (no `self`): the component model instantiates
//! one adapter per venue and calls exports statically, so adapter state
//! lives in the adapter's own statics, exactly as in event modules.

use crate::{Config, Fault, IntentHeader, IntentStatus, Quotation, SubmitOutcome, VenueError};

/// Reject an empty receipt as `invalid-receipt` before it reaches an
/// adapter. Called by the export shim ahead of `status` and `cancel`.
#[doc(hidden)]
pub fn guard_receipt(receipt: &[u8]) -> Result<(), VenueError> {
    if receipt.is_empty() {
        return Err(VenueError::InvalidReceipt);
    }
    Ok(())
}

/// One venue's protocol speaker: the guest-side face of the
/// `venue-adapter` world. Implement it on a unit struct and apply
/// [`#[videre_sdk::venue]`](crate::venue) to the impl; bodies and
/// receipts arrive as the opaque bytes the wire carries, and impls
/// recover typing through [`IntentBody`](crate::IntentBody) (whose
/// [`BodyError`](crate::BodyError) converts into [`VenueError`] via `?`).
pub trait VenueAdapter {
    /// Configure the adapter from its `[config]` table before any
    /// submission. Mirrors the event-module `init`, so the supervisor
    /// boots both component kinds through the same machinery.
    fn init(config: Config) -> Result<(), Fault>;

    /// Body-schema versions this adapter decodes. Install asserts it
    /// equals the manifest `[venue] body_versions` set. Defaults to
    /// declaring none.
    fn body_versions() -> Vec<u32> {
        Vec::new()
    }

    /// Project an opaque intent body onto the stable header guard
    /// policy runs on. Must be a pure derivation: no transport, no side
    /// effects, so the host can inspect a header before deciding to
    /// submit. The host's guard checkpoint is advisory-only until the
    /// egress-guard epic lands: a would-deny is logged, not enforced.
    fn derive_header(body: Vec<u8>) -> Result<IntentHeader, VenueError>;

    /// Price an opaque intent body: an indicative quotation, not an
    /// offer the venue is bound to fill.
    fn quote(body: Vec<u8>) -> Result<Quotation, VenueError>;

    /// Submit an opaque intent body to this adapter's venue. Success is
    /// either the venue's receipt or `requires-signing`: a transaction
    /// the host must sign and send before the intent exists.
    fn submit(body: Vec<u8>) -> Result<SubmitOutcome, VenueError>;

    /// Report where a previously submitted intent is in its life. The
    /// export shim rejects an empty receipt as `invalid-receipt` before
    /// dispatch.
    fn status(receipt: Vec<u8>) -> Result<IntentStatus, VenueError>;

    /// Ask the venue to withdraw an intent. Success means the venue
    /// accepted the cancellation, not that an in-flight settlement can
    /// no longer win the race. The export shim rejects an empty receipt
    /// as `invalid-receipt` before dispatch.
    fn cancel(receipt: Vec<u8>) -> Result<(), VenueError>;
}

/// Internal codegen `#[videre_sdk::venue]` expands to: a hidden shim
/// wiring a [`VenueAdapter`] impl to the macro-synthesized world's
/// `Guest` faces, then that world's `export!`. `Guest`, `exports`, and
/// `export!` resolve at the expansion site (the adapter crate root,
/// where the attribute put the world's bindgen), so the macro is
/// meaningful only inside the attribute's output. Not public API.
#[doc(hidden)]
#[macro_export]
macro_rules! __export_venue_adapter {
    ($adapter:ty) => {
        #[doc(hidden)]
        struct __VidereVenueAdapterExport;

        impl Guest for __VidereVenueAdapterExport {
            fn init(
                config: ::std::vec::Vec<(::std::string::String, ::std::string::String)>,
            ) -> ::core::result::Result<(), $crate::Fault> {
                <$adapter as $crate::VenueAdapter>::init(config)
            }
        }

        impl exports::videre::venue::adapter::Guest for __VidereVenueAdapterExport {
            fn body_versions() -> ::std::vec::Vec<u32> {
                <$adapter as $crate::VenueAdapter>::body_versions()
            }

            fn derive_header(
                body: ::std::vec::Vec<u8>,
            ) -> ::core::result::Result<$crate::IntentHeader, $crate::VenueError> {
                <$adapter as $crate::VenueAdapter>::derive_header(body)
            }

            fn quote(
                body: ::std::vec::Vec<u8>,
            ) -> ::core::result::Result<$crate::Quotation, $crate::VenueError> {
                <$adapter as $crate::VenueAdapter>::quote(body)
            }

            fn submit(
                body: ::std::vec::Vec<u8>,
            ) -> ::core::result::Result<$crate::SubmitOutcome, $crate::VenueError> {
                <$adapter as $crate::VenueAdapter>::submit(body)
            }

            fn status(
                receipt: ::std::vec::Vec<u8>,
            ) -> ::core::result::Result<$crate::IntentStatus, $crate::VenueError> {
                $crate::adapter::guard_receipt(&receipt)?;
                <$adapter as $crate::VenueAdapter>::status(receipt)
            }

            fn cancel(
                receipt: ::std::vec::Vec<u8>,
            ) -> ::core::result::Result<(), $crate::VenueError> {
                $crate::adapter::guard_receipt(&receipt)?;
                <$adapter as $crate::VenueAdapter>::cancel(receipt)
            }
        }

        export!(__VidereVenueAdapterExport);
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_receipt_is_rejected_as_invalid_receipt() {
        match guard_receipt(&[]).unwrap_err() {
            VenueError::InvalidReceipt => {}
            other => panic!("expected invalid-receipt, got {other:?}"),
        }
        guard_receipt(&[1]).unwrap();
    }
}
