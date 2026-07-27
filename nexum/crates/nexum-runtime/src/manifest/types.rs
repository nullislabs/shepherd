//! Serde shapes: `Manifest`, its sections, and `LoadedManifest`.

use std::collections::BTreeMap;
use std::fmt;

use serde::Deserialize;
use serde::de::Error as _;

/// Core capability names: the `nexum:host` interfaces linked into every
/// module. `http` is gated separately (it gates `wasi:http/*`), and
/// extensions register their own namespaces.
pub const CORE_CAPABILITIES: &[&str] = &nexum_world::CORE_IFACES;

#[derive(Debug, Deserialize, Default)]
pub struct Manifest {
    #[serde(default)]
    pub module: ModuleSection,
    #[serde(default)]
    pub capabilities: Option<CapabilitiesSection>,
    #[serde(default)]
    pub config: toml::Table,
    /// Event subscriptions wired before `_init`. `block` and `chain-log`
    /// are dispatched; `cron` is parsed and ignored.
    #[serde(default, rename = "subscription")]
    pub subscriptions: Vec<Subscription>,
    /// Extension-owned sections (every non-core top-level key), parsed
    /// opaquely and routed to the wired extensions; a section no extension
    /// claims is refused at boot.
    #[serde(flatten)]
    pub extensions: ExtensionSections,
}

/// Extension-owned manifest sections, keyed by top-level name. Opaque
/// to the runtime; each claiming extension parses its own.
pub type ExtensionSections = BTreeMap<String, toml::Value>;

/// One `[[subscription]]` table. The `kind` field discriminates; an
/// unknown kind parses as [`Subscription::Extension`] and is validated at
/// boot against the wired extensions' declared kinds.
#[derive(Debug, Clone)]
pub enum Subscription {
    /// New-block events; one subscription per chain id, fanned out to every
    /// module watching that chain.
    Block {
        /// EVM chain id.
        chain_id: u64,
    },
    /// Chain-log events matching `address` + topic-0; one subscription per
    /// entry, tagged with the owning module.
    ChainLog {
        /// EVM chain id.
        chain_id: u64,
        /// Contract address as `0x`-prefixed 20-byte hex. Optional.
        address: Option<String>,
        /// Topic-0 filter as `0x`-prefixed 32-byte hex; absent matches
        /// every event from the address(es).
        event_signature: Option<String>,
        /// Persist a durable per-subscription cursor and re-open from just
        /// after the last dispatched block instead of head. Delivery is
        /// then at-least-once; the module must tolerate redelivery.
        resume: bool,
        /// Backfill cap for a `resume` subscription, in blocks. `None`
        /// backfills the whole gap; set it only for a consumer that
        /// tolerates dropping the oldest missed blocks.
        max_lookback: Option<u64>,
    },
    /// Cron-scheduled tick; parsed but not dispatched (the supervisor
    /// warns).
    Cron {
        /// Standard 5-field cron expression.
        #[allow(dead_code)]
        schedule: String,
    },
    /// An extension-owned event kind. Delivered when the kind matches and
    /// every filter pair is present in the event's attributes.
    Extension {
        /// The extension-declared subscription kind.
        kind: String,
        /// Attribute filters; empty admits every event of the kind.
        filters: BTreeMap<String, String>,
    },
}

/// Core subscription kinds parsed by shape; others fall through to
/// [`Subscription::Extension`].
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
    /// Component kind; defaults to the worker (`event-module`), a provider
    /// names its registered kind.
    #[serde(default)]
    pub kind: ComponentKind,
    /// Per-module resource overrides; each unset field inherits the engine
    /// `[limits]` default.
    #[serde(default)]
    pub resources: ResourceSection,
}

/// The worker kind's manifest spelling.
pub const WORKER_KIND: &str = "event-module";

/// Component kind a manifest declares: the worker, or a provider spelling
/// an extension registers. Defaults to the worker; an unregistered spelling
/// is refused at boot.
#[derive(Debug, Deserialize, Default, Clone, PartialEq, Eq)]
#[serde(from = "String")]
pub enum ComponentKind {
    /// Event-driven worker (`event-module`).
    #[default]
    Worker,
    /// A provider, named by its manifest spelling.
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
/// `[module.resources]` overrides; each unset field keeps the engine
/// `[limits]` default.
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
    /// `[config]` flattened to `(key, stringified-value)` pairs for a
    /// module's `init`. Scalars become their text form; arrays and tables
    /// their TOML representation.
    pub config: Vec<(String, String)>,
}
