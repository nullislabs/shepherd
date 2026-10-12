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

/// The operator configs this repo ships.
const CONFIGS: [&str; 2] = ["engine.docker.toml", "engine.example.toml"];

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

/// The manifest each module of `engine.docker.toml` loads. The config names
/// the path the `Dockerfile` copies the manifest to; this is its source.
const DOCKER_MANIFESTS: [(&str, &str); 2] = [
    ("ccow-monitor", "modules/ccow-monitor/component.toml"),
    ("ethflow-watcher", "modules/ethflow-watcher/component.toml"),
];

/// The container config boots only if every chain a shipped manifest reads
/// has a `[chains.<id>]` table, and the venue serves one chain, so that
/// chain must be the one the modules read. Parsing alone does not catch a
/// config that names the wrong chain.
#[test]
fn the_container_config_serves_the_chain_the_shipped_modules_read() {
    let raw = std::fs::read_to_string(workspace_path("engine.docker.toml"))
        .expect("read engine.docker.toml");
    let config: toml::Value = toml::from_str(&substitute(&raw)).expect("engine.docker.toml parses");
    let declared: Vec<String> = config["chains"]
        .as_table()
        .expect("a [chains] table")
        .keys()
        .cloned()
        .collect();
    let venue = config["extensions"]["videre"]["venues"]["cow"]["chain"]
        .as_integer()
        .expect("the venue chain is a chain id");
    let ids: Vec<&str> = config["modules"]
        .as_array()
        .expect("[[modules]] entries")
        .iter()
        .map(|entry| entry["id"].as_str().expect("an id"))
        .collect();
    assert_eq!(
        ids,
        DOCKER_MANIFESTS.map(|(id, _)| id),
        "the module list moved: update DOCKER_MANIFESTS",
    );

    for (id, manifest) in DOCKER_MANIFESTS {
        let raw = std::fs::read_to_string(workspace_path(manifest))
            .unwrap_or_else(|e| panic!("read {manifest}: {e}"));
        let manifest: toml::Value =
            toml::from_str(&raw).unwrap_or_else(|e| panic!("{id}: manifest does not parse: {e}"));
        let chains: Vec<i64> = manifest["trigger"]
            .as_array()
            .expect("[[trigger]] entries")
            .iter()
            .filter_map(|trigger| trigger.get("chain_id").and_then(toml::Value::as_integer))
            .collect();
        assert!(!chains.is_empty(), "{id}: no trigger names a chain");
        for chain in chains {
            assert!(
                declared.contains(&chain.to_string()),
                "{id}: a trigger reads chain {chain}, which engine.docker.toml does not declare",
            );
            assert_eq!(
                chain, venue,
                "{id}: a trigger reads chain {chain}, and the venue serves chain {venue}",
            );
        }
    }
}
