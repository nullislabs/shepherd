//! `module.toml` parser and capability-enforcement helpers.
//!
//! - `[capabilities].required` is parsed and validated: names must be in
//!   the known capability set, which the engine always provides.
//! - `[capabilities].optional` is parsed and logged.
//! - `[capabilities.http].allow` is parsed and consulted by the
//!   wasi:http gate before any outbound call.
//! - `[config]` is flattened to `Vec<(String, String)>` and passed to the
//!   module's `init`.
//!
//! When the manifest file is missing or has no `[capabilities]` section,
//! a deprecation warning is emitted and the engine falls back to treating
//! every linked capability as required.
//!
//! ## Layout
//!
//! - `types`: the serde `Manifest` shape + `LoadedManifest` the engine
//!   actually consumes, plus the core-capability list.
//! - `load`: `module.toml` -> `LoadedManifest`, plus the host-matching
//!   helper the wasi:http gate uses at request time.
//! - `capabilities`: WIT-import vs declared-capabilities cross-check, plus
//!   the extension-extensible `CapabilityRegistry`.
//! - `error`: `ParseError`, `CapabilityViolation`, `CapabilityError`.

mod capabilities;
mod error;
mod load;
mod types;

pub(crate) use capabilities::enforce_capabilities;
pub use capabilities::{CapabilityRegistry, NamespaceCaps};
pub(crate) use load::{fallback_manifest, host_allowed, load};
pub use types::ExtensionSections;
pub(crate) use types::{ComponentKind, LoadedManifest, ResourceSection, Subscription};
// CapabilityViolation, ParseError, and the *Section structs are
// reachable through these functions' return / argument types;
// consumers that need to name them directly do so via
// `crate::manifest::error::*` or `::types::*`.
