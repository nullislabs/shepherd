//! The [`VenueAdapter`] trait and the export glue that turns an impl of
//! it into the component's `venue-adapter` world surface.
//!
//! The trait mirrors the world's export face one to one: `init` from the
//! world itself, the four intent functions from `nexum:intent/adapter`.
//! Functions are associated (no `self`): the component model instantiates
//! one adapter per venue and calls exports statically, so adapter state
//! lives in the adapter's own statics, exactly as in event modules.

use crate::{Config, Fault, IntentHeader, IntentStatus, SubmitOutcome, VenueError};

/// One venue's protocol speaker: the guest-side face of the
/// `venue-adapter` world. Implement it on a unit struct and hand that to
/// [`export_venue_adapter!`](crate::export_venue_adapter); bodies and
/// receipts arrive as the opaque bytes the wire carries, and impls
/// recover typing through [`IntentBody`](crate::IntentBody) (whose
/// [`BodyError`](crate::BodyError) converts into [`VenueError`] via `?`).
pub trait VenueAdapter {
    /// Configure the adapter from its `[config]` table before any
    /// submission. Mirrors the event-module `init`, so the supervisor
    /// boots both component kinds through the same machinery.
    fn init(config: Config) -> Result<(), Fault>;

    /// Project an opaque intent body onto the stable header guard
    /// policy runs on. Must be a pure derivation: no transport, no side
    /// effects, so the host can inspect a header before deciding to
    /// submit.
    fn derive_header(body: Vec<u8>) -> Result<IntentHeader, VenueError>;

    /// Submit an opaque intent body to this adapter's venue. Success is
    /// either the venue's receipt or `requires-signing`: a transaction
    /// the host must sign and send before the intent exists.
    fn submit(body: Vec<u8>) -> Result<SubmitOutcome, VenueError>;

    /// Report where a previously submitted intent is in its life.
    fn status(receipt: Vec<u8>) -> Result<IntentStatus, VenueError>;

    /// Ask the venue to withdraw an intent. Success means the venue
    /// accepted the cancellation, not that an in-flight settlement can
    /// no longer win the race.
    fn cancel(receipt: Vec<u8>) -> Result<(), VenueError>;
}

/// Export a [`VenueAdapter`] impl as the crate's `venue-adapter` world.
///
/// Invoke once at the top level of the adapter's cdylib crate. Emits a
/// hidden shim type wiring the world's `Guest` traits to the adapter's
/// associated functions, then the wit-bindgen export glue; the linker
/// rejects a second invocation in one component (duplicate export
/// symbols), matching the one-adapter-per-component contract.
#[macro_export]
macro_rules! export_venue_adapter {
    ($adapter:ty) => {
        #[doc(hidden)]
        struct __NexumVenueAdapterExport;

        impl $crate::bindings::Guest for __NexumVenueAdapterExport {
            fn init(
                config: ::std::vec::Vec<(::std::string::String, ::std::string::String)>,
            ) -> ::core::result::Result<(), $crate::Fault> {
                <$adapter as $crate::VenueAdapter>::init(config)
            }
        }

        impl $crate::bindings::exports::nexum::intent::adapter::Guest
            for __NexumVenueAdapterExport
        {
            fn derive_header(
                body: ::std::vec::Vec<u8>,
            ) -> ::core::result::Result<$crate::IntentHeader, $crate::VenueError> {
                <$adapter as $crate::VenueAdapter>::derive_header(body)
            }

            fn submit(
                body: ::std::vec::Vec<u8>,
            ) -> ::core::result::Result<$crate::SubmitOutcome, $crate::VenueError> {
                <$adapter as $crate::VenueAdapter>::submit(body)
            }

            fn status(
                receipt: ::std::vec::Vec<u8>,
            ) -> ::core::result::Result<$crate::IntentStatus, $crate::VenueError> {
                <$adapter as $crate::VenueAdapter>::status(receipt)
            }

            fn cancel(
                receipt: ::std::vec::Vec<u8>,
            ) -> ::core::result::Result<(), $crate::VenueError> {
                <$adapter as $crate::VenueAdapter>::cancel(receipt)
            }
        }

        $crate::bindings::__export_venue_adapter_world!(
            __NexumVenueAdapterExport with_types_in $crate::bindings
        );
    };
}
