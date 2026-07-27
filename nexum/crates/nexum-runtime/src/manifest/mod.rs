//! `module.toml` parser and capability enforcement.
//!
//! `load` parses and validates a manifest; `capabilities` cross-checks a
//! component's WIT imports against its declared `[capabilities]`; `types`
//! holds the serde shapes and `LoadedManifest`; `error` the error types.
//! A manifest with no `[capabilities]` section falls back to all-required,
//! with a deprecation warning.

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
