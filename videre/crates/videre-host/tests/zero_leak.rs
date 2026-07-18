//! Zero-leak oracle: the host boots the echo venue and routes a worker's
//! submission purely through the generic extension seam, while the
//! `nexum-runtime` crate graph reaches no venue-shaped crate.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use nexum_runtime::bindings::nexum;
use nexum_runtime::engine_config::{AdapterEntry, EngineConfig, ModuleEntry};
use nexum_runtime::host::component::ChainMethod;
use nexum_runtime::host::extension::Extension;
use nexum_runtime::supervisor::{Supervisor, build_linker};
use nexum_runtime::test_utils::{
    MockChainProvider, MockStateStore, MockTypes, mock_components_from,
};
use videre_host::{VenueRegistry, platform};

/// Workspace-root-relative path. `CARGO_MANIFEST_DIR` is
/// `videre/crates/videre-host`; three parents up is the workspace root.
fn workspace_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates dir")
        .parent()
        .expect("videre root")
        .parent()
        .expect("workspace root")
        .join(relative)
}

/// Path to a module's `.wasm` artefact under the workspace target dir.
/// A missing artefact is a hard failure under CI (the gate may not skip
/// itself) and a soft skip locally.
fn module_wasm_or_skip(module_name: &str) -> Option<PathBuf> {
    let artifact = module_name.replace('-', "_");
    let p = workspace_path(&format!("target/wasm32-wasip2/release/{artifact}.wasm"));
    if p.exists() {
        return Some(p);
    }
    assert!(
        std::env::var_os("CI").is_none(),
        "{} must be prebuilt in CI",
        p.display()
    );
    eprintln!(
        "SKIP: {} not found - build with `cargo build -p {module_name} --target wasm32-wasip2 --release`",
        p.display()
    );
    None
}

/// The boot oracle: the venue adapter installs and a worker's submission
/// reaches it, with the platform supplied only as a generic extension.
#[tokio::test]
async fn e2e_echo_venue_boots_and_submits_through_the_generic_seam() {
    let (Some(adapter_wasm), Some(module_wasm)) = (
        module_wasm_or_skip("echo-venue"),
        module_wasm_or_skip("echo-client"),
    ) else {
        return;
    };

    // The adapter reads eth_blockNumber on submit to justify its `chain`
    // grant; program the mock so that read succeeds.
    let chain = MockChainProvider::new();
    chain.on_method(ChainMethod::EthBlockNumber, "\"0x1\"");
    let components = mock_components_from(chain, MockStateStore::new());

    let mut engine_config = wasmtime::Config::new();
    engine_config.wasm_component_model(true);
    engine_config.consume_fuel(true);
    let engine = wasmtime::Engine::new(&engine_config).expect("wasmtime engine");

    let config = EngineConfig {
        adapters: vec![AdapterEntry {
            path: adapter_wasm,
            manifest: Some(workspace_path(
                "videre/modules/examples/echo-venue/module.toml",
            )),
            http_allow: Vec::new(),
            messaging_topics: Vec::new(),
        }],
        modules: vec![ModuleEntry {
            path: module_wasm,
            manifest: Some(workspace_path(
                "videre/modules/examples/echo-client/module.toml",
            )),
        }],
        ..Default::default()
    };
    let extensions: Vec<Arc<dyn Extension<MockTypes>>> = vec![Arc::new(platform(&config))];
    let linker = build_linker::<MockTypes>(&engine, &extensions).expect("build_linker");

    let mut supervisor =
        Supervisor::boot(&engine, &linker, &config, &components, &extensions, None)
            .await
            .expect("boot");
    assert_eq!(supervisor.adapter_alive_count(), 1, "echo-venue installed");
    assert_eq!(supervisor.alive_count(), 1, "echo-client alive");

    // One block drives the worker's on_block submission; the registry the
    // extension published on the service map observes the accepted receipt.
    let block = nexum::host::types::Block {
        chain_id: 1,
        number: 19_000_000,
        hash: vec![0xab; 32],
        timestamp: 1_700_000_000_000,
    };
    assert_eq!(supervisor.dispatch_block(block).await, 1);
    let registry = supervisor
        .services()
        .get::<VenueRegistry>(VenueRegistry::NAMESPACE)
        .expect("registry service");
    let updates = registry.poll_status_transitions().await;
    assert!(
        updates.iter().any(|u| u.venue == "echo-venue"),
        "the submission reached the venue; updates were: {updates:?}"
    );
}

/// The graph oracle: `cargo tree` for the host crate (normal + build
/// edges) names no videre, intent, venue, or cow crate.
#[test]
fn host_crate_graph_reaches_no_venue_shaped_crate() {
    let output = Command::new(env!("CARGO"))
        .args([
            "tree",
            "-p",
            "nexum-runtime",
            "-e",
            "normal,build",
            "--all-features",
            "--prefix",
            "none",
            "--locked",
        ])
        .current_dir(workspace_path(""))
        .output()
        .expect("cargo tree runs");
    assert!(
        output.status.success(),
        "cargo tree failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tree = String::from_utf8_lossy(&output.stdout);
    let reached: Vec<&str> = tree
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| {
            let name = name.to_lowercase();
            ["videre", "intent", "venue", "cow"]
                .iter()
                .any(|word| name.contains(word))
        })
        .collect();
    assert!(
        reached.is_empty(),
        "venue-shaped crates reached: {reached:?}"
    );
}
