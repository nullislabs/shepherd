//! Error types for manifest parsing and capability enforcement.

use strum::IntoStaticStr;
use thiserror::Error;

/// Errors from loading or validating a manifest.
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
    /// A declared capability the engine does not recognise.
    #[error("manifest: unknown capability {name:?} in [capabilities] (known: {known})")]
    UnknownCapability {
        /// The unrecognised name.
        name: String,
        /// Comma-joined recognised capability names.
        known: String,
    },
    /// `[module].name` contains `/`, `\`, or `..`, so it could escape the
    /// state directory.
    #[error("manifest: [module].name {0:?} must not contain '/', '\\', or '..'")]
    InvalidModuleName(String),
}

/// A capability-bearing WIT import the manifest did not declare.
#[derive(Debug, Error)]
#[error(
    "component imports `{capability}` ({wit_import}) but it is not listed in \
     [capabilities].required or [capabilities].optional"
)]
pub struct CapabilityViolation {
    /// Capability name.
    pub capability: String,
    /// Full WIT import name.
    pub wit_import: String,
}

/// A component's WIT imports exceed its declared capabilities.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CapabilityError {
    /// A gated import was not declared in `[capabilities]`.
    #[error(transparent)]
    Undeclared(#[from] CapabilityViolation),
    /// An unrecognised `wasi:` interface was imported; refused fail-closed.
    #[error(
        "component imports unrecognised WASI interface `{wit_import}`; \
         undeclared WASI is refused by default"
    )]
    UnknownWasi {
        /// Full WIT import name.
        wit_import: String,
    },
}
