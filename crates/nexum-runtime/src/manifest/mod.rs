//! `module.toml` parser and capability-enforcement helpers (0.2 scope).
//!
//! 0.2 intentionally ships a slim subset of the manifest spec:
//!
//! - `[capabilities].required` is parsed and validated (names must be in
//!   the known capability set; the 0.2 reference engine always provides
//!   all of them, so this is a sanity check + future-proofing).
//! - `[capabilities].optional` is parsed and logged; trap-stub fallback
//!   for absent optionals is deferred to 0.3.
//! - `[capabilities.http].allow` is parsed and consulted by the `http`
//!   host impl before any outbound call.
//! - `[config]` is flattened to `Vec<(String, String)>` and passed to the
//!   module's `init`. Typed `config-value` variant is deferred to 0.3.
//!
//! When the manifest file is missing or has no `[capabilities]` section,
//! a deprecation warning is emitted and the engine falls back to 0.1
//! behaviour (treat every linked capability as required). This fallback
//! will be removed in 0.3.
//!
//! ## Layout
//!
//! - `types`: the serde `Manifest` shape + `LoadedManifest` the engine
//!   actually consumes, plus the `KNOWN_CAPABILITIES` registry.
//! - `load`: `module.toml` -> `LoadedManifest`, plus the host/URL
//!   helpers the `http` backend uses at request time.
//! - `capabilities`: WIT-import vs declared-capabilities cross-check.
//! - `error`: `ParseError`, `CapabilityViolation`.

mod capabilities;
mod error;
mod load;
mod types;

pub(crate) use capabilities::enforce_capabilities;
pub(crate) use load::{extract_host, fallback_manifest, host_allowed, load};
pub(crate) use types::{LoadedManifest, Subscription};
// CapabilityViolation, ParseError, and the *Section structs are
// reachable through these functions' return / argument types;
// consumers that need to name them directly do so via
// `crate::manifest::error::*` or `::types::*`.
