//! Engine-side runtime configuration.
//!
//! Distinct from `module.toml` (module manifest): this file describes
//! the *engine*'s I/O wiring - chain RPC endpoints and the on-disk
//! location of the `local-store` database. Both are required for the
//! 0.2 reference engine to do anything other than print stubs.
//!
//! Lookup order:
//!
//! 1. `--engine-config <path>` CLI flag (future), or third positional
//!    argument today;
//! 2. `engine.toml` in the current working directory;
//! 3. defaults - no chains configured, `state_dir = ./data`.
//!
//! A missing config is OK for the example module (it only logs); for
//! the chain-backed capabilities it surfaces as a `fault.unsupported`
//! so guests learn early.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use alloy_chains::Chain;
use serde::Deserialize;
use strum::IntoStaticStr;
use thiserror::Error;
use tracing::{info, warn};

use crate::runtime::dispatch_rate::{
    DEFAULT_DISPATCH_BURST, DEFAULT_DISPATCH_REFILL_PER_SEC, DispatchRatePolicy,
};
use crate::runtime::poison_policy::{POISON_MAX_FAILURES, POISON_WINDOW, PoisonPolicy};

/// Default per-caller submission budget within [`DEFAULT_QUOTA_WINDOW`].
pub const DEFAULT_QUOTA_MAX_CHARGES: u32 = 256;
/// Default sliding window the per-caller submission budget is counted over.
pub const DEFAULT_QUOTA_WINDOW: Duration = Duration::from_secs(60);
/// Default cap on receipts under status watch at once.
pub const DEFAULT_WATCH_MAX_ENTRIES: usize = 1024;
/// Default lifetime of one status watch before it is evicted unreported.
pub const DEFAULT_WATCH_EXPIRY: Duration = Duration::from_secs(86_400);

/// Per-caller submission quota toward installed providers. Both a
/// forwarded submission and a charged decode failure consume one unit;
/// the window slides so a caller's budget refills as old charges age out.
/// Resolved from `[limits.quota]`; the extension service that meters
/// callers consumes it.
#[derive(Debug, Clone, Copy)]
pub struct SubmitQuota {
    /// Maximum charges a single caller may accrue within `window`.
    pub max_charges: u32,
    /// Sliding window the charges are counted across.
    pub window: Duration,
}

impl SubmitQuota {
    /// Pair a budget with the window it is counted over.
    pub const fn new(max_charges: u32, window: Duration) -> Self {
        Self {
            max_charges,
            window,
        }
    }
}

impl Default for SubmitQuota {
    fn default() -> Self {
        Self::new(DEFAULT_QUOTA_MAX_CHARGES, DEFAULT_QUOTA_WINDOW)
    }
}

/// Bounds on a provider status-watch set. The cap bounds the per-cadence
/// poll fan-out; the expiry evicts a watch whose provider has gone silent
/// for a whole window. Resolved from `[limits.watch]`.
#[derive(Debug, Clone, Copy)]
pub struct WatchLimit {
    /// Maximum receipts under status watch at once.
    pub max_entries: usize,
    /// How long a watch survives without a successful poll before it is
    /// evicted unreported.
    pub expiry: Duration,
}

impl WatchLimit {
    /// Pair a cap with the per-entry expiry.
    pub const fn new(max_entries: usize, expiry: Duration) -> Self {
        Self {
            max_entries,
            expiry,
        }
    }
}

impl Default for WatchLimit {
    fn default() -> Self {
        Self::new(DEFAULT_WATCH_MAX_ENTRIES, DEFAULT_WATCH_EXPIRY)
    }
}

/// Errors surfaced by [`load_or_default`].
///
/// Library-side modules must not propagate `anyhow::Error`; the rust
/// idiomatic rubric reserves `anyhow` for `main.rs` and
/// `supervisor.rs` top-level dispatch. The variants carry the
/// upstream error via `#[from]` so the caller in `main.rs` (which
/// uses `anyhow`) gets a free conversion through `?`.
///
/// `IntoStaticStr` exposes the snake_case variant name for metric
/// labels and structured-log `error_kind` fields.
#[derive(Debug, Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum EngineConfigError {
    /// Failed to read the config file from disk.
    #[error("read engine config: {0}")]
    Io(#[from] std::io::Error),
    /// Config file was unparseable as TOML.
    #[error("parse engine config: {0}")]
    Toml(#[from] toml::de::Error),
    /// `${VAR}` env-var substitution failed (missing, malformed, or unclosed).
    #[error("engine config env-var substitution failed: {0}")]
    Substitute(#[from] EnvVarError),
}

/// Engine-side configuration loaded from `engine.toml`.
#[derive(Debug, Default, Deserialize)]
pub struct EngineConfig {
    #[serde(default)]
    pub engine: EngineSection,
    /// Per-module wasmtime resource limits. Applies uniformly to every
    /// module; per-module overrides land in 0.3.
    #[serde(default)]
    pub limits: ModuleLimits,
    /// Per-chain RPC URLs keyed by EIP-155 chain id. Numeric TOML keys
    /// (`[chains.11155111]`) stay canonical; named keys
    /// (`[chains.sepolia]`) also parse, since the key string is handed
    /// to `Chain`'s `FromStr`. `Chain` is not `Ord`, so this is a
    /// `HashMap`; call sites that need deterministic output sort by
    /// `Chain::id()`.
    #[serde(default)]
    pub chains: HashMap<Chain, ChainConfig>,
    /// Opaque `[extensions.<name>]` tables. The engine never
    /// interprets these; each extension parses its own table at the
    /// composition root.
    #[serde(default)]
    pub extensions: HashMap<String, toml::Value>,
    /// Modules the supervisor should boot. Each entry resolves a
    /// `(component.wasm, module.toml)` pair on the local filesystem
    /// for 0.2 - content-addressed resolution (Swarm / OCI /
    /// `[[content.sources]]`) lands in 0.3 per
    /// `docs/03-module-discovery.md`.
    #[serde(default)]
    pub modules: Vec<ModuleEntry>,
    /// Provider components the supervisor should boot alongside the
    /// modules. Each entry resolves a `(component.wasm, module.toml)` pair
    /// like a module, but the operator scopes its transport here rather
    /// than in the provider's own manifest: the installer of a provider,
    /// not its author, decides which hosts and messaging topics it may
    /// reach.
    #[serde(default)]
    pub adapters: Vec<AdapterEntry>,
}

/// One `[[modules]]` table from `engine.toml`.
///
/// Both fields are filesystem paths in 0.2. `manifest` defaults to
/// `module.toml` next to `path` if omitted, matching the bundle layout
/// in `docs/02-modules-events-packaging.md`.
#[derive(Debug, Deserialize)]
pub struct ModuleEntry {
    /// Path to the compiled `.wasm` component.
    pub path: std::path::PathBuf,
    /// Path to the module's `module.toml`. Defaults to `<path-parent>/module.toml`.
    #[serde(default)]
    pub manifest: Option<std::path::PathBuf>,
}

/// One `[[adapters]]` table from `engine.toml`.
///
/// `path` and `manifest` mirror [`ModuleEntry`]; `manifest` defaults to a
/// sibling `module.toml`. The two scope fields are the operator's grant of
/// the adapter's transport: `http_allow` is the outbound HTTP host
/// allowlist the adapter's wasi:http gate enforces, and `messaging_topics`
/// scopes the messaging content topics it may publish to. Both default
/// empty; an empty `http_allow` denies every outbound request, and an
/// empty `messaging_topics` leaves messaging unscoped for parity with the
/// module default (the messaging backend itself is deferred).
#[derive(Debug, Deserialize)]
pub struct AdapterEntry {
    /// Path to the compiled `.wasm` adapter component.
    pub path: std::path::PathBuf,
    /// Path to the adapter's `module.toml`. Defaults to `<path-parent>/module.toml`.
    #[serde(default)]
    pub manifest: Option<std::path::PathBuf>,
    /// Outbound HTTP host allowlist granted to this adapter. Each entry is
    /// either an exact hostname or a `*.suffix` wildcard, matched the same
    /// way as a module's `[capabilities.http].allow`.
    #[serde(default)]
    pub http_allow: Vec<String>,
    /// Messaging content topics this adapter may reach.
    #[serde(default)]
    pub messaging_topics: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct EngineSection {
    #[serde(default = "default_state_dir")]
    pub state_dir: PathBuf,
    /// `tracing_subscriber::EnvFilter`-compatible directive. Defaults to
    /// `info` when absent; `RUST_LOG` overrides at process start.
    #[serde(default = "default_log_level")]
    pub log_level: String,
    /// Prometheus metrics exporter wiring. Absent table =
    /// disabled (the engine still installs the recorder so call sites
    /// stay live but no HTTP listener binds).
    #[serde(default)]
    pub metrics: MetricsSection,
    /// Concurrency for the chain-log poller's per-block `eth_getLogs`
    /// during backfill; higher catches up faster at more node load.
    /// `0` is treated as `1` by alloy.
    #[serde(default = "default_log_backfill_concurrency")]
    pub log_backfill_concurrency: usize,
}

impl Default for EngineSection {
    fn default() -> Self {
        Self {
            state_dir: default_state_dir(),
            log_level: default_log_level(),
            metrics: MetricsSection::default(),
            log_backfill_concurrency: default_log_backfill_concurrency(),
        }
    }
}

fn default_log_backfill_concurrency() -> usize {
    16
}

/// `[engine.metrics]` config. When `enabled = true` the engine starts
/// a Prometheus HTTP exporter on `bind_addr` and serves `/metrics`.
///
/// Default: disabled. Operators opt in explicitly so the M3 / M4
/// runbook smoke runs do not bind a port unintentionally.
#[derive(Debug, Deserialize)]
pub struct MetricsSection {
    #[serde(default)]
    pub enabled: bool,
    /// IPv4 / IPv6 socket address to bind. Default `127.0.0.1:9100`.
    #[serde(default = "default_metrics_bind")]
    pub bind_addr: String,
}

impl Default for MetricsSection {
    fn default() -> Self {
        Self {
            enabled: false,
            bind_addr: default_metrics_bind(),
        }
    }
}

fn default_metrics_bind() -> String {
    "127.0.0.1:9100".to_owned()
}

#[derive(Debug, Deserialize)]
pub struct ChainConfig {
    /// JSON-RPC endpoint. `ws://` and `wss://` engage alloy's pubsub
    /// transport (required for `eth_subscribe`); `http://` and `https://`
    /// engage the HTTP transport (request/response only).
    pub rpc_url: String,
    /// Per-request timeout for `chain::request` JSON-RPC calls, in
    /// seconds. Does not apply to `eth_subscribe` streams or the log
    /// poller (both long-lived by design). Default: 30 s. `0` is
    /// rejected at boot - every call would time out immediately.
    #[serde(default = "default_chain_request_timeout_secs")]
    pub request_timeout_secs: u64,
}

fn default_chain_request_timeout_secs() -> u64 {
    30
}

/// Default fuel budget per `on_event` invocation (~1 billion WASM
/// instructions).
const DEFAULT_FUEL_PER_EVENT: u64 = 1_000_000_000;

/// Default per-dispatch wall-clock deadline: the coarse backstop for a
/// dispatch parked in an unmetered host call.
const DEFAULT_EVENT_DEADLINE: Duration = Duration::from_secs(120);

/// Floor for the resolved dispatch deadline.
const MIN_EVENT_DEADLINE: Duration = Duration::from_secs(1);

/// Default linear-memory cap per module store (64 MiB).
const DEFAULT_MEMORY_LIMIT: usize = 64 * 1024 * 1024;

/// Default per-module local-store byte quota (50 MiB).
const DEFAULT_STATE_BYTES: u64 = 50 * 1024 * 1024;

/// Default ceiling on the guest-settable connect timeout. A TCP + TLS
/// connect that has not completed in 10 s is dead; anything longer just
/// parks a host task.
const DEFAULT_HTTP_CONNECT_TIMEOUT_MAX: Duration = Duration::from_secs(10);

/// Default ceiling on the guest-settable first-byte timeout. Generous
/// enough for slow API endpoints without letting one request hold a
/// connection for minutes.
const DEFAULT_HTTP_FIRST_BYTE_TIMEOUT_MAX: Duration = Duration::from_secs(30);

/// Default ceiling on the guest-settable between-bytes timeout.
const DEFAULT_HTTP_BETWEEN_BYTES_TIMEOUT_MAX: Duration = Duration::from_secs(30);

/// Default total deadline on one outgoing exchange, connect through
/// body streaming. Event-driven modules should never hold a request
/// across minutes; the per-phase timeouts above cannot bound a server
/// that trickles bytes forever, this does.
const DEFAULT_HTTP_TOTAL_DEADLINE: Duration = Duration::from_secs(60);

/// Default cap on one incoming response body (16 MiB): a quarter of the
/// default module memory, so a single response cannot dominate the
/// guest heap that has to buffer it.
const DEFAULT_HTTP_RESPONSE_BODY_MAX: u64 = 16 * 1024 * 1024;

/// Default cap on one chain JSON-RPC response body (1 MiB). Large enough
/// for typical read responses (receipts, log batches, contract state),
/// while preventing a misbehaving or adversarial node from filling the
/// guest heap via a single large reply.
const DEFAULT_CHAIN_RESPONSE_MAX_BYTES: usize = 1024 * 1024;

/// Ceiling for the `[limits.http]` millisecond knobs (24 h).
const HTTP_LIMIT_MS_MAX: u64 = 86_400_000;

/// Default per-run log ring budget (256 KiB). Large enough to hold a
/// substantial tail of a run's output for post-mortem, small enough that
/// memory stays bounded at roughly `bytes_per_run * runs_retained *
/// modules`. Each record is charged its message bytes plus a fixed
/// per-record overhead, so a flood of empty lines cannot outgrow the
/// budget. The per-run ceiling is really `max(bytes_per_run,
/// MAX_LINE_BYTES)`: the ring never evicts its sole record, and the stdio
/// writer force-flushes an unterminated line at 1 MiB, so a newline-less
/// flood transiently holds one record up to that size (evicted as soon as
/// a newer record arrives).
const DEFAULT_LOG_BYTES_PER_RUN: usize = 256 * 1024;

/// Default number of past runs retained per module (16). A crash-looping
/// module restarts repeatedly; keeping the last several runs gives
/// history for diagnosis without unbounded growth.
const DEFAULT_LOG_RUNS_RETAINED: usize = 16;

/// Default cadence for provider status polling (5 s). Fast enough that a
/// settling submission is observed within a block time or two, slow
/// enough that per-receipt provider calls stay negligible.
const DEFAULT_STATUS_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Saturate an operator-supplied millisecond knob into [1 ms, 24 h]:
/// zero would fail every request instantly, and huge values overflow
/// timer arithmetic.
fn clamp_http_ms(ms: u64) -> Duration {
    Duration::from_millis(ms.clamp(1, HTTP_LIMIT_MS_MAX))
}

/// Per-module wasmtime resource limits. Every field is optional;
/// omitted values resolve to built-in defaults.
///
/// ```toml
/// [limits]
/// fuel_per_event      = 1_000_000_000
/// event_deadline_secs = 120
/// memory_bytes        = 67_108_864
/// state_bytes         = 52_428_800
///
/// [limits.http]
/// connect_timeout_max_ms       = 10_000
/// first_byte_timeout_max_ms    = 30_000
/// between_bytes_timeout_max_ms = 30_000
/// total_deadline_ms            = 60_000
/// response_body_max_bytes      = 16_777_216
///
/// [limits.chain]
/// response_body_max_bytes = 1_048_576
///
/// [limits.logs]
/// bytes_per_run  = 262_144
/// runs_retained  = 16
///
/// [limits.poison]
/// max_failures = 5
/// window_secs  = 600
///
/// [limits.dispatch]
/// burst          = 256
/// refill_per_sec = 128
/// ```
#[derive(Debug, Default, Deserialize)]
pub struct ModuleLimits {
    /// Fuel budget granted per `on_event` invocation.
    pub fuel_per_event: Option<u64>,
    /// Wall-clock deadline (s) for a dispatch, covering host-call time fuel cannot meter.
    pub event_deadline_secs: Option<u64>,
    /// Linear-memory cap in bytes per module store.
    pub memory_bytes: Option<usize>,
    /// Local-store on-disk byte quota (prefix + key + value + per-entry
    /// overhead) per module.
    pub state_bytes: Option<u64>,
    /// Outbound wasi:http limits.
    #[serde(default)]
    pub http: HttpLimitsSection,
    /// Chain JSON-RPC response size limits.
    #[serde(default)]
    pub chain: ChainLimitsSection,
    /// Per-run log retention limits.
    #[serde(default)]
    pub logs: LogLimitsSection,
    /// Poison-pill quarantine thresholds.
    #[serde(default)]
    pub poison: PoisonLimitsSection,
    /// Per-caller provider submission quota.
    #[serde(default)]
    pub quota: QuotaLimitsSection,
    /// Provider status polling cadence.
    #[serde(default)]
    pub status_poll: StatusPollSection,
    /// Status-watch set bounds.
    #[serde(default)]
    pub watch: WatchLimitsSection,
    /// Per-module dispatch rate-limit thresholds.
    #[serde(default)]
    pub dispatch: DispatchLimitsSection,
}

impl ModuleLimits {
    /// Resolved fuel budget (override or default).
    pub fn fuel(&self) -> u64 {
        self.fuel_per_event.unwrap_or(DEFAULT_FUEL_PER_EVENT)
    }

    /// Resolved memory cap (override or default).
    pub fn memory(&self) -> usize {
        self.memory_bytes.unwrap_or(DEFAULT_MEMORY_LIMIT)
    }

    /// Resolved chain response size cap (override or default). A
    /// degenerate `0` saturates to 1 byte, matching the `logs` /
    /// `poison` sections' zero handling, so resolution never yields a
    /// cap that rejects even an empty body.
    pub fn chain_response_max_bytes(&self) -> usize {
        self.chain
            .response_body_max_bytes
            .map(|b| (b.max(1)) as usize)
            .unwrap_or(DEFAULT_CHAIN_RESPONSE_MAX_BYTES)
    }

    /// Resolved local-store byte quota (override or default).
    pub fn state_bytes(&self) -> u64 {
        self.state_bytes.unwrap_or(DEFAULT_STATE_BYTES)
    }

    /// Resolved per-dispatch wall-clock deadline; an override saturates
    /// up to a 1 s floor.
    pub fn event_deadline(&self) -> Duration {
        self.event_deadline_secs
            .map(|secs| Duration::from_secs(secs).max(MIN_EVENT_DEADLINE))
            .unwrap_or(DEFAULT_EVENT_DEADLINE)
    }

    /// Resolved outbound HTTP limits (overrides or defaults).
    pub fn http(&self) -> OutboundHttpLimits {
        OutboundHttpLimits {
            connect_timeout_max: self
                .http
                .connect_timeout_max_ms
                .map(clamp_http_ms)
                .unwrap_or(DEFAULT_HTTP_CONNECT_TIMEOUT_MAX),
            first_byte_timeout_max: self
                .http
                .first_byte_timeout_max_ms
                .map(clamp_http_ms)
                .unwrap_or(DEFAULT_HTTP_FIRST_BYTE_TIMEOUT_MAX),
            between_bytes_timeout_max: self
                .http
                .between_bytes_timeout_max_ms
                .map(clamp_http_ms)
                .unwrap_or(DEFAULT_HTTP_BETWEEN_BYTES_TIMEOUT_MAX),
            total_deadline: self
                .http
                .total_deadline_ms
                .map(clamp_http_ms)
                .unwrap_or(DEFAULT_HTTP_TOTAL_DEADLINE),
            response_body_max_bytes: self
                .http
                .response_body_max_bytes
                .unwrap_or(DEFAULT_HTTP_RESPONSE_BODY_MAX),
        }
    }

    /// Resolved log retention limits (overrides or defaults). Degenerate
    /// zeroes saturate up to 1 so at least the newest record and run stay
    /// retained; resolution never fails.
    pub fn logs(&self) -> LogRetentionLimits {
        LogRetentionLimits {
            bytes_per_run: self
                .logs
                .bytes_per_run
                .map(|b| b.max(1))
                .unwrap_or(DEFAULT_LOG_BYTES_PER_RUN),
            runs_retained: self
                .logs
                .runs_retained
                .map(|r| r.max(1))
                .unwrap_or(DEFAULT_LOG_RUNS_RETAINED),
        }
    }

    /// Resolved poison-pill thresholds (overrides or production
    /// defaults). Degenerate zeroes saturate up to 1: a zero
    /// `max_failures` would quarantine on the first trap, and a zero
    /// `window` would prune every recorded failure before the check.
    pub fn poison(&self) -> PoisonPolicy {
        PoisonPolicy::new(
            self.poison
                .max_failures
                .map(|n| n.max(1))
                .unwrap_or(POISON_MAX_FAILURES),
            self.poison
                .window_secs
                .map(|s| Duration::from_secs(s.max(1)))
                .unwrap_or(POISON_WINDOW),
        )
    }

    /// Resolved dispatch rate policy; a zero `burst` or `refill_per_sec`
    /// saturates up to 1.
    pub fn dispatch_rate(&self) -> DispatchRatePolicy {
        DispatchRatePolicy::new(
            self.dispatch
                .burst
                .map(|b| b.max(1))
                .unwrap_or(DEFAULT_DISPATCH_BURST),
            self.dispatch
                .refill_per_sec
                .map(|r| r.max(1))
                .unwrap_or(DEFAULT_DISPATCH_REFILL_PER_SEC),
        )
    }

    /// Resolved status-poll cadence (override or default). A zero interval
    /// saturates up to 1 ms so a misconfigured cadence busy-loops a poll
    /// task instead of dividing by zero timer arithmetic.
    pub fn status_poll_interval(&self) -> Duration {
        self.status_poll
            .interval_ms
            .map(|ms| Duration::from_millis(ms.max(1)))
            .unwrap_or(DEFAULT_STATUS_POLL_INTERVAL)
    }

    /// Resolved per-caller submission quota (overrides or defaults). A zero
    /// `max_charges` is saturated up to 1 by the consuming service, so a
    /// misconfigured budget still admits one submission rather than
    /// bricking every provider.
    pub fn quota(&self) -> SubmitQuota {
        SubmitQuota::new(
            self.quota.max_charges.unwrap_or(DEFAULT_QUOTA_MAX_CHARGES),
            self.quota
                .window_secs
                .map(|s| Duration::from_secs(s.max(1)))
                .unwrap_or(DEFAULT_QUOTA_WINDOW),
        )
    }

    /// Resolved status-watch bounds (overrides or defaults). A zero
    /// `max_entries` saturates up to 1 and a zero `expiry_secs` up to 1 s,
    /// so a misconfigured bound still watches one receipt briefly rather
    /// than nothing at all.
    pub fn watch(&self) -> WatchLimit {
        WatchLimit::new(
            self.watch
                .max_entries
                .map(|n| n.max(1))
                .unwrap_or(DEFAULT_WATCH_MAX_ENTRIES),
            self.watch
                .expiry_secs
                .map(|s| Duration::from_secs(s.max(1)))
                .unwrap_or(DEFAULT_WATCH_EXPIRY),
        )
    }
}

/// `[limits.http]` outbound wasi:http limits. Every field is optional;
/// omitted values resolve to built-in defaults, and millisecond values
/// saturate into [1 ms, 24 h]; degenerate values are clamped at resolve time.
///
/// The three `*_timeout_max_ms` fields are ceilings on the matching
/// guest-settable `request-options` timeouts, not the timeouts
/// themselves: a guest value above the ceiling is clamped down, and an
/// unset guest value inherits the ceiling.
#[derive(Debug, Default, Deserialize)]
pub struct HttpLimitsSection {
    /// Ceiling on the guest-settable connect timeout, in milliseconds.
    pub connect_timeout_max_ms: Option<u64>,
    /// Ceiling on the guest-settable first-byte timeout, in milliseconds.
    pub first_byte_timeout_max_ms: Option<u64>,
    /// Ceiling on the guest-settable between-bytes timeout, in milliseconds.
    pub between_bytes_timeout_max_ms: Option<u64>,
    /// Total deadline on one outgoing exchange (connect through body
    /// streaming), in milliseconds.
    pub total_deadline_ms: Option<u64>,
    /// Cap on one incoming response body, in bytes.
    pub response_body_max_bytes: Option<u64>,
}

/// `[limits.chain]` chain JSON-RPC response size limit. Optional;
/// omitted values resolve to the built-in 1 MiB default.
///
/// ```toml
/// [limits.chain]
/// response_body_max_bytes = 1_048_576
/// ```
#[derive(Debug, Default, Deserialize)]
pub struct ChainLimitsSection {
    /// Cap on one chain JSON-RPC response body, in bytes. Named for
    /// symmetry with `[limits.http].response_body_max_bytes`.
    pub response_body_max_bytes: Option<u64>,
}

/// Resolved outbound HTTP limits the wasi:http gate enforces per
/// request. Built by [`ModuleLimits::http`].
#[derive(Debug, Clone, Copy)]
pub struct OutboundHttpLimits {
    /// Ceiling on the guest-settable connect timeout.
    pub connect_timeout_max: Duration,
    /// Ceiling on the guest-settable first-byte timeout.
    pub first_byte_timeout_max: Duration,
    /// Ceiling on the guest-settable between-bytes timeout.
    pub between_bytes_timeout_max: Duration,
    /// Total deadline on one exchange, connect through body streaming.
    pub total_deadline: Duration,
    /// Cap on one incoming response body.
    pub response_body_max_bytes: u64,
}

/// `[limits.logs]` per-run log retention knobs. Both optional; omitted
/// values resolve to built-in defaults and degenerate zeroes saturate up
/// to 1 at resolve time.
///
/// Captured-line levels are fixed, not configurable: guest stdout is
/// recorded at info, stderr at warn, and a supervisor-synthesized panic
/// record at error. A guest panic's stderr copy therefore records at
/// warn while its host-interface and supervisor copies carry error.
#[derive(Debug, Default, Deserialize)]
pub struct LogLimitsSection {
    /// Byte budget for one run's in-memory ring.
    pub bytes_per_run: Option<usize>,
    /// Number of past runs retained per module.
    pub runs_retained: Option<usize>,
}

/// `[limits.poison]` quarantine thresholds. Both optional; omitted
/// values resolve to the production defaults and degenerate zeroes
/// saturate up to 1 at resolve time via [`ModuleLimits::poison`].
///
/// A module that reaches `max_failures` traps within a sliding
/// `window_secs` is quarantined: the check fires at the threshold, not one
/// past it. The supervisor then stops dispatching to the module until an
/// operator-driven engine restart clears the state.
#[derive(Debug, Default, Deserialize)]
pub struct PoisonLimitsSection {
    /// Maximum traps within the window before a module is poisoned.
    pub max_failures: Option<u32>,
    /// Sliding window the traps are counted across, in seconds.
    pub window_secs: Option<u64>,
}

/// `[limits.quota]` per-caller provider submission budget. Both optional;
/// omitted values resolve to the defaults via [`ModuleLimits::quota`].
///
/// A caller (a strategy module, keyed by its namespace) may accrue at most
/// `max_charges` submissions within a sliding `window_secs`; a decode failure
/// charged back to the caller counts the same, so a module feeding garbage
/// bodies exhausts its own budget rather than the provider's fuel.
#[derive(Debug, Default, Deserialize)]
pub struct QuotaLimitsSection {
    /// Maximum submissions (plus charged decode failures) per caller in the
    /// window.
    pub max_charges: Option<u32>,
    /// Sliding window the charges are counted across, in seconds.
    pub window_secs: Option<u64>,
}

/// `[limits.status_poll]` provider status polling cadence. Optional; an
/// omitted value resolves to the built-in default and a degenerate zero
/// saturates up to 1 ms via [`ModuleLimits::status_poll_interval`].
///
/// The cadence is how often the consuming service polls each installed
/// provider's `status` export for the receipts it watches; only observed
/// transitions fan out as events.
#[derive(Debug, Default, Deserialize)]
pub struct StatusPollSection {
    /// Milliseconds between status poll sweeps.
    pub interval_ms: Option<u64>,
}

/// `[limits.watch]` status-watch set bounds. Both optional; omitted
/// values resolve to the defaults via [`ModuleLimits::watch`] and
/// degenerate zeroes saturate up to a usable minimum.
///
/// The consuming service watches each accepted receipt until a terminal
/// status: the cap bounds the per-cadence poll fan-out, and the expiry
/// evicts a watch whose provider never reports one. At the cap a new
/// watch is refused and logged; live watches are never dropped.
#[derive(Debug, Default, Deserialize)]
pub struct WatchLimitsSection {
    /// Maximum receipts under status watch at once.
    pub max_entries: Option<usize>,
    /// Seconds one watch stays live before it is evicted unreported.
    pub expiry_secs: Option<u64>,
}

/// `[limits.dispatch]` per-module dispatch rate-limit knobs. Both
/// optional; omitted values resolve to the production defaults, and a
/// degenerate zero saturates up to 1 via [`ModuleLimits::dispatch_rate`].
#[derive(Debug, Default, Deserialize)]
pub struct DispatchLimitsSection {
    /// Burst allowance: the token-bucket capacity.
    pub burst: Option<u32>,
    /// Sustained dispatch ceiling: tokens replenished per second.
    pub refill_per_sec: Option<u32>,
}

/// Resolved log retention limits the in-memory store enforces. Built by
/// [`ModuleLimits::logs`].
#[derive(Debug, Clone, Copy)]
pub struct LogRetentionLimits {
    /// Byte budget for one run's ring; the oldest records evict first,
    /// but the newest record is never evicted to nothing.
    pub bytes_per_run: usize,
    /// Runs retained per module; the oldest run evicts first.
    pub runs_retained: usize,
}

fn default_state_dir() -> PathBuf {
    PathBuf::from("./data")
}

fn default_log_level() -> String {
    "info".to_owned()
}

/// Read an engine config from disk, returning defaults if the file is
/// missing. Parse errors propagate via [`EngineConfigError`].
pub fn load_or_default(path: Option<&Path>) -> Result<EngineConfig, EngineConfigError> {
    let path = match path {
        Some(p) => p.to_path_buf(),
        None => PathBuf::from("engine.toml"),
    };

    if !path.exists() {
        warn!(
            path = %path.display(),
            "engine.toml not found - running with defaults (no chain RPC endpoints; \
             chain-backed host calls will return Unsupported)"
        );
        return Ok(EngineConfig::default());
    }

    let raw = std::fs::read_to_string(&path)?;
    // Operators reference RPC URLs (which carry API keys) via
    // `${VAR_NAME}` placeholders so the committed `engine.toml` /
    // `engine.docker.toml` stays secret-free. The substitution runs
    // before TOML parse so a missing var fails fast with the exact
    // variable name, not a downstream "invalid URI" several layers
    // deep.
    let substituted = substitute_env_vars(&raw)?;
    let cfg: EngineConfig = toml::from_str(&substituted)?;
    info!(
        path = %path.display(),
        chains = cfg.chains.len(),
        state_dir = %cfg.engine.state_dir.display(),
        "engine config loaded",
    );
    Ok(cfg)
}

/// Replace every `${VAR_NAME}` token in `raw` with the value of the
/// corresponding environment variable. Returns an error naming any
/// missing variable so the operator sees the exact fix.
///
/// Recognised variable names: `[A-Z_][A-Z0-9_]*` (matches shell env
/// var conventions). Anything else inside `${...}` is rejected so a
/// typo doesn't silently pass through.
///
/// Note: substitution runs over the whole TOML text, including
/// comments. This is fine in practice - comments are stripped during
/// the subsequent `toml::from_str` parse, and the only realistic
/// `${VAR}` payload is in string values anyway.
fn substitute_env_vars(raw: &str) -> Result<String, EnvVarError> {
    let mut out = String::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            // Find the closing `}`.
            let start = i + 2;
            let Some(end_offset) = raw[start..].find('}') else {
                return Err(EnvVarError::Unclosed { offset: i });
            };
            let end = start + end_offset;
            let name = &raw[start..end];
            if !is_valid_env_name(name) {
                return Err(EnvVarError::InvalidName {
                    name: name.to_owned(),
                });
            }
            match std::env::var(name) {
                Ok(val) => out.push_str(&val),
                Err(_) => {
                    return Err(EnvVarError::Missing {
                        name: name.to_owned(),
                    });
                }
            }
            i = end + 1;
        } else {
            // Push one UTF-8 char (find the next char boundary).
            let ch = raw[i..]
                .chars()
                .next()
                .expect("byte index is on char boundary");
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    Ok(out)
}

fn is_valid_env_name(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_uppercase() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// `IntoStaticStr` exposes the snake_case variant name for the
/// `tracing::error!` / `metrics::counter!` call sites in `main.rs`
/// when an `engine.toml` substitution fails at boot, matching the
/// pattern used on every other engine-side error enum.
#[derive(Debug, thiserror::Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum EnvVarError {
    #[error(
        "environment variable `{name}` referenced via ${{{name}}} in engine.toml but not set. \
         Export it before launching the engine (e.g. via a `.env` file consumed by `docker compose`)."
    )]
    Missing { name: String },
    #[error(
        "invalid env var name `{name}` inside ${{...}} in engine.toml - names must match \
         [A-Z_][A-Z0-9_]*. Typo, or did you mean `${{{name_upper}}}`?",
        name_upper = name.to_uppercase()
    )]
    InvalidName { name: String },
    #[error(
        "unclosed `${{` at byte offset {offset} in engine.toml - every `${{` needs a matching `}}`."
    )]
    Unclosed { offset: usize },
}

/// Blank the credential-bearing parts of a URL (userinfo, query, fragment, and
/// long API-key path segments) so it is safe to log. Parsing with [`url::Url`]
/// rather than string-splitting is what makes bare query flags (`?token`) and
/// fragments redact; an unparseable url yields a placeholder. Shared by every
/// call site that logs an RPC url.
pub fn redact_url(url: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(url) else {
        return "<unparseable-url>".to_owned();
    };
    if !parsed.username().is_empty() {
        let _ = parsed.set_username("REDACTED");
    }
    if parsed.password().is_some() {
        let _ = parsed.set_password(Some("REDACTED"));
    }
    // Key-in-path shape (Alchemy/Infura): a >20-char segment with no '.'/':' is
    // an API key. Collect owned first - can't hold the read + write borrows.
    let redacted: Option<Vec<String>> = parsed.path_segments().map(|segs| {
        segs.map(|seg| {
            if seg.len() > 20 && !seg.contains('.') && !seg.contains(':') {
                "KEY".to_owned()
            } else {
                seg.to_owned()
            }
        })
        .collect()
    });
    if let Some(segments) = redacted
        && let Ok(mut pm) = parsed.path_segments_mut()
    {
        pm.clear();
        for seg in &segments {
            pm.push(seg);
        }
    }
    if parsed.query().is_some() {
        parsed.set_query(Some("REDACTED"));
    }
    if parsed.fragment().is_some() {
        parsed.set_fragment(Some("REDACTED"));
    }
    parsed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_chain_key_round_trips_to_the_chain() {
        // A named TOML key must deserialize to the same `Chain` the
        // numeric id would, because `toml` forwards the key string to
        // `Chain`'s `FromStr`.
        let cfg: EngineConfig = toml::from_str(
            r#"
[chains.sepolia]
rpc_url = "wss://example.test/sepolia"
"#,
        )
        .expect("named chain key parses");
        assert!(
            cfg.chains.contains_key(&Chain::sepolia()),
            "the [chains.sepolia] table keys on the Sepolia chain",
        );
        assert_eq!(
            cfg.chains
                .get(&Chain::sepolia())
                .expect("sepolia entry")
                .rpc_url,
            "wss://example.test/sepolia",
        );
    }

    #[test]
    fn invalid_chain_key_surfaces_a_toml_error() {
        // A key that is neither a numeric id nor a known chain name must
        // fail the parse (a `Toml` error variant), not silently drop.
        let err = toml::from_str::<EngineConfig>(
            r#"
[chains.bogus]
rpc_url = "wss://example.test/x"
"#,
        )
        .expect_err("bogus chain key must not parse");
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn http_limits_default_when_absent() {
        let http = ModuleLimits::default().http();
        assert_eq!(http.connect_timeout_max, Duration::from_secs(10));
        assert_eq!(http.first_byte_timeout_max, Duration::from_secs(30));
        assert_eq!(http.between_bytes_timeout_max, Duration::from_secs(30));
        assert_eq!(http.total_deadline, Duration::from_secs(60));
        assert_eq!(http.response_body_max_bytes, 16 * 1024 * 1024);
    }

    #[test]
    fn http_limits_parse_with_partial_overrides() {
        let cfg: EngineConfig = toml::from_str(
            r#"
[limits]
fuel_per_event = 7

[limits.http]
connect_timeout_max_ms  = 5_000
total_deadline_ms       = 90_000
response_body_max_bytes = 1_024
"#,
        )
        .expect("limits.http parses");
        assert_eq!(cfg.limits.fuel(), 7);
        let http = cfg.limits.http();
        assert_eq!(http.connect_timeout_max, Duration::from_millis(5_000));
        assert_eq!(http.total_deadline, Duration::from_millis(90_000));
        assert_eq!(http.response_body_max_bytes, 1_024);
        // Unset fields keep the built-in defaults.
        assert_eq!(http.first_byte_timeout_max, Duration::from_secs(30));
        assert_eq!(http.between_bytes_timeout_max, Duration::from_secs(30));
    }

    #[test]
    fn chain_limits_default_when_absent() {
        assert_eq!(
            ModuleLimits::default().chain_response_max_bytes(),
            1024 * 1024,
        );
    }

    #[test]
    fn chain_limits_parse_with_override() {
        let cfg: EngineConfig = toml::from_str(
            r#"
[limits.chain]
response_body_max_bytes = 2_048
"#,
        )
        .expect("limits.chain parses");
        assert_eq!(cfg.limits.chain_response_max_bytes(), 2_048);
    }

    #[test]
    fn chain_limits_saturate_degenerate_zero() {
        let cfg: EngineConfig = toml::from_str(
            r#"
[limits.chain]
response_body_max_bytes = 0
"#,
        )
        .expect("limits.chain parses");
        assert_eq!(
            cfg.limits.chain_response_max_bytes(),
            1,
            "zero saturates to 1 so resolution never rejects an empty body",
        );
    }

    #[test]
    fn http_limits_saturate_degenerate_millisecond_values() {
        // Zero would fail every request instantly; u64::MAX would
        // overflow timer arithmetic at request time. Both saturate.
        let limits = ModuleLimits {
            http: HttpLimitsSection {
                connect_timeout_max_ms: Some(0),
                total_deadline_ms: Some(u64::MAX),
                ..Default::default()
            },
            ..Default::default()
        };
        let http = limits.http();
        assert_eq!(http.connect_timeout_max, Duration::from_millis(1));
        assert_eq!(http.total_deadline, Duration::from_millis(86_400_000));
    }

    #[test]
    fn http_limits_saturate_zero_from_toml() {
        let cfg: EngineConfig = toml::from_str(
            r#"
[limits.http]
total_deadline_ms = 0
"#,
        )
        .expect("limits.http parses");
        assert_eq!(cfg.limits.http().total_deadline, Duration::from_millis(1));
    }

    #[test]
    fn log_limits_default_when_absent() {
        let logs = ModuleLimits::default().logs();
        assert_eq!(logs.bytes_per_run, 256 * 1024);
        assert_eq!(logs.runs_retained, 16);
    }

    #[test]
    fn log_limits_parse_with_overrides() {
        let cfg: EngineConfig = toml::from_str(
            r#"
[limits.logs]
bytes_per_run = 4_096
runs_retained = 3
"#,
        )
        .expect("limits.logs parses");
        let logs = cfg.limits.logs();
        assert_eq!(logs.bytes_per_run, 4_096);
        assert_eq!(logs.runs_retained, 3);
    }

    #[test]
    fn log_limits_saturate_zero_up_to_one() {
        // Zero would retain nothing; the saturating resolve keeps at
        // least the newest record and run.
        let cfg: EngineConfig = toml::from_str(
            r#"
[limits.logs]
bytes_per_run = 0
runs_retained = 0
"#,
        )
        .expect("limits.logs parses");
        let logs = cfg.limits.logs();
        assert_eq!(logs.bytes_per_run, 1);
        assert_eq!(logs.runs_retained, 1);
    }

    #[test]
    fn poison_limits_default_when_absent() {
        let poison = ModuleLimits::default().poison();
        assert_eq!(poison.max_failures, POISON_MAX_FAILURES);
        assert_eq!(poison.window, POISON_WINDOW);
    }

    #[test]
    fn poison_limits_parse_with_overrides() {
        let cfg: EngineConfig = toml::from_str(
            r#"
[limits.poison]
max_failures = 3
window_secs  = 60
"#,
        )
        .expect("limits.poison parses");
        let poison = cfg.limits.poison();
        assert_eq!(poison.max_failures, 3);
        assert_eq!(poison.window, Duration::from_secs(60));
    }

    #[test]
    fn poison_limits_saturate_zero_up_to_one() {
        // Zero max_failures would quarantine on the first trap; a zero
        // window would prune every failure before the check. Both
        // saturate to a usable minimum.
        let cfg: EngineConfig = toml::from_str(
            r#"
[limits.poison]
max_failures = 0
window_secs  = 0
"#,
        )
        .expect("limits.poison parses");
        let poison = cfg.limits.poison();
        assert_eq!(poison.max_failures, 1);
        assert_eq!(poison.window, Duration::from_secs(1));
    }

    #[test]
    fn adapters_parse_with_scoped_transport_grants() {
        let cfg: EngineConfig = toml::from_str(
            r#"
[[adapters]]
path = "providers/acme/acme_provider.wasm"
http_allow = ["api.acme.example", "*.acme.example"]
messaging_topics = ["/nexum/1/acme-orders/proto"]

[[adapters]]
path = "adapters/bare/bare.wasm"
manifest = "adapters/bare/module.toml"
"#,
        )
        .expect("adapters parse");
        assert_eq!(cfg.adapters.len(), 2);
        let first = &cfg.adapters[0];
        assert_eq!(
            first.path,
            PathBuf::from("providers/acme/acme_provider.wasm")
        );
        assert!(first.manifest.is_none(), "manifest defaults to sibling");
        assert_eq!(first.http_allow, vec!["api.acme.example", "*.acme.example"]);
        assert_eq!(first.messaging_topics, vec!["/nexum/1/acme-orders/proto"]);
        let second = &cfg.adapters[1];
        assert_eq!(
            second.manifest.as_deref(),
            Some(Path::new("adapters/bare/module.toml"))
        );
        assert!(
            second.http_allow.is_empty() && second.messaging_topics.is_empty(),
            "unset scope grants default empty",
        );
    }

    #[test]
    fn adapters_default_empty_when_absent() {
        let cfg = EngineConfig::default();
        assert!(cfg.adapters.is_empty());
    }

    #[test]
    fn dispatch_rate_default_when_absent() {
        let policy = ModuleLimits::default().dispatch_rate();
        assert_eq!(policy.capacity, DEFAULT_DISPATCH_BURST);
        assert_eq!(policy.refill_per_sec, DEFAULT_DISPATCH_REFILL_PER_SEC);
    }

    #[test]
    fn dispatch_rate_parse_with_overrides() {
        let cfg: EngineConfig = toml::from_str(
            r#"
[limits.dispatch]
burst          = 8
refill_per_sec = 4
"#,
        )
        .expect("limits.dispatch parses");
        let policy = cfg.limits.dispatch_rate();
        assert_eq!(policy.capacity, 8);
        assert_eq!(policy.refill_per_sec, 4);
    }

    #[test]
    fn dispatch_rate_saturates_zero_up_to_one() {
        // A zero burst or refill would wedge the bucket; saturate to a minimum.
        let cfg: EngineConfig = toml::from_str(
            r#"
[limits.dispatch]
burst          = 0
refill_per_sec = 0
"#,
        )
        .expect("limits.dispatch parses");
        let policy = cfg.limits.dispatch_rate();
        assert_eq!(policy.capacity, 1);
        assert_eq!(policy.refill_per_sec, 1);
    }

    #[test]
    fn watch_limits_default_when_absent() {
        let watch = ModuleLimits::default().watch();
        assert_eq!(watch.max_entries, DEFAULT_WATCH_MAX_ENTRIES);
        assert_eq!(watch.expiry, DEFAULT_WATCH_EXPIRY);
    }

    #[test]
    fn watch_limits_parse_with_overrides() {
        let cfg: EngineConfig = toml::from_str(
            r#"
[limits.watch]
max_entries = 32
expiry_secs = 900
"#,
        )
        .expect("limits.watch parses");
        let watch = cfg.limits.watch();
        assert_eq!(watch.max_entries, 32);
        assert_eq!(watch.expiry, Duration::from_secs(900));
    }

    #[test]
    fn watch_limits_saturate_zero_up_to_one() {
        // A zero cap would refuse every watch; a zero expiry would evict
        // each watch before its first poll. Both saturate.
        let cfg: EngineConfig = toml::from_str(
            r#"
[limits.watch]
max_entries = 0
expiry_secs = 0
"#,
        )
        .expect("limits.watch parses");
        let watch = cfg.limits.watch();
        assert_eq!(watch.max_entries, 1);
        assert_eq!(watch.expiry, Duration::from_secs(1));
    }

    #[test]
    fn extensions_tables_parse_opaquely() {
        let cfg: EngineConfig = toml::from_str(
            r#"
[extensions.example]
key = "value"
"#,
        )
        .expect("extensions table parses");
        let section = cfg.extensions.get("example").expect("example table");
        assert_eq!(section.get("key").and_then(|v| v.as_str()), Some("value"));
    }

    #[test]
    fn redact_replaces_long_path_segments() {
        let redacted =
            redact_url("https://lb.drpc.live/sepolia/AnOfyGnZ_0nWpS-OOwQzqAnFj_Naa0sR8ZxkVjewFaCJ");
        assert!(
            redacted.contains("KEY"),
            "long segment redacted: {redacted}"
        );
        assert!(
            !redacted.contains("AnOfyGnZ"),
            "the key must be gone: {redacted}",
        );
    }

    #[test]
    fn redact_keeps_short_segments_intact() {
        // Hostnames + "v2" path bits must not be redacted.
        let redacted = redact_url("https://eth-mainnet.g.alchemy.com/v2/abc");
        assert!(redacted.contains("eth-mainnet.g.alchemy.com"));
        assert!(redacted.contains("v2"));
    }

    #[test]
    fn redact_strips_userinfo_credentials() {
        // url renders userinfo as REDACTED:REDACTED@ when both parts are
        // present; assert the secret is gone rather than an exact string.
        let redacted = redact_url("https://user:pass@rpc.example.com/path");
        assert!(!redacted.contains("user:pass"), "userinfo gone: {redacted}");
        assert!(!redacted.contains("pass"), "password gone: {redacted}");
        assert!(
            redacted.contains("rpc.example.com"),
            "host kept: {redacted}"
        );
        assert!(redacted.contains("REDACTED"));
    }

    #[test]
    fn redact_strips_query_param_values() {
        let redacted = redact_url("https://rpc.example.com/v1?key=supersecret");
        assert!(
            !redacted.contains("supersecret"),
            "query secret gone: {redacted}"
        );
        assert!(redacted.contains("rpc.example.com"));
    }

    #[test]
    fn redact_strips_bare_query_flag() {
        // A bare `?token` flag (no `=`) is the whole query string; blanking
        // the query removes it. This is the gap string heuristics missed.
        let redacted = redact_url("https://rpc.example.com/v1?myapitoken");
        assert!(
            !redacted.contains("myapitoken"),
            "bare flag gone: {redacted}"
        );
        assert!(redacted.contains("rpc.example.com"));
    }

    #[test]
    fn redact_strips_fragment() {
        // OAuth-style bearer tokens can ride in the fragment.
        let redacted = redact_url("https://rpc.example.com/v1#bearertoken");
        assert!(
            !redacted.contains("bearertoken"),
            "fragment gone: {redacted}"
        );
        assert!(redacted.contains("rpc.example.com"));
    }

    #[test]
    fn redact_at_in_path_is_not_treated_as_userinfo() {
        // An `@` inside a path segment must not be parsed as userinfo; the
        // host stays intact.
        let redacted = redact_url("https://rpc.example.com/foo@bar/baz");
        assert!(
            redacted.contains("rpc.example.com"),
            "host kept: {redacted}"
        );
    }

    #[test]
    fn redact_leaves_clean_wss_url_intact() {
        // A url with no secret survives materially unchanged.
        let redacted = redact_url("wss://rpc.example.com/v1");
        assert!(redacted.contains("rpc.example.com"));
        assert!(redacted.contains("v1"));
        assert!(!redacted.contains("REDACTED"));
        assert!(!redacted.contains("KEY"));
    }

    #[test]
    fn redact_returns_placeholder_for_unparseable_url() {
        assert_eq!(redact_url("not a url"), "<unparseable-url>");
    }

    // ----------------- env var substitution -----------------------
    //
    // These tests stash + restore process env vars under unique names
    // so parallel `cargo test` runs don't trip on each other.

    fn with_env<F: FnOnce()>(name: &str, value: &str, body: F) {
        let prev = std::env::var(name).ok();
        // SAFETY: tests are single-threaded within one test fn; setting
        // an env var here is fine since the unique-name convention
        // avoids cross-test races.
        unsafe { std::env::set_var(name, value) };
        body();
        match prev {
            Some(v) => unsafe { std::env::set_var(name, v) },
            None => unsafe { std::env::remove_var(name) },
        }
    }

    #[test]
    fn substitute_replaces_known_variable() {
        with_env("NEXUM_TEST_RPC", "wss://example.test/abc", || {
            let raw = r#"rpc_url = "${NEXUM_TEST_RPC}""#;
            let out = substitute_env_vars(raw).unwrap();
            assert_eq!(out, r#"rpc_url = "wss://example.test/abc""#);
        });
    }

    #[test]
    fn substitute_errors_on_missing_variable() {
        // Variable name must not collide with anything in the operator
        // environment. Use a guaranteed-unique prefix.
        let err =
            substitute_env_vars(r#"x = "${NEXUM_TEST_DEFINITELY_UNSET_VAR_XYZ}""#).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("NEXUM_TEST_DEFINITELY_UNSET_VAR_XYZ"));
        assert!(msg.contains("not set"));
    }

    #[test]
    fn substitute_errors_on_invalid_name() {
        let err = substitute_env_vars(r#"x = "${lowercase_name}""#).unwrap_err();
        assert!(matches!(err, EnvVarError::InvalidName { .. }));
    }

    #[test]
    fn substitute_errors_on_unclosed_brace() {
        let err = substitute_env_vars(r#"x = "${UNCLOSED"#).unwrap_err();
        assert!(matches!(err, EnvVarError::Unclosed { .. }));
    }

    #[test]
    fn substitute_passes_text_with_no_placeholders_through() {
        let raw = "no placeholders here\nrpc_url = \"wss://x\"";
        assert_eq!(substitute_env_vars(raw).unwrap(), raw);
    }

    #[test]
    fn substitute_handles_multiple_placeholders_in_one_line() {
        with_env("NEXUM_TEST_A", "alpha", || {
            with_env("NEXUM_TEST_B", "beta", || {
                let raw = "k = \"${NEXUM_TEST_A}-${NEXUM_TEST_B}\"";
                let out = substitute_env_vars(raw).unwrap();
                assert_eq!(out, "k = \"alpha-beta\"");
            });
        });
    }

    #[test]
    fn substitute_preserves_utf8_around_placeholder() {
        // The hand-rolled byte loop must respect multi-byte UTF-8.
        with_env("NEXUM_TEST_U", "X", || {
            let raw = "# 河 ${NEXUM_TEST_U} ⚙️\n";
            let out = substitute_env_vars(raw).unwrap();
            assert_eq!(out, "# 河 X ⚙️\n");
        });
    }
}
