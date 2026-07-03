//! The `[extensions.cow]` config table, owned by this extension.
//!
//! `engine.toml` stays domain-free: the engine hands every
//! `[extensions.<name>]` table to the composition root verbatim, and
//! this module parses the `cow` one.

use std::collections::HashMap;

use alloy_chains::Chain;
use nexum_runtime::engine_config::EngineConfig;
use serde::Deserialize;
use strum::IntoStaticStr;
use thiserror::Error;

/// The `[extensions.cow]` table from `engine.toml`.
///
/// ```toml
/// [extensions.cow.orderbook_urls]
/// 11155111 = "http://localhost:9999"
/// ```
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CowConfig {
    /// Per-chain orderbook base URL overrides keyed by EIP-155 chain
    /// id (numeric or named, as with `[chains.<id>]`). Chains without
    /// an entry use the canonical `cowprotocol::Chain` URL.
    #[serde(default)]
    pub orderbook_urls: HashMap<Chain, String>,
}

/// Boot-time errors from parsing the cow extension's config.
///
/// `IntoStaticStr` exposes the snake_case variant name for
/// structured-log `error_kind` fields, matching the other host-side
/// error enums.
#[derive(Debug, Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum CowConfigError {
    /// The `[extensions.cow]` table failed to deserialize.
    #[error("parse [extensions.cow]: {0}")]
    Section(#[from] toml::de::Error),
}

impl TryFrom<&EngineConfig> for CowConfig {
    type Error = CowConfigError;

    /// Parse the `[extensions.cow]` table. An absent table yields an
    /// empty override set, so every chain uses its canonical URL.
    fn try_from(cfg: &EngineConfig) -> Result<Self, Self::Error> {
        match cfg.extensions.get("cow") {
            Some(section) => Ok(section.clone().try_into()?),
            None => Ok(Self::default()),
        }
    }
}

#[cfg(test)]
mod tests;
