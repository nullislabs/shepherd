//! Data structures: `Manifest`, sections, and `LoadedManifest`.
//!
//! Plain serde shapes plus the `KNOWN_CAPABILITIES` registry. The parsing
//! and validation logic lives in [`mod@super::load`]; capability enforcement
//! in [`super::capabilities`].

use serde::Deserialize;

/// Capability names recognised by the 0.2 reference engine. Matches the
/// interfaces the `shepherd` world links into the linker.
pub const KNOWN_CAPABILITIES: &[&str] = &[
    "chain",
    "identity",
    "local-store",
    "remote-store",
    "messaging",
    "logging",
    "clock",
    "random",
    "http",
    // Domain-extension caps (provided by the shepherd world only):
    "cow-api",
];

#[derive(Debug, Deserialize, Default)]
pub struct Manifest {
    #[serde(default)]
    pub module: ModuleSection,
    #[serde(default)]
    pub capabilities: Option<CapabilitiesSection>,
    #[serde(default)]
    pub config: toml::Table,
    /// Event subscriptions the runtime wires before calling
    /// `_init`. See `docs/02-modules-events-packaging.md` for the
    /// schema; 0.2 implements `block` and `log` kinds, `cron` is
    /// parsed and ignored (deferred to 0.3).
    #[serde(default, rename = "subscription")]
    pub subscriptions: Vec<Subscription>,
}

/// One `[[subscription]]` table in `module.toml`.
///
/// The discriminator is the `kind` field; remaining fields are
/// validated per-kind by the supervisor. Unknown kinds are surfaced
/// at load time so a typo does not silently disable an event source.
#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Subscription {
    /// New-block events. Fan-out is shared per chain - the
    /// supervisor opens one subscription per chain id and routes to
    /// every module that asked for blocks on that chain.
    Block {
        /// EVM chain id.
        chain_id: u64,
    },
    /// Log events matching `address` + topic-0. Fan-out is
    /// per-module - the supervisor opens one subscription per
    /// `[[subscription]]` entry and tags emitted events with the
    /// owning module.
    Log {
        /// EVM chain id.
        chain_id: u64,
        /// Contract address as `0x`-prefixed 20-byte hex. Optional.
        #[serde(default)]
        address: Option<String>,
        /// Topic-0 of the event the module wants to consume. `0x`-
        /// prefixed 32-byte hex. Optional - when absent the
        /// subscription matches every event from the address(es).
        #[serde(default)]
        event_signature: Option<String>,
    },
    /// Cron-scheduled tick. 0.2 parses but does not dispatch; the
    /// supervisor emits a warning so the operator knows the
    /// declaration is currently inert. `schedule` is preserved so a
    /// 0.3 dispatcher can pick it up without re-parsing the manifest.
    Cron {
        /// Standard 5-field cron expression.
        #[allow(dead_code)]
        schedule: String,
    },
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)] // version + component parsed for future 0.3 hash-verification.
pub struct ModuleSection {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub component: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct CapabilitiesSection {
    #[serde(default)]
    pub required: Vec<String>,
    #[serde(default)]
    pub optional: Vec<String>,
    #[serde(default)]
    pub http: Option<HttpSection>,
}

#[derive(Debug, Deserialize, Default)]
pub struct HttpSection {
    #[serde(default)]
    pub allow: Vec<String>,
}

/// Loaded + validated manifest, plus the data the engine needs to
/// instantiate a module.
#[derive(Debug)]
pub struct LoadedManifest {
    pub manifest: Manifest,
    /// Hosts to allow for `http::fetch`. Each entry is either an exact
    /// hostname or a `*.suffix` wildcard.
    pub http_allowlist: Vec<String>,
    /// `[config]` flattened to `(key, stringified-value)` pairs ready to
    /// hand to a module's `init`. TOML scalars (string, integer, float,
    /// boolean) become their text form. Arrays and tables are rendered as
    /// their TOML representation.
    pub config: Vec<(String, String)>,
}
