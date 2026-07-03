//! The `[extensions.cow]` config table, owned by this extension.
//!
//! `engine.toml` stays domain-free: the engine hands every
//! `[extensions.<name>]` table to the composition root verbatim, and
//! this module parses the `cow` one. The deprecated
//! `[chains.<id>] orderbook_url` location still resolves (with a
//! boot-time warning) so existing deployments keep working.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use alloy_chains::Chain;
use nexum_runtime::engine_config::EngineConfig;
use serde::Deserialize;
use strum::IntoStaticStr;
use thiserror::Error;
use tracing::warn;

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
    /// The deprecated `[chains.<id>] orderbook_url` key held a
    /// non-string value.
    #[error(
        "[chains.{chain_id}] orderbook_url must be a string; the key is also deprecated - \
         move it to [extensions.cow.orderbook_urls]"
    )]
    LegacyType { chain_id: u64 },
}

impl TryFrom<&EngineConfig> for CowConfig {
    type Error = CowConfigError;

    /// Parse `[extensions.cow]`, then fold in the deprecated
    /// `[chains.<id>] orderbook_url` location. Each legacy key warns
    /// once at boot; the `[extensions.cow]` entry wins when both name
    /// the same chain.
    fn try_from(cfg: &EngineConfig) -> Result<Self, Self::Error> {
        let mut parsed: Self = match cfg.extensions.get("cow") {
            Some(section) => section.clone().try_into()?,
            None => Self::default(),
        };
        // Sort by numeric id so the warning order is deterministic
        // (`Chain` is not `Ord`).
        let mut legacy: Vec<_> = cfg
            .chains
            .iter()
            .filter_map(|(chain, c)| c.extra.get("orderbook_url").map(|v| (*chain, v)))
            .collect();
        legacy.sort_by_key(|(c, _)| c.id());
        for (chain, value) in legacy {
            let chain_id = chain.id();
            let Some(url) = value.as_str() else {
                return Err(CowConfigError::LegacyType { chain_id });
            };
            match parsed.orderbook_urls.entry(chain) {
                Entry::Occupied(_) => warn!(
                    chain_id,
                    "deprecated [chains.<id>] orderbook_url is ignored: \
                     [extensions.cow.orderbook_urls] also names this chain and wins; \
                     remove the old key"
                ),
                Entry::Vacant(slot) => {
                    warn!(
                        chain_id,
                        "[chains.<id>] orderbook_url is deprecated; \
                         move it to [extensions.cow.orderbook_urls]"
                    );
                    slot.insert(url.to_owned());
                }
            }
        }
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests;
