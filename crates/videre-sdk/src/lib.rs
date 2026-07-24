//! # videre-sdk
//!
//! Guest-side SDK for the videre personas: the venue author (one
//! venue's protocol speaker exporting the `venue-adapter` world) and
//! the keeper author driving venues through the client seam. Where
//! `nexum-sdk` serves the strategy-module persona, this crate serves
//! both venue sides of it.
//!
//! ## What lives here
//!
//! - [`VenueAdapter`] - the trait mirroring the world's export face
//!   (`init` plus the five intent functions). `#[videre_sdk::venue]`
//!   on the impl turns it into the component's export glue: the single
//!   blessed authoring path.
//!
//! - [`IntentBody`] (trait and derive) with [`BodyError`] - the borsh
//!   codec over the outer per-venue version enum. The wire form is a
//!   one-byte version tag plus the borsh payload; an unknown tag fails
//!   typedly rather than as a stringly decode error.
//!
//! - [`client`] - the typed venue client: a [`Venue`] marker (its
//!   [`VenueId`] plus body schema) drives [`VenueClient`], which
//!   encodes through [`IntentBody`] before the byte-level, native-AFIT
//!   [`VenueTransport`] seam ([`HostVenues`] binds it to the module's
//!   own `videre:venue/client` import). Lives here (not in the
//!   strategy SDK) so the codec and the client that speaks it version
//!   together. `#[videre_sdk::keeper]` on a handler impl wires the
//!   import and drives async handlers;
//!   [`poll_once`](client::poll_once) completes their futures on the
//!   synchronous guest boundary.
//!
//! - [`keeper`](mod@keeper) - the generic run assembler:
//!   [`Keeper::run`] runs the world-neutral `nexum_sdk::keeper`
//!   stores over a
//!   [`Poller`](nexum_sdk::keeper::Poller)
//!   producing the shared [`Outcome`] outcome, submitting through the
//!   [`VenueTransport`] seam.
//!
//! - [`transport`] - typed wrappers over the world's scoped imports:
//!   [`HostChain`](transport::HostChain) behind the SDK [`ChainHost`]
//!   seam (plus batch), [`HostMessaging`](transport::HostMessaging)
//!   behind [`MessagingHost`](transport::MessagingHost), the
//!   wasi:http surface re-exported as [`transport::http`], and
//!   [`BoundedFetch`](transport::BoundedFetch), which caps the
//!   wasi:http phase timeouts of every adapter request.
//!
//! - [`faults`] - the conversions that make `?` work across the wire
//!   fault, the SDK-neutral fault, and [`VenueError`]; plus
//!   [`VenueFault`], the owned client-side mirror.
//!
//! - [`event`] - typed recovery of videre events from the core `custom`
//!   escape hatch: [`event::intent_status_update`] decodes an
//!   intent-status transition a keeper subscribes to.
//!
//! - [`status_body`] - the venue status-body codec: decode an
//!   `intent-status` event's `status` bytes into a typed
//!   [`StatusBody`](status_body::StatusBody).
//!
//! ## Why the bindgen lives in this crate
//!
//! The shared interfaces generate once, in [`bindings`], from an
//! import-only world: the trait, wrappers, and client core are all
//! typed over them. The per-cdylib bindgens (`#[venue]`, `#[keeper]`)
//! remap the shared interfaces onto [`bindings`], so a macro-built
//! component speaks these types while its world stays derived from its
//! own manifest.
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
/// Derive [`IntentBody`] on the outer per-venue version enum. See
/// [`videre_macros::IntentBody`].
pub use videre_macros::IntentBody;
/// The blessed keeper authoring path. Apply to a worker's handler impl:
/// emits the per-cdylib bindgen for a world derived from `module.toml`
/// (asserting the `client` capability), remaps the videre interfaces
/// onto the SDK bindings so the module drives a [`VenueClient`] with
/// shared type identity, dispatches events to the handlers (async ones
/// completed through [`client::poll_once`]), and folds [`ClientError`] into
/// the wire fault so `?` works in handlers. See
/// [`videre_macros::keeper`].
pub use videre_macros::keeper;
/// The single blessed venue authoring path. Apply to the adapter's
/// `impl VenueAdapter for MyVenue` block: emits the per-cdylib bindgen
/// for a world derived from `module.toml` (asserting its
/// `kind = "venue-adapter"`), the `videre:venue/adapter` export glue,
/// and `export!`. The built component imports exactly the manifest's
/// declared scoped transport. See [`videre_macros::venue`].
pub use videre_macros::venue;

/// The intent ontology at its plain spellings: the types the
/// [`VenueAdapter`] face and the client core speak.
pub use bindings::videre::types::types::{
    AuthScheme, IntentHeader, IntentStatus, Quotation, RateLimit, Settlement, SubmitOutcome,
    UnsignedTx, VenueError,
};
/// The value-flow vocabulary intent headers are expressed in.
pub mod value_flow;
/// The venue status-body codec: decode an `intent-status` event's
/// `status` bytes into a typed [`StatusBody`](status_body::StatusBody).
pub use videre_status_body as status_body;

/// The intent-status transition a keeper recovers from a `custom` event
/// through [`event::intent_status_update`]. Its wire form is a version
/// tag plus the borsh envelope defined by this struct, not a WIT record:
/// it crosses the `custom` event as opaque bytes, and an unknown tag
/// fails closed. The status body rides its own inner codec.
pub use videre_status_body::IntentStatusUpdate;

/// The wire config table (`nexum:host/types.config`) `init` receives.
pub use bindings::nexum::host::types::Config;
/// The wire fault (`nexum:host/types.fault`) `init` returns. Transport
/// seams speak the SDK-neutral [`nexum_sdk::host::Fault`] instead; the
/// [`faults`] conversions bridge the two.
pub use bindings::nexum::host::types::Fault;
