//! Data structures: `Manifest`, sections, and `LoadedManifest`.
//!
//! Plain serde shapes plus the core-capability list. The parsing
//! and validation logic lives in [`mod@super::load`]; capability enforcement
//! in [`super::capabilities`].

use serde::Deserialize;

/// Core capability names: the `nexum:host` interfaces the `event-module`
/// world links into every module linker. The `http` capability is not a
/// `nexum:host` interface (it gates `wasi:http/*` imports) and is handled
/// separately by the registry. Domain-extension capabilities (e.g.
/// cow-api) are not listed here; each extension contributes its own
/// namespace to the [`super::capabilities::CapabilityRegistry`] at the
/// composition root.
pub const CORE_CAPABILITIES: &[&str] = &[
    "chain",
    "identity",
    "local-store",
    "remote-store",
    "messaging",
    "logging",
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
    /// schema; 0.2 implements `block` and `chain-log` kinds, `cron` is
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
    /// Chain-log events matching `address` + topic-0. Fan-out is
    /// per-module - the supervisor opens one subscription per
    /// `[[subscription]]` entry and tags emitted events with the
    /// owning module.
    #[serde(rename = "chain-log")]
    ChainLog {
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
    /// Router-polled intent status transitions, delivered as
    /// `intent-status` events. Fan-out is shared: the router polls each
    /// installed adapter once per cadence and every subscribed module
    /// receives the transition, filtered by `venue` when set.
    #[serde(rename = "intent-status")]
    IntentStatus {
        /// Restrict delivery to transitions from this venue id.
        /// Absent means transitions from every venue.
        #[serde(default)]
        venue: Option<String>,
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
    /// Which component kind this manifest describes. Defaults to
    /// `event-module` so every existing `module.toml` keeps its meaning;
    /// a venue adapter sets `kind = "venue-adapter"`. The supervisor picks
    /// the bindgen and the scoped capability set from this discriminator.
    #[serde(default)]
    pub kind: ModuleKind,
}

/// The component kind a manifest declares. The runtime carries two: the
/// original event-module over the six core primitives, and the venue
/// adapter over scoped transport only. Defaulting to `event-module`
/// preserves the meaning of every manifest written before adapters
/// existed.
#[derive(Debug, Deserialize, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ModuleKind {
    /// Event-driven automation over the six core primitives.
    #[default]
    EventModule,
    /// A single-venue adapter over scoped chain, messaging, and HTTP.
    VenueAdapter,
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
    /// Hosts wasi:http outgoing requests may target. Each entry is
    /// either an exact hostname or a `*.suffix` wildcard.
    pub http_allowlist: Vec<String>,
    /// `[config]` flattened to `(key, stringified-value)` pairs ready to
    /// hand to a module's `init`. TOML scalars (string, integer, float,
    /// boolean) become their text form. Arrays and tables are rendered as
    /// their TOML representation.
    pub config: Vec<(String, String)>,
}
