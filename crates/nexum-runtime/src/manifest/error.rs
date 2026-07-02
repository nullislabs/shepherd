//! Error types for manifest parsing and capability enforcement.

use strum::IntoStaticStr;
use thiserror::Error;

use super::types::KNOWN_CAPABILITIES;

/// Errors returned while loading or validating a manifest.
///
/// `IntoStaticStr` exposes the snake_case variant name as a
/// `&'static str` for the manifest-loader's `tracing::warn!` /
/// `metrics::counter!` call sites.
#[derive(Debug, Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum ParseError {
    /// Failed to read the manifest file from disk.
    #[error("manifest: i/o: {0}")]
    Io(#[from] std::io::Error),
    /// Manifest file was not valid TOML.
    #[error("manifest: parse: {0}")]
    Toml(#[from] toml::de::Error),
    /// `[capabilities].required` or `.optional` listed a capability
    /// the engine does not recognise.
    #[error("manifest: unknown capability {name:?} in [capabilities].required (known: {known})",
        name = .0,
        known = KNOWN_CAPABILITIES.join(", ")
    )]
    UnknownCapability(String),
}

/// Error returned when a component's WIT imports exceed its declared capabilities.
#[derive(Debug, Error)]
#[error(
    "component imports `{capability}` ({wit_import}) but it is not listed in \
     [capabilities].required or [capabilities].optional"
)]
pub struct CapabilityViolation {
    /// Capability name (e.g. `"remote-store"`).
    pub capability: String,
    /// Full WIT import name as it appeared in the component (e.g.
    /// `"nexum:host/remote-store@0.2.0"`).
    pub wit_import: String,
}
