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
//! - [`client`] - the typed intent client core: [`VenueId`] and
//!   [`IntentClient`], which binds a venue and encodes through
//!   [`IntentBody`] before the byte-level [`VenueClient`] seam. Lives
//!   here (not in the strategy SDK) so the codec and the client that
//!   speaks it version together.
//!
//! - [`keeper`] - the generic sweep assembler: [`Keeper::sweep`] runs
//!   the world-neutral `nexum_sdk::keeper` stores over a
//!   [`ConditionalSource`](nexum_sdk::keeper::ConditionalSource)
//!   producing the shared [`Sweep`] outcome, submitting through the
//!   [`VenueClient`] seam.
//!
//! - [`transport`] - typed wrappers over the world's scoped imports:
//!   [`HostChain`](transport::HostChain) behind the SDK [`ChainHost`]
//!   seam (plus batch), [`HostMessaging`](transport::HostMessaging)
//!   behind [`MessagingHost`](transport::MessagingHost), and the
//!   wasi:http surface re-exported as [`transport::http`].
//!
//! - [`faults`] - the conversions that make `?` work across the wire
//!   fault, the SDK-neutral fault, and [`VenueError`]; plus
//!   [`VenueFault`], the owned client-side mirror.
//!
//! ## Why the bindgen lives in this crate
//!
//! The adapter world's types generate once, in [`bindings`]: the trait,
//! wrappers, and client core are all typed over them. `#[venue]`'s
//! per-cdylib bindgen remaps the type interfaces onto [`bindings`], so
//! an adapter speaks these types while its world imports stay derived
//! from its own manifest.
//!
//! [`ChainHost`]: nexum_sdk::host::ChainHost
//! [`IntentClient`]: client::IntentClient
//! [`VenueClient`]: client::VenueClient

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![warn(missing_docs)]

#[allow(missing_docs)]
pub mod bindings;

pub mod adapter;
pub mod body;
pub mod client;
pub mod faults;
pub mod keeper;
pub mod transport;

pub use adapter::VenueAdapter;
pub use body::{BodyError, IntentBody};
pub use client::{ClientError, IntentClient, Quoted, VenueClient, VenueId};
pub use faults::VenueFault;
pub use keeper::{Keeper, Sweep, SweepReport};
/// Derive [`IntentBody`] on the outer per-venue version enum. See
/// [`videre_macros::IntentBody`].
pub use videre_macros::IntentBody;
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
pub use bindings::videre::value_flow::types as value_flow;

/// The wire config table (`nexum:host/types.config`) `init` receives.
pub use bindings::nexum::host::types::Config;
/// The wire fault (`nexum:host/types.fault`) `init` returns. Transport
/// seams speak the SDK-neutral [`nexum_sdk::host::Fault`] instead; the
/// [`faults`] conversions bridge the two.
pub use bindings::nexum::host::types::Fault;
