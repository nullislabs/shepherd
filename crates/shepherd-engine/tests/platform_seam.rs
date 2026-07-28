//! Cow-on-the-seam E2E coverage, relocated from videre-host: these tests
//! reach into shepherd's own L3 assets - the `cow-venue` adapter wasm, the
//! `ethflow-watcher` and `twap-monitor` modules, and the cow adapter's
//! Sepolia manifest - and exercise them over the generic runtime seam
//! (`platform()` / `VenueRegistry`) that shepherd already depends on. They
//! live here, in the repo that OWNS those assets, rather than in videre-host
//! (L2, the generic venue host) which must not know about the cow venue.
//!
//! Skips gracefully when a wasm artefact is absent; build the fixtures with
//! `cargo build --release --target wasm32-wasip2 -p cow-venue
//! --features cow-venue/adapter -p ethflow-watcher -p twap-monitor`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use nexum_runtime::bindings::nexum;
use nexum_runtime::engine_config::{AdapterEntry, EngineConfig, ModuleEntry, ModuleLimits};
use nexum_runtime::host::extension::{Extension, ExtensionEvent};
use nexum_runtime::host::state::HostState;
use nexum_runtime::manifest::CapabilityRegistry;
use nexum_runtime::supervisor::{Supervisor, build_linker};
use nexum_runtime::test_utils::{MockTypes, mock_components};
use videre_host::{Videre, platform};
use wasmtime::component::Linker;

/// The subscription kind the platform's status poller emits.
const INTENT_STATUS: &str = "intent-status";

// ── fixtures + assembly ───────────────────────────────────────────────

/// Path under the workspace root (the topmost ancestor with a `Cargo.toml`).
/// Shepherd IS the L3 root, so assets sit directly beneath it with no group
/// prefix (unlike the `shepherd/`-prefixed paths these tests carried inside
/// the videre workspace).
fn workspace_path(relative: &str) -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .filter(|d| d.join("Cargo.toml").is_file())
        .last()
        .unwrap_or(manifest)
        .join(relative)
}

/// Path to a module's `.wasm` artefact under the workspace target dir,
/// or `None` with a skip message when it is not built.
fn module_wasm_or_skip(module_name: &str) -> Option<PathBuf> {
    let artifact = module_name.replace('-', "_");
    let p = workspace_path(&format!("target/wasm32-wasip2/release/{artifact}.wasm"));
    if p.exists() {
        Some(p)
    } else {
        eprintln!(
            "SKIP: {} not found - build with `cargo build -p {module_name} --target wasm32-wasip2 --release`",
            p.display()
        );
        None
    }
}

fn make_wasmtime_engine() -> wasmtime::Engine {
    let mut config = wasmtime::Config::new();
    config.wasm_component_model(true);
    config.consume_fuel(true);
    wasmtime::Engine::new(&config).expect("wasmtime engine")
}

/// The platform's extension slice, keeping the concrete handle for
/// event-source calls.
fn videre_assembly(videre: &Arc<Videre>) -> Vec<Arc<dyn Extension<MockTypes>>> {
    vec![Arc::clone(videre) as Arc<dyn Extension<MockTypes>>]
}

fn make_linker(
    engine: &wasmtime::Engine,
    extensions: &[Arc<dyn Extension<MockTypes>>],
) -> Linker<HostState<MockTypes>> {
    build_linker::<MockTypes>(engine, extensions).expect("build_linker")
}

/// A test block that drives dispatch and the dispatch-time sweeps.
fn block(chain_id: u64) -> nexum::host::types::Block {
    nexum::host::types::Block {
        chain_id,
        number: 19_000_000,
        hash: vec![0xab; 32],
        timestamp: 1_700_000_000_000,
    }
}

/// Wrap a polled transition as the extension event the platform emits.
fn status_event(update: videre_host::IntentStatusUpdate) -> ExtensionEvent {
    let attrs = vec![("venue", update.venue.clone())];
    let payload = update.encode().expect("encode intent-status envelope");
    ExtensionEvent {
        kind: INTENT_STATUS,
        attrs,
        event: nexum::host::types::Event::Custom(nexum::host::types::CustomEvent {
            kind: INTENT_STATUS.to_owned(),
            payload,
        }),
    }
}

/// Boot a single module against the given videre platform.
async fn boot_example(videre: &Arc<Videre>, wasm: &Path, manifest: &Path) -> Supervisor<MockTypes> {
    let engine = make_wasmtime_engine();
    let extensions = videre_assembly(videre);
    let linker = make_linker(&engine, &extensions);
    let components = mock_components();
    let limits = ModuleLimits::default();
    Supervisor::boot_single(
        &engine,
        &linker,
        wasm,
        Some(manifest),
        &components,
        &limits,
        &extensions,
        None,
    )
    .await
    .expect("boot_single")
}

// ── world contract ────────────────────────────────────────────────────

/// The shipped cow adapter's only capability is outbound HTTP, so it cannot
/// reach chain, messaging, key material, or persistence.
#[test]
fn e2e_cow_venue_component_imports_equal_declared_capabilities() {
    let wasm = workspace_path("target/wasm32-wasip2/release/cow_venue.wasm");
    if !wasm.exists() {
        eprintln!(
            "SKIP: {} not found - build with `just build-cow-venue`",
            wasm.display()
        );
        return;
    }
    let engine = make_wasmtime_engine();
    let component = wasmtime::component::Component::from_file(&engine, &wasm).expect("compile");
    let imports: Vec<String> = component
        .component_type()
        .imports(&engine)
        .map(|(name, _)| name.to_owned())
        .collect();

    let registry = CapabilityRegistry::core();
    let caps: std::collections::BTreeSet<&str> = imports
        .iter()
        .filter_map(|name| registry.wit_import_to_cap(name))
        .collect();
    assert_eq!(
        caps,
        std::collections::BTreeSet::from(["http"]),
        "imports were: {imports:?}"
    );
    assert!(
        imports.iter().all(|name| !name.contains("nexum:host/chain")
            && !name.contains("messaging")
            && !name.contains("local-store")
            && !name.contains("identity")
            && !name.contains("logging")),
        "imports were: {imports:?}"
    );
}

// ── intent-status subscription E2E ────────────────────────────────────

/// ethflow-watcher (built by `#[videre_sdk::keeper]`) boots with its shipped
/// manifest and handles a delivered cow status transition without trapping.
#[tokio::test]
async fn e2e_ethflow_watcher_boots_and_handles_intent_status() {
    let Some(wasm) = module_wasm_or_skip("ethflow-watcher") else {
        return;
    };
    let manifest = workspace_path("modules/ethflow-watcher/module.toml");
    let videre = Arc::new(platform(&EngineConfig::default()));
    let mut supervisor = boot_example(&videre, &wasm, &manifest).await;
    assert_eq!(supervisor.alive_count(), 1);
    assert!(
        supervisor
            .extension_subscription_kinds()
            .contains(INTENT_STATUS)
    );

    let update = videre_host::IntentStatusUpdate {
        venue: "cow".to_owned(),
        receipt: vec![0xAB; 56],
        status: videre_status_body::StatusBody {
            status: videre_status_body::IntentStatus::Open,
            proof: None,
            reason: None,
        }
        .encode()
        .expect("encode"),
    };
    assert_eq!(
        supervisor
            .dispatch_extension_event(status_event(update))
            .await,
        1
    );
    assert_eq!(supervisor.alive_count(), 1);
}

/// The shepherd bundle pair: twap-monitor (a `#[videre_sdk::keeper]` worker)
/// boots against the cow adapter (the body-version handshake admits the
/// pair) and a Sepolia block dispatch reaches it and keeps it alive.
#[tokio::test]
async fn e2e_twap_monitor_boots_against_the_cow_adapter() {
    let (Some(adapter_wasm), Some(module_wasm)) = (
        module_wasm_or_skip("cow-venue"),
        module_wasm_or_skip("twap-monitor"),
    ) else {
        return;
    };

    let components = mock_components();
    let engine = make_wasmtime_engine();
    let config = EngineConfig {
        adapters: vec![AdapterEntry {
            path: adapter_wasm,
            // Sepolia variant: twap-monitor pins chain 11155111, so the
            // adapter manifest must name the same chain for the pair to
            // submit to the right orderbook.
            manifest: Some(workspace_path("crates/cow-venue/module.sepolia.toml")),
            http_allow: Vec::new(),
            messaging_topics: Vec::new(),
        }],
        modules: vec![ModuleEntry {
            path: module_wasm,
            manifest: Some(workspace_path("modules/twap-monitor/module.toml")),
        }],
        ..Default::default()
    };
    let videre = Arc::new(platform(&config));
    let extensions = videre_assembly(&videre);
    let linker = make_linker(&engine, &extensions);

    let mut supervisor =
        Supervisor::boot(&engine, &linker, &config, &components, &extensions, None)
            .await
            .expect("boot");
    assert_eq!(supervisor.adapter_alive_count(), 1, "cow is routable");
    assert_eq!(supervisor.alive_count(), 1, "twap-monitor is alive");

    // twap-monitor subscribes to Sepolia blocks (poll path); with no
    // watches indexed the run is empty and the keeper stays alive.
    assert_eq!(supervisor.dispatch_block(block(11_155_111)).await, 1);
    assert_eq!(supervisor.alive_count(), 1);
}
