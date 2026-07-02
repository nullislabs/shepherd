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
//! the cow-api / chain backends it surfaces as `HostError {
//! kind: unsupported }` so guests learn early.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use alloy_chains::Chain;
use serde::Deserialize;
use strum::IntoStaticStr;
use thiserror::Error;
use tracing::{info, warn};

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
    /// Modules the supervisor should boot. Each entry resolves a
    /// `(component.wasm, module.toml)` pair on the local filesystem
    /// for 0.2 - content-addressed resolution (Swarm / OCI /
    /// `[[content.sources]]`) lands in 0.3 per
    /// `docs/03-module-discovery.md`.
    #[serde(default)]
    pub modules: Vec<ModuleEntry>,
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
}

impl Default for EngineSection {
    fn default() -> Self {
        Self {
            state_dir: default_state_dir(),
            log_level: default_log_level(),
            metrics: MetricsSection::default(),
        }
    }
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
    /// Optional CoW orderbook base URL override for this chain. When
    /// absent (the common case), the host uses the canonical
    /// `api.cow.fi/{slug}/api/v1` URL from `cowprotocol::Chain`. Set
    /// this to point at a staging/barn instance or a local mock (e.g.
    /// `tools/orderbook-mock` for the load test).
    #[serde(default)]
    pub orderbook_url: Option<String>,
    /// Escape hatch: silence the boot-time warning when an `http(s)://`
    /// `rpc_url` is configured. Default `true` - every production
    /// module today subscribes to blocks or logs, so an HTTP URL is
    /// almost certainly an operator mistake (drpc / Alchemy / Infura
    /// expose BOTH `https://...` and `wss://...` per endpoint; the WS
    /// form is what `eth_subscribe` needs). Flip this to `false` only
    /// for a chain consumed exclusively by poll-style modules
    /// (request/response `chain::request`, no block / log subscriptions).
    #[serde(default = "default_require_ws")]
    pub require_ws: bool,
}

fn default_require_ws() -> bool {
    true
}

/// Default fuel budget per `on_event` invocation (~1 billion WASM
/// instructions).
const DEFAULT_FUEL_PER_EVENT: u64 = 1_000_000_000;

/// Default linear-memory cap per module store (64 MiB).
const DEFAULT_MEMORY_LIMIT: usize = 64 * 1024 * 1024;

/// Per-module wasmtime resource limits. Both fields are optional;
/// omitted values resolve to built-in defaults.
///
/// ```toml
/// [limits]
/// fuel_per_event = 1_000_000_000
/// memory_bytes   = 67_108_864
/// ```
#[derive(Debug, Default, Deserialize)]
pub struct ModuleLimits {
    /// Fuel budget granted per `on_event` invocation.
    pub fuel_per_event: Option<u64>,
    /// Linear-memory cap in bytes per module store.
    pub memory_bytes: Option<usize>,
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
             chain::request and cow_api::submit_order will return Unsupported)"
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
    // `validate_transports()` is intentionally NOT called here:
    // `load_or_default` runs before `tracing_subscriber::init()` in
    // `main.rs`, so any ERROR logs emitted here would be silently
    // dropped. The validator is invoked explicitly from `bootstrap::run_from_config`
    // after the subscriber is up.
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

impl EngineConfig {
    /// Surface configuration footguns at boot time, before the event
    /// loop opens any transport. Today's only check: an HTTP(S)
    /// `rpc_url` will refuse `eth_subscribe` (the protocol requires a
    /// WebSocket transport), and the engine's reconnect
    /// backoff will loop forever waiting for a subscription that can
    /// never open. We emit a single loud ERROR-level structured log
    /// per offending chain pointing the operator at the exact swap.
    ///
    /// `[chains.<id>] require_ws = false` opts a chain out of the
    /// check (poll-only deployments where no module subscribes).
    pub fn validate_transports(&self) {
        // `Chain` is not `Ord`, so sort by the numeric id for a stable
        // log order across boots.
        let mut chains: Vec<_> = self.chains.iter().collect();
        chains.sort_by_key(|(c, _)| c.id());
        for (chain, chain_cfg) in chains {
            if !chain_cfg.require_ws {
                continue;
            }
            let url = chain_cfg.rpc_url.trim().to_lowercase();
            if url.starts_with("ws://") || url.starts_with("wss://") {
                continue;
            }
            let chain_id = chain.id();
            // Redact BOTH the original URL and the suggested swap -
            // log files often end up in shared aggregators (Loki,
            // Datadog), and the swap is straightforward enough that
            // the operator doesn't need the full URL printed back.
            let suggested = redact_url(&suggest_ws_swap(&chain_cfg.rpc_url));
            tracing::error!(
                chain_id,
                rpc_url = %redact_url(&chain_cfg.rpc_url),
                suggested = %suggested,
                "rpc_url uses HTTP transport but the engine subscribes to \
                 blocks/logs via eth_subscribe (WS-only). Modules expecting \
                 these events will never receive them; the event-loop will \
                 log retry-with-backoff lines forever. Switch the URL to \
                 `wss://` (every paid provider exposes both forms) or set \
                 `[chains.{chain_id}] require_ws = false` if this chain is \
                 consumed by poll-only modules.",
            );
        }
    }
}

/// Best-effort swap of an `http(s)://` URL to the operator-likely WS
/// variant so the boot-time error message can suggest a concrete fix.
/// Falls back to the original URL if the scheme doesn't match.
fn suggest_ws_swap(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("https://") {
        return format!("wss://{rest}");
    }
    if let Some(rest) = url.strip_prefix("http://") {
        return format!("ws://{rest}");
    }
    url.to_owned()
}

/// Drop an embedded API key from a URL so the validation log line is
/// safe to share. Heuristic: replace any path segment longer than 20
/// characters with `<KEY>` (matches Alchemy / drpc / Infura key
/// shapes).
///
/// Public so other engine call sites that log the configured RPC URL
/// (provider pool boot, host-side debug traces) can apply the same
/// redaction; log aggregators (Loki, Datadog, Splunk) routinely
/// retain weeks of logs and the key should never sit in cold storage.
pub fn redact_url(url: &str) -> String {
    url.split('/')
        .map(|seg| {
            if seg.len() > 20 && !seg.contains('.') && !seg.contains(':') {
                "<KEY>".to_owned()
            } else {
                seg.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with_url(url: &str, require_ws: bool) -> EngineConfig {
        let mut chains = HashMap::new();
        chains.insert(
            Chain::from_id(11155111),
            ChainConfig {
                rpc_url: url.into(),
                orderbook_url: None,
                require_ws,
            },
        );
        EngineConfig {
            chains,
            ..Default::default()
        }
    }

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
    fn validate_accepts_wss_url() {
        let cfg = cfg_with_url("wss://lb.drpc.org/sepolia/<key>", true);
        cfg.validate_transports();
        // No assertion needed - passes if no panic and (in a real
        // logger setup) no ERROR line was emitted.
    }

    #[test]
    fn validate_accepts_ws_url() {
        let cfg = cfg_with_url("ws://localhost:8545", true);
        cfg.validate_transports();
    }

    #[test]
    fn validate_is_silent_when_require_ws_is_false() {
        // Operator explicitly opted out - HTTP is intentional (poll
        // only). The validator must not nag.
        let cfg = cfg_with_url("https://eth-mainnet.example.com/v2/abc", false);
        cfg.validate_transports();
    }

    #[test]
    fn validate_runs_without_panicking_on_http_url() {
        // The validator's contract is *log + continue*, not *abort*.
        // Catching a panic here would mask the only-WARN behaviour we
        // ship today.
        let cfg = cfg_with_url("https://eth-mainnet.example.com/v2/abc", true);
        cfg.validate_transports();
    }

    #[test]
    fn suggest_swaps_https_to_wss() {
        assert_eq!(
            suggest_ws_swap("https://lb.drpc.org/sepolia/abc"),
            "wss://lb.drpc.org/sepolia/abc",
        );
    }

    #[test]
    fn suggest_swaps_http_to_ws() {
        assert_eq!(
            suggest_ws_swap("http://localhost:8545"),
            "ws://localhost:8545",
        );
    }

    #[test]
    fn suggest_passes_through_already_ws_url() {
        assert_eq!(suggest_ws_swap("wss://x.example/k"), "wss://x.example/k",);
    }

    #[test]
    fn redact_replaces_long_path_segments() {
        let redacted =
            redact_url("https://lb.drpc.live/sepolia/AnOfyGnZ_0nWpS-OOwQzqAnFj_Naa0sR8ZxkVjewFaCJ");
        assert!(redacted.contains("<KEY>"));
        assert!(!redacted.contains("AnOfyGnZ"));
    }

    #[test]
    fn redact_keeps_short_segments_intact() {
        // Hostnames + "v1" path bits must not be redacted.
        let redacted = redact_url("https://eth-mainnet.g.alchemy.com/v2/abc");
        assert!(redacted.contains("eth-mainnet.g.alchemy.com"));
        assert!(redacted.contains("v2"));
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
