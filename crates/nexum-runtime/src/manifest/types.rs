//! Data structures: `Manifest`, sections, and `LoadedManifest`.
//!
//! Plain serde shapes plus the core-capability list. The parsing
//! and validation logic lives in [`mod@super::load`]; capability enforcement
//! in [`super::capabilities`].

use std::collections::BTreeMap;
use std::fmt;

use serde::Deserialize;
use serde::de::Error as _;

/// Core capability names: the `nexum:host` interfaces the `event-module`
/// world links into every module linker. The `http` capability is not a
/// `nexum:host` interface (it gates `wasi:http/*` imports) and is handled
/// separately by the registry. Domain-extension capabilities are not
/// listed here; each extension contributes its own namespace to the
/// [`super::capabilities::CapabilityRegistry`] at the composition root.
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
/// validated per-kind by the supervisor. A kind outside the core set
/// parses as [`Subscription::Extension`] and is validated at boot
/// against the kinds the wired extensions declare, so a typo still
/// fails loudly rather than silently disabling an event source.
#[derive(Debug, Clone)]
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
    ChainLog {
        /// EVM chain id.
        chain_id: u64,
        /// Contract address as `0x`-prefixed 20-byte hex. Optional.
        address: Option<String>,
        /// Topic-0 of the event the module wants to consume. `0x`-
        /// prefixed 32-byte hex. Optional - when absent the
        /// subscription matches every event from the address(es).
        event_signature: Option<String>,
        /// Resume across engine restarts. When `true` the host persists a
        /// durable per-subscription cursor and re-opens the log poller
        /// from just after the last dispatched block, instead of at the
        /// current head. Delivery is then at-least-once, so the module must
        /// tolerate redelivery (the keeper idempotency journal already
        /// dedups it).
        resume: bool,
        /// Optional cap on how far back a `resume` subscription will
        /// backfill, in blocks. `None` (the default) backfills the entire
        /// gap with no loss; set it only for a consumer that explicitly
        /// tolerates dropping the oldest missed blocks.
        max_lookback: Option<u64>,
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
    /// An extension-owned event kind. Every non-`kind` key is a string
    /// filter matched against the event's routing attributes: an event
    /// is delivered when its kind matches and every filter pair is
    /// present in the event's attributes.
    Extension {
        /// The extension-declared subscription kind.
        kind: String,
        /// Attribute filters; empty admits every event of the kind.
        filters: BTreeMap<String, String>,
    },
}

/// The core subscription kinds, parsed by shape. Any other kind falls
/// through to [`Subscription::Extension`].
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum CoreSubscription {
    Block {
        chain_id: u64,
    },
    #[serde(rename = "chain-log")]
    ChainLog {
        chain_id: u64,
        #[serde(default)]
        address: Option<String>,
        #[serde(default)]
        event_signature: Option<String>,
        #[serde(default)]
        resume: bool,
        #[serde(default)]
        max_lookback: Option<u64>,
    },
    Cron {
        schedule: String,
    },
}

impl From<CoreSubscription> for Subscription {
    fn from(sub: CoreSubscription) -> Self {
        match sub {
            CoreSubscription::Block { chain_id } => Self::Block { chain_id },
            CoreSubscription::ChainLog {
                chain_id,
                address,
                event_signature,
                resume,
                max_lookback,
            } => Self::ChainLog {
                chain_id,
                address,
                event_signature,
                resume,
                max_lookback,
            },
            CoreSubscription::Cron { schedule } => Self::Cron { schedule },
        }
    }
}

impl<'de> Deserialize<'de> for Subscription {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let table = toml::Table::deserialize(deserializer)?;
        let Some(kind) = table.get("kind").and_then(toml::Value::as_str) else {
            return Err(D::Error::missing_field("kind"));
        };
        match kind {
            "block" | "chain-log" | "cron" => toml::Value::Table(table.clone())
                .try_into::<CoreSubscription>()
                .map(Into::into)
                .map_err(D::Error::custom),
            _ => {
                let kind = kind.to_owned();
                let mut filters = BTreeMap::new();
                for (key, value) in table {
                    if key == "kind" {
                        continue;
                    }
                    let Some(value) = value.as_str() else {
                        return Err(D::Error::custom(format!(
                            "subscription filter `{key}` must be a string"
                        )));
                    };
                    filters.insert(key, value.to_owned());
                }
                Ok(Self::Extension { kind, filters })
            }
        }
    }
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
    /// Which component kind this manifest describes. Defaults to the
    /// worker kind (`event-module`) so every existing `module.toml` keeps
    /// its meaning; a provider names its registered kind. The supervisor
    /// resolves the boot path from this discriminator.
    #[serde(default)]
    pub kind: ComponentKind,
    /// Per-module resource overrides; each unset field inherits the engine
    /// `[limits]` default.
    #[serde(default)]
    pub resources: ResourceSection,
}

/// The worker kind's manifest spelling.
pub const WORKER_KIND: &str = "event-module";

/// The component kind a manifest declares: the core worker kind, or the
/// manifest spelling of a provider kind an extension registers. Defaults
/// to the worker so every manifest written before providers existed keeps
/// its meaning; an unregistered provider spelling is refused at boot,
/// where the registered kinds are known.
#[derive(Debug, Deserialize, Default, Clone, PartialEq, Eq)]
#[serde(from = "String")]
pub enum ComponentKind {
    /// Event-driven worker over the six core primitives (`event-module`).
    #[default]
    Worker,
    /// A provider the host holds behind a serialised actor, named by its
    /// manifest spelling.
    Provider(String),
}

impl From<String> for ComponentKind {
    fn from(kind: String) -> Self {
        if kind == WORKER_KIND {
            Self::Worker
        } else {
            Self::Provider(kind)
        }
    }
}

impl fmt::Display for ComponentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Worker => f.write_str(WORKER_KIND),
            Self::Provider(kind) => f.write_str(kind),
        }
    }
}
/// `[module.resources]` overrides layered over the engine `[limits]`
/// defaults. Every field is optional; an unset field keeps the default.
#[derive(Debug, Deserialize, Default)]
pub struct ResourceSection {
    /// Linear-memory cap, in bytes.
    #[serde(default)]
    pub max_memory_bytes: Option<usize>,
    /// Fuel granted per event dispatch.
    #[serde(default)]
    pub max_fuel_per_event: Option<u64>,
    /// Local-store byte quota (key + value bytes).
    #[serde(default)]
    pub max_state_bytes: Option<u64>,
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
