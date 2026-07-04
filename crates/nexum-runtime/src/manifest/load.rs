//! Parse `module.toml` from disk, validate, and emit operator-visible
//! warnings.
//!
//! Also exposes the host-matching helper the wasi:http gate uses to
//! enforce the manifest's `[capabilities.http].allow` list at request
//! time.

use std::path::Path;

use tracing::{info, warn};

use super::capabilities::CapabilityRegistry;
use super::error::ParseError;
use super::types::{LoadedManifest, Manifest};

/// Read `module.toml` from `path`, parse, validate, and emit a deprecation
/// warning if `[capabilities]` is absent (0.1-compat fallback). Declared
/// capability names are validated against `registry`, so extension
/// capabilities are recognised only once their namespace is registered.
pub fn load(path: &Path, registry: &CapabilityRegistry) -> Result<LoadedManifest, ParseError> {
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
        for name in c.required.iter().chain(c.optional.iter()) {
            if !registry.is_known(name) {
                return Err(ParseError::UnknownCapability {
                    name: name.clone(),
                    known: registry.known_names(),
                });
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
/// any subdomain of `suffix` (but not `suffix` itself). Matching is
/// case-insensitive and host-only: no scheme, no port, and IPv6 literals
/// keep their brackets.
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
        let err = load(&path, &CapabilityRegistry::core()).unwrap_err();
        assert!(
            matches!(err, ParseError::UnknownCapability { ref name, .. } if name == "not-a-real-cap")
        );
    }

    #[test]
    fn load_rejects_the_retired_clock_capability() {
        // `clock` is no longer a host capability (WASI clocks are ambient);
        // a manifest declaring it fails like any other unknown name.
        let toml = r#"
[module]
name = "stale"

[capabilities]
required = ["clock"]
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("module.toml");
        std::fs::write(&path, toml).unwrap();
        let err = load(&path, &CapabilityRegistry::core()).unwrap_err();
        assert!(matches!(err, ParseError::UnknownCapability { ref name, .. } if name == "clock"));
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
        let loaded = load(&path, &CapabilityRegistry::core()).unwrap();
        let config: std::collections::HashMap<_, _> = loaded.config.into_iter().collect();
        assert_eq!(config.get("chain_id").map(String::as_str), Some("1"));
        assert_eq!(config.get("label").map(String::as_str), Some("mainnet"));
        assert_eq!(config.get("enabled").map(String::as_str), Some("true"));
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

    #[test]
    fn host_allowed_is_case_insensitive_both_ways() {
        let upper = vec!["API.COW.FI".to_string()];
        let lower = vec!["api.cow.fi".to_string()];
        assert!(host_allowed("api.cow.fi", &upper));
        assert!(host_allowed("Api.Cow.Fi", &lower));
    }

    #[test]
    fn host_allowed_matches_hosts_not_authorities() {
        // Entries are bare hosts; a port or userinfo in a pattern can
        // never match a host string.
        let allow = vec!["api.cow.fi:8443".to_string(), "u@api.cow.fi".to_string()];
        assert!(!host_allowed("api.cow.fi", &allow));
    }
}
