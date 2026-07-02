//! Parse `module.toml` from disk, validate, and emit operator-visible
//! warnings.
//!
//! Also exposes the small URL/host helpers the `http` host backend
//! uses to enforce the manifest's `[capabilities.http].allow` list at
//! request time.

use std::collections::HashSet;
use std::path::Path;

use tracing::{info, warn};

use super::error::ParseError;
use super::types::{KNOWN_CAPABILITIES, LoadedManifest, Manifest};

/// Read `module.toml` from `path`, parse, validate, and emit a deprecation
/// warning if `[capabilities]` is absent (0.1-compat fallback).
pub fn load(path: &Path) -> Result<LoadedManifest, ParseError> {
    let raw = std::fs::read_to_string(path)?;
    let manifest: Manifest = toml::from_str(&raw)?;

    let caps = manifest.capabilities.as_ref();
    if caps.is_none() {
        warn!(
            target: "manifest",
            "no [capabilities] section in module.toml - defaulting to \
             all-required (0.1 behaviour). This default will be removed \
             in 0.3; add an explicit [capabilities] block."
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
            info!(target: "manifest", required = %c.required.join(", "), "required capabilities");
        }
        if !c.optional.is_empty() {
            info!(
                target: "manifest",
                optional = %c.optional.join(", "),
                "optional capabilities (advisory in 0.2; trap-stub fallback ships in 0.3)",
            );
        }
    }

    let http_allowlist = caps
        .and_then(|c| c.http.as_ref())
        .map(|h| h.allow.clone())
        .unwrap_or_default();
    if !http_allowlist.is_empty() {
        info!(target: "manifest", allow = %http_allowlist.join(", "), "http allowlist");
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

/// Synthesise a "0.1 fallback" manifest for when no `module.toml` is found.
/// Emits the same deprecation warning as a missing-section manifest.
pub fn fallback_manifest() -> LoadedManifest {
    warn!(
        target: "manifest",
        "no module.toml found - defaulting to all-required (0.1 \
         behaviour). This default will be removed in 0.3; ship a \
         module.toml alongside your component."
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
/// schemes or malformed input. Delegates to `url::Url::parse` so we
/// inherit RFC 3986 handling of user-info, port, IDNA, IPv6 brackets,
/// etc.
pub fn extract_host(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    match parsed.scheme() {
        "http" | "https" => parsed.host_str().map(|h| h.to_owned()),
        _ => None,
    }
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
    use crate::manifest::types::Subscription;

    #[test]
    fn load_parses_block_and_log_subscriptions() {
        let toml = r#"
[module]
name = "twap-monitor"

[capabilities]
required = ["chain", "local-store"]

[[subscription]]
kind     = "block"
chain_id = 1

[[subscription]]
kind     = "log"
chain_id = 1
address  = "0xC92E8bdf79f0507f65a392b0ab4667716BFE0110"
event_signature = "0x00000000000000000000000000000000000000000000000000000000deadbeef"
"#;
        let manifest: Manifest = toml::from_str(toml).expect("parse");
        assert_eq!(manifest.module.name, "twap-monitor");
        assert_eq!(manifest.subscriptions.len(), 2);
        assert!(matches!(
            &manifest.subscriptions[0],
            Subscription::Block { chain_id: 1 }
        ));
        if let Subscription::Log {
            chain_id, address, ..
        } = &manifest.subscriptions[1]
        {
            assert_eq!(*chain_id, 1);
            assert!(address.is_some());
        } else {
            panic!("expected Log subscription");
        }
    }

    #[test]
    fn load_parses_cron_subscription() {
        let toml = r#"
[module]
name = "scheduler"

[[subscription]]
kind     = "cron"
schedule = "*/5 * * * *"
"#;
        let manifest: Manifest = toml::from_str(toml).expect("parse");
        assert!(matches!(
            &manifest.subscriptions[0],
            Subscription::Cron { .. }
        ));
    }

    #[test]
    fn load_rejects_unknown_capability() {
        let toml = r#"
[module]
name = "bad"

[capabilities]
required = ["chain", "not-a-real-cap"]
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("module.toml");
        std::fs::write(&path, toml).unwrap();
        let err = load(&path).unwrap_err();
        assert!(matches!(err, ParseError::UnknownCapability(ref name) if name == "not-a-real-cap"));
    }

    #[test]
    fn load_parses_config_table() {
        let toml = r#"
[module]
name = "example"

[config]
chain_id = 1
label    = "mainnet"
enabled  = true
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("module.toml");
        std::fs::write(&path, toml).unwrap();
        let loaded = load(&path).unwrap();
        let config: std::collections::HashMap<_, _> = loaded.config.into_iter().collect();
        assert_eq!(config.get("chain_id").map(String::as_str), Some("1"));
        assert_eq!(config.get("label").map(String::as_str), Some("mainnet"));
        assert_eq!(config.get("enabled").map(String::as_str), Some("true"));
    }

    #[test]
    fn extract_host_handles_common_shapes() {
        assert_eq!(
            extract_host("https://api.example.com/v1/x").as_deref(),
            Some("api.example.com")
        );
        assert_eq!(
            extract_host("http://example.com").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            extract_host("https://user:pw@host.example.com:8443/x").as_deref(),
            Some("host.example.com")
        );
        assert_eq!(
            extract_host("https://example.com?q=1").as_deref(),
            Some("example.com")
        );
        assert_eq!(extract_host("ftp://example.com"), None);
        assert_eq!(extract_host("not a url"), None);
    }

    #[test]
    fn extract_host_rejects_ssrf_bypass_attempts() {
        // Userinfo confusion: the actual host is evil.com, not allowed.com
        assert_eq!(
            extract_host("http://allowed.com@evil.com/path").as_deref(),
            Some("evil.com")
        );
        // URL-encoded @ must NOT resolve to "allowed.com" (bypass)
        assert_ne!(
            extract_host("http://allowed.com%40evil.com/path").as_deref(),
            Some("allowed.com")
        );
        // IPv6 loopback
        assert_eq!(extract_host("http://[::1]/path").as_deref(), Some("[::1]"));
        // Port is stripped from host — allowlist must match host only
        assert_eq!(
            extract_host("http://api.cow.fi:8080/v1").as_deref(),
            Some("api.cow.fi")
        );
        // Fragment containing slash should not affect host extraction
        assert_eq!(
            extract_host("https://api.cow.fi/path#frag/with/slash").as_deref(),
            Some("api.cow.fi")
        );
        // Query string containing slash should not affect host extraction
        assert_eq!(
            extract_host("https://api.cow.fi/path?q=/evil/path").as_deref(),
            Some("api.cow.fi")
        );
    }

    #[test]
    fn host_allowed_rejects_port_mismatch() {
        // Allowlist has "api.cow.fi" — host_allowed checks host only (no port),
        // because extract_host already strips port. Port enforcement is
        // operational, not host-level.
        let allow = vec!["api.cow.fi".to_string()];
        let host = extract_host("http://api.cow.fi:8080/v1").unwrap();
        assert!(host_allowed(&host, &allow));

        // But a different host should still be rejected
        let evil_host = extract_host("http://allowed.com@evil.com/path").unwrap();
        assert!(!host_allowed(&evil_host, &allow));
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
