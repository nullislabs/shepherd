//! Guest-side SDK for the videre venue personas: the venue author
//! (a venue's protocol speaker exporting the `venue-adapter` world) and
//! the keeper author driving venues through the client seam.
//!
//! Entry points:
//! - [`VenueAdapter`] plus [`venue`](macro@venue): the venue authoring path.
//! - [`IntentBody`] with [`BodyError`]: the versioned borsh body codec.
//! - [`client`]: [`VenueClient`] over the [`VenueTransport`] seam, plus
//!   [`keeper`](macro@keeper) wiring and [`poll_once`](client::poll_once).
//! - [`keeper`](mod@keeper): [`Keeper::run`], the generic run assembler.
//! - [`transport`], [`faults`], [`event`], [`status_body`].
//!
//! [`ChainHost`]: nexum_sdk::host::ChainHost
//! [`VenueClient`]: client::VenueClient
//! [`VenueTransport`]: client::VenueTransport

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![warn(missing_docs)]

#[allow(missing_docs)]
pub mod bindings;

pub mod adapter;
pub mod body;
pub mod client;
pub mod event;
pub mod faults;
pub mod keeper;
pub mod transport;

pub use adapter::VenueAdapter;
pub use body::{BodyError, IntentBody};
pub use client::{
    ClientError, HostVenues, Quoted, Venue, VenueClient, VenueId, VenueReconcile, VenueTransport,
};
pub use faults::VenueFault;
pub use keeper::{
    DEFAULT_RECONCILE_BUDGET, Keeper, Outcome, ReconcileReport, RunReport, reconcile, retry_action,
};
/// Derive [`IntentBody`] on the outer per-venue version enum.
pub use videre_macros::IntentBody;
/// Keeper authoring path. Apply to a handler impl: emits the per-cdylib
/// bindgen for a `module.toml`-derived world, drives a [`VenueClient`]
/// with shared type identity, dispatches events (async ones through
/// [`client::poll_once`]), and folds [`ClientError`] into the wire fault
/// so `?` works in handlers.
pub use videre_macros::keeper;
/// Venue authoring path. Apply to the `impl VenueAdapter for MyVenue`
/// block: emits the per-cdylib bindgen for a `module.toml`-derived world,
/// the `videre:venue/adapter` export glue, and `export!`.
pub use videre_macros::venue;

/// The intent ontology the [`VenueAdapter`] face and client core speak.
pub use bindings::videre::types::types::{
    AuthScheme, IntentHeader, IntentStatus, Quotation, RateLimit, Settlement, SubmitOutcome,
    UnsignedTx, VenueError,
};
/// The value-flow vocabulary intent headers are expressed in.
pub mod value_flow;
/// The venue status-body codec.
pub use videre_status_body as status_body;

/// The intent-status transition a keeper recovers from a `custom` event
/// through [`event::intent_status_update`]. Wire form is a version tag
/// plus a borsh envelope; an unknown tag fails closed.
pub use videre_status_body::IntentStatusUpdate;

/// The wire config table (`nexum:host/types.config`) `init` receives.
pub use bindings::nexum::host::types::Config;
/// The wire fault (`nexum:host/types.fault`) `init` returns; the
/// [`faults`] conversions bridge it to the SDK-neutral
/// [`nexum_sdk::host::Fault`] the transport seams speak.
pub use bindings::nexum::host::types::Fault;
