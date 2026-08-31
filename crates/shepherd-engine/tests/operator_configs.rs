//! The shipped `engine.*.toml` files, held to the runtime's own parser and
//! to this composition root's venue section.
//!
//! Every key in these files must exist in the pinned config crate, which
//! parses with `deny_unknown_fields`, so a retired key (`[[adapters]]`,
//! `[limits] fuel_per_event`, `[limits.watch]`) refuses here rather than at
//! an operator's boot.

use std::path::{Path, PathBuf};

use nexum_runtime::config::EngineConfig;
use nexum_runtime::toml;

/// The eight operator configs this repo ships.
const CONFIGS: [&str; 8] = [
    "engine.docker.toml",
    "engine.e2e.toml",
    "engine.example.toml",
    "engine.load.toml",
    "engine.m2.toml",
    "engine.m3.toml",
    "engine.soak.docker.toml",
    "engine.soak.toml",
];

/// Path under the workspace root. This crate sits at `crates/<pkg>`, so
/// the root is exactly two levels up. An ancestor walk would answer the
/// enclosing checkout instead when the worktree is nested inside one.
fn workspace_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<pkg> sits two levels under the workspace root")
        .join(relative)
}

/// Replace every `${VAR}` token with a parseable endpoint. The loader does
/// this from the environment; the test only needs the shape to survive.
fn substitute(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('}') else {
            break;
        };
        out.push_str("wss://rpc.invalid/");
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    out
}

fn load(name: &str) -> EngineConfig {
    let path = workspace_path(name);
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    toml::from_str::<EngineConfig>(&substitute(&raw))
        .unwrap_or_else(|e| panic!("{name} does not parse: {e}"))
}

/// Every shipped config parses against the pinned schema.
#[test]
fn the_shipped_configs_parse() {
    for name in CONFIGS {
        let _ = load(name);
    }
}

/// Each `[[modules]]` entry carries the operator-written `id` the
/// `[policy.component.<id>]` join needs, and a config that pins no digest
/// says so on `[engine]`.
#[test]
fn every_module_entry_is_identified_and_the_digest_stance_is_explicit() {
    for name in CONFIGS {
        let config = load(name);
        for entry in &config.modules {
            assert!(!entry.id.trim().is_empty(), "{name}: an entry has no id");
            assert!(
                entry.digest.is_some() || !config.engine.require_component_digest,
                "{name}: {} pins no digest while [engine] demands one",
                entry.id,
            );
        }
    }
}

/// The venue section of every shipped config registers on a live registry,
/// which pins the `[extensions.videre.venues.<id>]` schema against the
/// venue crate rather than against a hand-written fixture.
#[test]
fn the_venue_sections_register() {
    for name in CONFIGS {
        let config = load(name);
        let videre = videre_host::platform();
        shepherd_engine::venues::register(videre.registry(), &config)
            .unwrap_or_else(|e| panic!("{name}: venue registration refused: {e:#}"));
    }
}
