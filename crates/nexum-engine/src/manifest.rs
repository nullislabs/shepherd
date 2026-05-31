//! Minimal `nexum.toml` parser and capability-enforcement helpers (0.2 scope).
//!
//! 0.2 intentionally ships a slim subset of the manifest spec described in
//! the migration guide §3:
//!
//! - `[capabilities].required` is parsed and validated (names must be in
//!   the known capability set; the 0.2 reference engine always provides
//!   all of them, so this is a sanity check + future-proofing).
//! - `[capabilities].optional` is parsed and logged; trap-stub fallback
//!   for absent optionals is deferred to 0.3.
//! - `[capabilities.http].allow` is parsed and consulted by the `http`
//!   host impl before any outbound call.
//! - `[config]` is flattened to `Vec<(String, String)>` and passed to the
//!   module's `init`. Typed `config-value` variant is deferred to 0.3.
//!
//! When the manifest file is missing or has no `[capabilities]` section,
//! a deprecation warning is emitted on stderr and the engine falls back
//! to 0.1 behaviour (treat every linked capability as required). This
//! fallback will be removed in 0.3.

use std::collections::HashSet;
use std::path::Path;

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

/// Errors returned while loading or validating a manifest.
#[derive(Debug)]
pub enum ParseError {
    Io(std::io::Error),
    Toml(toml::de::Error),
    UnknownCapability(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "manifest: i/o: {e}"),
            Self::Toml(e) => write!(f, "manifest: parse: {e}"),
            Self::UnknownCapability(name) => write!(
                f,
                "manifest: unknown capability {:?} in [capabilities].required (known: {})",
                name,
                KNOWN_CAPABILITIES.join(", ")
            ),
        }
    }
}

impl std::error::Error for ParseError {}

/// Loaded + validated manifest, plus its source path for diagnostics.
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

/// Read `nexum.toml` from `path`, parse, validate, and emit a deprecation
/// warning if `[capabilities]` is absent (0.1-compat fallback).
pub fn load(path: &Path) -> Result<LoadedManifest, ParseError> {
    let raw = std::fs::read_to_string(path).map_err(ParseError::Io)?;
    let manifest: Manifest = toml::from_str(&raw).map_err(ParseError::Toml)?;

    let caps = manifest.capabilities.as_ref();
    if caps.is_none() {
        eprintln!(
            "[deprecation] no [capabilities] section in nexum.toml — \
             defaulting to all-required (0.1 behaviour). This default \
             will be removed in 0.3; add an explicit [capabilities] block \
             now."
        );
    }

    if let Some(c) = caps {
        let known: HashSet<&str> = KNOWN_CAPABILITIES.iter().copied().collect();
        for name in c.required.iter().chain(c.optional.iter()) {
            if !known.contains(name.as_str()) {
                return Err(ParseError::UnknownCapability(name.clone()));
            }
        }
        if !c.required.is_empty() {
            eprintln!(
                "[manifest] required capabilities: {}",
                c.required.join(", ")
            );
        }
        if !c.optional.is_empty() {
            eprintln!(
                "[manifest] optional capabilities (advisory in 0.2; trap-stub fallback \
                 ships in 0.3): {}",
                c.optional.join(", ")
            );
        }
    }

    let http_allowlist = caps
        .and_then(|c| c.http.as_ref())
        .map(|h| h.allow.clone())
        .unwrap_or_default();
    if !http_allowlist.is_empty() {
        eprintln!("[manifest] http allowlist: {}", http_allowlist.join(", "));
    }

    let config = manifest
        .config
        .iter()
        .map(|(k, v)| (k.clone(), stringify_toml_value(v)))
        .collect();

    Ok(LoadedManifest {
        manifest,
        http_allowlist,
        config,
    })
}

/// Synthesise a "0.1 fallback" manifest for when no `nexum.toml` is found.
/// Emits the same deprecation warning as a missing-section manifest.
pub fn fallback_manifest() -> LoadedManifest {
    eprintln!(
        "[deprecation] no nexum.toml found — defaulting to all-required \
         (0.1 behaviour). This default will be removed in 0.3; ship a \
         nexum.toml alongside your component."
    );
    LoadedManifest {
        manifest: Manifest::default(),
        http_allowlist: Vec::new(),
        config: Vec::new(),
    }
}

/// Check whether `host` matches any pattern in the allowlist. Patterns are
/// either exact (`api.example.com`) or `*.suffix` wildcards which match
/// any subdomain of `suffix` (but not `suffix` itself).
pub fn host_allowed(host: &str, allowlist: &[String]) -> bool {
    let host = host.to_ascii_lowercase();
    allowlist.iter().any(|pat| {
        let pat = pat.to_ascii_lowercase();
        if let Some(suffix) = pat.strip_prefix("*.") {
            host.ends_with(&format!(".{suffix}"))
        } else {
            host == pat
        }
    })
}

/// Extract the host component from a URL. Returns `None` for non-http(s)
/// schemes or malformed input. Intentionally simple — adds no `url`
/// crate dependency.
pub fn extract_host(url: &str) -> Option<&str> {
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host_end = after_scheme
        .find('/')
        .or_else(|| after_scheme.find('?'))
        .unwrap_or(after_scheme.len());
    let host = &after_scheme[..host_end];
    // strip optional user-info and port.
    let host = host.rsplit('@').next().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);
    if host.is_empty() { None } else { Some(host) }
}

fn stringify_toml_value(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Datetime(d) => d.to_string(),
        toml::Value::Array(_) | toml::Value::Table(_) => v.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_host_handles_common_shapes() {
        assert_eq!(
            extract_host("https://api.example.com/v1/x"),
            Some("api.example.com")
        );
        assert_eq!(extract_host("http://example.com"), Some("example.com"));
        assert_eq!(
            extract_host("https://user:pw@host.example.com:8443/x"),
            Some("host.example.com")
        );
        assert_eq!(extract_host("https://example.com?q=1"), Some("example.com"));
        assert_eq!(extract_host("ftp://example.com"), None);
        assert_eq!(extract_host("not a url"), None);
    }

    #[test]
    fn host_allowed_exact_and_wildcard() {
        let allow = vec!["api.cow.fi".to_string(), "*.discord.com".to_string()];
        assert!(host_allowed("api.cow.fi", &allow));
        assert!(!host_allowed("evil.api.cow.fi", &allow));
        assert!(host_allowed("foo.discord.com", &allow));
        assert!(host_allowed("a.b.discord.com", &allow));
        assert!(!host_allowed("discord.com", &allow));
        assert!(!host_allowed("nope.example", &allow));
    }
}
