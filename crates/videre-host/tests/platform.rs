//! E2E coverage for the videre platform over the generic runtime seam:
//! the venue-adapter provider boot, the client -> registry -> adapter
//! round trip, the status-poll event source, and the trap-to-recovery
//! sweeps. Exercises pre-built wasm artefacts and skips gracefully when
//! an artefact is absent.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use nexum_runtime::bindings::nexum;
use nexum_runtime::engine_config::{
    AdapterEntry, EngineConfig, ModuleEntry, ModuleLimits, PoisonLimitsSection,
};
use nexum_runtime::host::component::ChainMethod;
use nexum_runtime::host::extension::{EventSources, Extension, ExtensionEvent};
use nexum_runtime::host::state::HostState;
use nexum_runtime::manifest::CapabilityRegistry;
use nexum_runtime::supervisor::{Supervisor, build_linker, build_provider_linker};
use nexum_runtime::test_utils::{MockChainProvider, MockStateStore, MockTypes, mock_components};
use videre_host::bindings::{
    IntentHeader, IntentStatus, Quotation, Settlement, SubmitOutcome, VenueError, value_flow,
};
use videre_host::{
    VenueAdapterKind, VenueId, VenueInvoker, VenueRegistry, VenueRegistryBuilder, Videre, platform,
};
use wasmtime::component::Linker;

/// The subscription kind the platform's status poller emits.
const INTENT_STATUS: &str = "intent-status";

// ── fixtures + assembly ───────────────────────────────────────────────

/// Workspace-root-relative path. `CARGO_MANIFEST_DIR` is
/// `crates/videre-host`; two parents up is the workspace root.
fn workspace_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates dir")
        .parent()
        .expect("workspace root")
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

/// The platform under test plus the extension slice the boot paths take.
/// The concrete handle stays available for the event-source calls.
fn videre_assembly(videre: &Arc<Videre>) -> Vec<Arc<dyn Extension<MockTypes>>> {
    vec![Arc::clone(videre) as Arc<dyn Extension<MockTypes>>]
}

fn make_linker(
    engine: &wasmtime::Engine,
    extensions: &[Arc<dyn Extension<MockTypes>>],
) -> Linker<HostState<MockTypes>> {
    build_linker::<MockTypes>(engine, extensions).expect("build_linker")
}

/// The registry the booted supervisor publishes under the videre
/// namespace.
fn registry_of(supervisor: &Supervisor<MockTypes>) -> Arc<VenueRegistry> {
    supervisor
        .services()
        .get::<VenueRegistry>(VenueRegistry::NAMESPACE)
        .expect("registry service")
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
    ExtensionEvent {
        kind: INTENT_STATUS,
        attrs: vec![("venue", update.venue.clone())],
        event: nexum::host::types::Event::IntentStatus(update),
    }
}

// ── world contract ────────────────────────────────────────────────────

/// The per-component venue-adapter world contract: an adapter built
/// through `#[videre_sdk::venue]` imports exactly the scoped
/// transport its manifest declares (`chain`), by construction of the
/// emitted world. The venue side never depended on toolchain elision;
/// this pins that it does not regress to it.
#[test]
fn e2e_echo_venue_component_imports_equal_declared_capabilities() {
    let Some(wasm) = module_wasm_or_skip("echo-venue") else {
        return;
    };
    let engine = make_wasmtime_engine();
    let component = wasmtime::component::Component::from_file(&engine, &wasm).expect("compile");
    let imports: Vec<String> = component
        .component_type()
        .imports(&engine)
        .map(|(name, _)| name.to_owned())
        .collect();

    // Capability-bearing imports resolve to exactly the declared set.
    let registry = CapabilityRegistry::core();
    let caps: std::collections::BTreeSet<&str> = imports
        .iter()
        .filter_map(|name| registry.wit_import_to_cap(name))
        .collect();
    assert_eq!(
        caps,
        std::collections::BTreeSet::from(["chain"]),
        "imports were: {imports:?}"
    );

    // No host key-material or persistence interface leaks in: an adapter
    // structurally cannot reach messaging it never declared, local-store,
    // identity, or logging.
    assert!(
        imports.iter().all(|name| !name.contains("messaging")
            && !name.contains("local-store")
            && !name.contains("identity")
            && !name.contains("logging")),
        "imports were: {imports:?}"
    );
}

/// The venue-adapter provider linker binds only the scoped transport
/// (chain, messaging, wasi base, allowlisted http) and withholds the
/// core-only interfaces. Assembling it proves the scope wires without a
/// duplicate-definition clash between the shared `nexum:host` interfaces.
#[tokio::test]
async fn provider_linker_assembles_with_scoped_transport() {
    let engine = make_wasmtime_engine();
    build_provider_linker::<MockTypes>(&engine, &VenueAdapterKind)
        .expect("provider linker assembles");
}

// ── intent-status subscription E2E ────────────────────────────────────

/// A scripted venue adapter for the registry: accepts every submission
/// with a fixed receipt and serves statuses front-first from a script;
/// once drained, every further call reports `open`.
struct ScriptedAdapter {
    statuses: VecDeque<IntentStatus>,
}

impl ScriptedAdapter {
    fn new(statuses: impl IntoIterator<Item = IntentStatus>) -> Self {
        Self {
            statuses: statuses.into_iter().collect(),
        }
    }
}

fn native(bytes: Vec<u8>) -> value_flow::AssetAmount {
    value_flow::AssetAmount {
        asset: value_flow::Asset::Native,
        amount: bytes,
    }
}

impl VenueInvoker for ScriptedAdapter {
    fn derive_header<'a>(
        &'a mut self,
        _body: &'a [u8],
    ) -> BoxFuture<'a, Result<IntentHeader, VenueError>> {
        Box::pin(async move {
            Ok(IntentHeader {
                gives: native(vec![1]),
                wants: native(Vec::new()),
                settlement: Settlement { chain: 1 },
                authorisation: videre_host::bindings::AuthScheme::Eip712,
            })
        })
    }

    fn quote<'a>(&'a mut self, _body: &'a [u8]) -> BoxFuture<'a, Result<Quotation, VenueError>> {
        Box::pin(async move {
            Ok(Quotation {
                gives: native(vec![1]),
                wants: native(Vec::new()),
                fee: native(Vec::new()),
                valid_until_ms: 1_700_000_000_000,
            })
        })
    }

    fn submit<'a>(
        &'a mut self,
        _body: &'a [u8],
    ) -> BoxFuture<'a, Result<SubmitOutcome, VenueError>> {
        Box::pin(async move { Ok(SubmitOutcome::Accepted(b"receipt".to_vec())) })
    }

    fn status(&mut self, _receipt: Vec<u8>) -> BoxFuture<'_, Result<IntentStatus, VenueError>> {
        Box::pin(async move { Ok(self.statuses.pop_front().unwrap_or(IntentStatus::Open)) })
    }

    fn cancel(&mut self, _receipt: Vec<u8>) -> BoxFuture<'_, Result<(), VenueError>> {
        Box::pin(async move { Ok(()) })
    }
}

/// A registry with one scripted adapter installed under `cow`.
fn scripted_registry(adapter: ScriptedAdapter) -> VenueRegistry {
    let registry = VenueRegistryBuilder::new(Default::default()).build();
    registry
        .install(
            VenueId::from("cow"),
            nexum_runtime::host::actor::Liveness::default(),
            adapter,
        )
        .expect("install scripted adapter");
    registry
}

/// Write a manifest subscribing the example module to intent-status
/// events from the `cow` venue.
fn intent_status_manifest(dir: &Path) -> PathBuf {
    let manifest = dir.join("module.toml");
    std::fs::write(
        &manifest,
        r#"
[module]
name = "example"

[capabilities]
required = ["logging"]

[[subscription]]
kind  = "intent-status"
venue = "cow"
"#,
    )
    .expect("write manifest");
    manifest
}

/// Boot the example module against the given videre platform.
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

/// The acceptance path: a module subscribed to `intent-status` receives
/// the transitions the registry observed by polling the adapter's status
/// export, and a transition from a venue outside its filter is not
/// delivered.
#[tokio::test]
async fn e2e_intent_status_subscription_receives_polled_transitions() {
    let Some(wasm) = module_wasm_or_skip("example") else {
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = intent_status_manifest(dir.path());

    let registry = scripted_registry(ScriptedAdapter::new([
        IntentStatus::Pending,
        IntentStatus::Fulfilled,
    ]));
    let videre = Arc::new(Videre::from_registry(registry.clone()));
    let mut supervisor = boot_example(&videre, &wasm, &manifest).await;
    assert!(
        supervisor
            .extension_subscription_kinds()
            .contains(INTENT_STATUS)
    );

    // The registry watches the receipt of an accepted submission and polls
    // the adapter's status export; each poll here observes a transition.
    registry
        .submit("test-caller", &VenueId::from("cow"), b"body".to_vec())
        .await
        .expect("submit");

    let mut delivered = 0;
    for _ in 0..2 {
        for update in registry.poll_status_transitions().await {
            delivered += supervisor
                .dispatch_extension_event(status_event(update))
                .await;
        }
    }
    assert_eq!(delivered, 2, "pending then fulfilled, one subscriber each");
    assert_eq!(supervisor.alive_count(), 1, "module must remain alive");

    // A venue outside the module's filter is not delivered.
    let foreign = videre_host::IntentStatusUpdate {
        venue: "other".to_owned(),
        receipt: b"receipt".to_vec(),
        status: nexum_status_body::StatusBody {
            status: nexum_status_body::IntentStatus::Open,
            proof: None,
            reason: None,
        }
        .encode()
        .expect("encode"),
    };
    assert_eq!(
        supervisor
            .dispatch_extension_event(status_event(foreign))
            .await,
        0
    );
}

/// The event-loop wiring, through the real seam: the platform's `events`
/// source opens against the booted service map, its poll task drives the
/// supervisor, and the module's handler observably ran (its log line is
/// retained).
#[tokio::test]
async fn e2e_intent_status_flows_through_the_event_loop() {
    use nexum_tasks::{TaskManager, TaskSet};

    let Some(wasm) = module_wasm_or_skip("example") else {
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = intent_status_manifest(dir.path());

    let registry = scripted_registry(ScriptedAdapter::new([]));
    let videre = Arc::new(Videre::from_registry(registry.clone()));

    let engine = make_wasmtime_engine();
    let extensions = videre_assembly(&videre);
    let linker = make_linker(&engine, &extensions);
    let components = mock_components();
    let logs = components.logs.clone();
    let limits = ModuleLimits::default();
    let mut supervisor = Supervisor::boot_single(
        &engine,
        &linker,
        &wasm,
        Some(&manifest),
        &components,
        &limits,
        &extensions,
        None,
    )
    .await
    .expect("boot_single");

    registry
        .submit("test-caller", &VenueId::from("cow"), b"body".to_vec())
        .await
        .expect("submit");

    // A fast cadence so the 300 ms window sees the first poll.
    let mut config = EngineConfig::default();
    config.limits.status_poll.interval_ms = Some(10);

    let manager = TaskManager::new();
    let executor = manager.executor();
    let mut tasks = TaskSet::new();
    let subscribed = supervisor.extension_subscription_kinds();
    let streams = {
        let mut sources = EventSources::new(
            &config,
            supervisor.services(),
            &subscribed,
            &executor,
            &mut tasks,
        );
        Extension::<MockTypes>::events(&*videre, &mut sources).expect("open event source")
    };
    assert_eq!(streams.len(), 1, "one status-poll stream opened");

    nexum_runtime::runtime::event_loop::run(
        &mut supervisor,
        Vec::new(),
        Vec::new(),
        streams,
        tasks,
        tokio::time::sleep(Duration::from_millis(300)),
    )
    .await;

    assert_eq!(supervisor.alive_count(), 1, "module must remain alive");
    let runs = logs.list_runs("example");
    assert_eq!(runs.len(), 1, "one run recorded for the example module");
    let page = logs.read(&runs[0].run, 0);
    assert!(
        page.records
            .iter()
            .any(|r| r.message.contains("intent status update from venue cow")),
        "the module's on_intent_status handler ran; records were: {:?}",
        page.records
            .iter()
            .map(|r| r.message.as_str())
            .collect::<Vec<_>>(),
    );
}

/// With no subscriber (or no installed venue) the platform opens no
/// event source.
#[tokio::test]
async fn event_source_stays_closed_without_subscribers_or_venues() {
    use nexum_tasks::{TaskManager, TaskSet};

    let config = EngineConfig::default();
    let manager = TaskManager::new();
    let executor = manager.executor();
    let services = nexum_runtime::host::extension::HostServices::default();

    // A venue is installed but nothing subscribes.
    let with_venue = Arc::new(Videre::from_registry(scripted_registry(
        ScriptedAdapter::new([]),
    )));
    let empty = std::collections::BTreeSet::new();
    let mut tasks = TaskSet::new();
    let mut sources = EventSources::new(&config, &services, &empty, &executor, &mut tasks);
    let streams = Extension::<MockTypes>::events(&*with_venue, &mut sources).expect("events");
    assert!(streams.is_empty(), "no subscriber, no stream");

    // A subscriber exists but no venue is installed.
    let no_venue = Arc::new(platform(&config));
    let subscribed: std::collections::BTreeSet<String> =
        std::iter::once(INTENT_STATUS.to_owned()).collect();
    let mut tasks = TaskSet::new();
    let mut sources = EventSources::new(&config, &services, &subscribed, &executor, &mut tasks);
    let streams = Extension::<MockTypes>::events(&*no_venue, &mut sources).expect("events");
    assert!(streams.is_empty(), "no venue, no stream");
}

// ── echo round trip ───────────────────────────────────────────────────

/// The acceptance path, end to end over two real components: the
/// echo-client module submits through `videre:venue/client`, the host
/// registry forwards to the installed echo-venue adapter, and the module
/// receives the fulfilled `intent-status` the registry polls back. Proves
/// the intent core round-trips module -> host registry -> venue adapter
/// with no scripted stand-ins on either side.
#[tokio::test]
async fn e2e_echo_module_registry_adapter_round_trip() {
    let (Some(adapter_wasm), Some(module_wasm)) = (
        module_wasm_or_skip("echo-venue"),
        module_wasm_or_skip("echo-client"),
    ) else {
        return;
    };

    // The adapter reads eth_blockNumber on submit to justify its `chain`
    // grant; program the mock so that read succeeds. The response body is
    // discarded by the adapter, so any Ok value serves.
    let chain = MockChainProvider::new();
    chain.on_method(ChainMethod::EthBlockNumber, "\"0x1\"");
    let components = nexum_runtime::test_utils::mock_components_from(chain, MockStateStore::new());
    let logs = components.logs.clone();

    let engine = make_wasmtime_engine();
    let config = EngineConfig {
        adapters: vec![AdapterEntry {
            path: adapter_wasm,
            manifest: Some(workspace_path("modules/examples/echo-venue/module.toml")),
            http_allow: Vec::new(),
            messaging_topics: Vec::new(),
        }],
        modules: vec![ModuleEntry {
            path: module_wasm,
            manifest: Some(workspace_path("modules/examples/echo-client/module.toml")),
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
    assert_eq!(
        supervisor.adapter_alive_count(),
        1,
        "echo-venue is routable"
    );
    assert_eq!(supervisor.alive_count(), 1, "echo-client is alive");
    assert!(
        supervisor
            .extension_subscription_kinds()
            .contains(INTENT_STATUS)
    );

    // A block drives the module's on_block, which submits to the echo venue
    // through the shared registry; the registry watches the accepted receipt.
    assert_eq!(supervisor.dispatch_block(block(1)).await, 1);

    // Poll the registry the module submitted through and fan its transitions
    // back to the module. echo-venue settles instantly, so the first poll
    // reports a terminal status and the watch is pruned.
    let registry = registry_of(&supervisor);
    let mut delivered = 0;
    for _ in 0..2 {
        for update in registry.poll_status_transitions().await {
            assert_eq!(update.venue, "echo-venue");
            let body =
                nexum_status_body::StatusBody::decode(&update.status).expect("status body decodes");
            assert_eq!(
                body.status,
                nexum_status_body::IntentStatus::Fulfilled,
                "echo settles instantly",
            );
            delivered += supervisor
                .dispatch_extension_event(status_event(update))
                .await;
        }
    }
    assert_eq!(
        delivered, 1,
        "one terminal status delivered to the subscriber"
    );
    assert_eq!(supervisor.alive_count(), 1, "module must remain alive");

    // The module observably completed the round trip: it quoted, it
    // submitted, and it received the settled status from the echo venue.
    let runs = logs.list_runs("echo-client");
    assert_eq!(runs.len(), 1, "one run recorded for echo-client");
    let page = logs.read(&runs[0].run, 0);
    let messages: Vec<&str> = page.records.iter().map(|r| r.message.as_str()).collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("quoted") && m.contains("echo-venue")),
        "module quoted through the client face; records were: {messages:?}",
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("submitted") && m.contains("echo-venue")),
        "module submitted through the client face; records were: {messages:?}",
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("intent status from venue echo-venue")),
        "module received the settled status; records were: {messages:?}",
    );
}

/// The body-version handshake refuses a mismatched pair: an adapter
/// decoding only v1 against a keeper encoding v2 fails the boot at the
/// keeper's install, before instantiation, naming both sides' versions.
#[tokio::test]
async fn e2e_mismatched_body_versions_refuse_the_pair_at_boot() {
    let (Some(adapter_wasm), Some(module_wasm)) = (
        module_wasm_or_skip("echo-venue"),
        module_wasm_or_skip("echo-client"),
    ) else {
        return;
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let adapter_manifest = dir.path().join("echo-venue.toml");
    std::fs::write(
        &adapter_manifest,
        r#"
[module]
name = "echo-venue"
kind = "venue-adapter"

[capabilities]
required = ["chain"]

[venue]
body_versions = [1]
"#,
    )
    .expect("write adapter manifest");
    let keeper_manifest = dir.path().join("echo-client.toml");
    std::fs::write(
        &keeper_manifest,
        r#"
[module]
name = "echo-client"

[capabilities]
required = ["client", "logging"]

[venue]
body_version = 2
"#,
    )
    .expect("write keeper manifest");

    let engine = make_wasmtime_engine();
    let config = EngineConfig {
        adapters: vec![AdapterEntry {
            path: adapter_wasm,
            manifest: Some(adapter_manifest),
            http_allow: Vec::new(),
            messaging_topics: Vec::new(),
        }],
        modules: vec![ModuleEntry {
            path: module_wasm,
            manifest: Some(keeper_manifest),
        }],
        ..Default::default()
    };
    let videre = Arc::new(platform(&config));
    let extensions = videre_assembly(&videre);
    let linker = make_linker(&engine, &extensions);
    let components = mock_components();

    let Err(err) =
        Supervisor::boot(&engine, &linker, &config, &components, &extensions, None).await
    else {
        panic!("mismatched pair must refuse to boot");
    };
    let chain = format!("{err:#}");
    assert!(chain.contains("body version 2"), "{chain}");
    assert!(chain.contains("echo-venue decodes {1}"), "{chain}");
}

/// An adapter whose manifest claims versions its code does not decode
/// fails its own install: the `body-versions()` export must equal the
/// manifest `[venue] body_versions` set.
#[tokio::test]
async fn e2e_manifest_export_divergence_refuses_the_adapter_at_boot() {
    let Some(adapter_wasm) = module_wasm_or_skip("echo-venue") else {
        return;
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let adapter_manifest = dir.path().join("echo-venue.toml");
    std::fs::write(
        &adapter_manifest,
        r#"
[module]
name = "echo-venue"
kind = "venue-adapter"

[capabilities]
required = ["chain"]

[venue]
body_versions = [1, 2]
"#,
    )
    .expect("write adapter manifest");

    let engine = make_wasmtime_engine();
    let config = EngineConfig {
        adapters: vec![AdapterEntry {
            path: adapter_wasm,
            manifest: Some(adapter_manifest),
            http_allow: Vec::new(),
            messaging_topics: Vec::new(),
        }],
        ..Default::default()
    };
    let videre = Arc::new(platform(&config));
    let extensions = videre_assembly(&videre);
    let linker = make_linker(&engine, &extensions);
    let components = mock_components();

    let Err(err) =
        Supervisor::boot(&engine, &linker, &config, &components, &extensions, None).await
    else {
        panic!("a diverging adapter must refuse to boot");
    };
    let chain = format!("{err:#}");
    assert!(chain.contains("exports body versions {1}"), "{chain}");
    assert!(chain.contains("declares {1, 2}"), "{chain}");
}

// ── venue-adapter trap recovery ───────────────────────────────────────

/// Boot one flaky-venue adapter over the mock chain, whose head starts at
/// the fixture's poison sentinel. Returns the chain handle so the test can
/// let the venue recover.
async fn boot_flaky_venue(
    adapter_wasm: PathBuf,
    limits: ModuleLimits,
) -> (Supervisor<MockTypes>, MockChainProvider) {
    let chain = MockChainProvider::new();
    chain.on_method(ChainMethod::EthBlockNumber, "\"0xdead\"");
    let components =
        nexum_runtime::test_utils::mock_components_from(chain.clone(), MockStateStore::new());
    let engine = make_wasmtime_engine();
    let config = EngineConfig {
        adapters: vec![AdapterEntry {
            path: adapter_wasm,
            manifest: Some(workspace_path("modules/fixtures/flaky-venue/module.toml")),
            http_allow: Vec::new(),
            messaging_topics: Vec::new(),
        }],
        limits,
        ..Default::default()
    };
    let videre = Arc::new(platform(&config));
    let extensions = videre_assembly(&videre);
    let linker = make_linker(&engine, &extensions);
    let supervisor = Supervisor::boot(&engine, &linker, &config, &components, &extensions, None)
        .await
        .expect("boot");
    (supervisor, chain)
}

/// The full trap-to-recovery lifecycle over a real wasm adapter: a trapped
/// venue is temporarily dead (`unavailable`, not `unknown-venue`) and the
/// provider restart sweep reinstantiates it after backoff, after which a
/// submit succeeds again.
#[tokio::test]
async fn e2e_trapped_adapter_is_swept_and_restarts() {
    let Some(wasm) = module_wasm_or_skip("flaky-venue") else {
        return;
    };
    let (mut supervisor, chain) = boot_flaky_venue(wasm, ModuleLimits::default()).await;
    assert_eq!(supervisor.adapter_count(), 1);
    assert_eq!(supervisor.adapter_alive_count(), 1, "boots alive");
    let registry = registry_of(&supervisor);
    let venue = VenueId::from("flaky-venue");

    // The poison head detonates submit: the guest panic traps the store
    // and the shared liveness drops.
    let err = registry
        .submit("mod-a", &venue, b"body".to_vec())
        .await
        .expect_err("the poison head traps the adapter");
    assert!(matches!(err, VenueError::Unavailable(_)), "{err:?}");
    assert_eq!(
        supervisor.adapter_alive_count(),
        0,
        "the trap drops liveness"
    );

    // Temporarily dead resolves distinctly from never installed.
    assert!(matches!(
        registry.submit("mod-a", &venue, b"body".to_vec()).await,
        Err(VenueError::Unavailable(_))
    ));
    assert!(matches!(
        registry
            .submit("mod-a", &VenueId::from("unlisted"), b"body".to_vec())
            .await,
        Err(VenueError::UnknownVenue)
    ));

    // The venue recovers; past the 1s backoff the dispatch-time sweep
    // reinstalls the adapter on a fresh store.
    chain.on_method(ChainMethod::EthBlockNumber, "\"0x1\"");
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    supervisor.dispatch_block(block(1)).await;
    assert_eq!(supervisor.adapter_alive_count(), 1, "the sweep revived it");
    let outcome = registry
        .submit("mod-a", &venue, b"body".to_vec())
        .await
        .expect("the recovered adapter accepts");
    assert!(matches!(outcome, SubmitOutcome::Accepted(r) if r == b"body"));
}

/// A crash-looping adapter is quarantined by the provider poison sweep:
/// at the threshold the restarts stop, and the venue stays dead past every
/// backoff until an operator intervenes.
#[tokio::test]
async fn e2e_crash_looping_adapter_is_poisoned() {
    let Some(wasm) = module_wasm_or_skip("flaky-venue") else {
        return;
    };
    let limits = ModuleLimits {
        poison: PoisonLimitsSection {
            max_failures: Some(2),
            window_secs: Some(600),
        },
        ..ModuleLimits::default()
    };
    // The chain head stays at the poison sentinel for the whole test: every
    // submit after a restart traps again.
    let (mut supervisor, _chain) = boot_flaky_venue(wasm, limits).await;
    let registry = registry_of(&supervisor);
    let venue = VenueId::from("flaky-venue");

    // Trap 1, then a successful restart past the 1s backoff.
    let _ = registry.submit("mod-a", &venue, b"body".to_vec()).await;
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    supervisor.dispatch_block(block(1)).await;
    assert_eq!(supervisor.adapter_alive_count(), 1, "first restart lands");

    // Trap 2 crosses the 2-failure threshold: the sweep quarantines the
    // adapter instead of scheduling another restart.
    let _ = registry.submit("mod-a", &venue, b"body".to_vec()).await;
    supervisor.dispatch_block(block(1)).await;
    assert_eq!(supervisor.adapter_alive_count(), 0, "quarantined");

    // Past every backoff the poisoned adapter stays dead and unavailable.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    supervisor.dispatch_block(block(1)).await;
    assert_eq!(
        supervisor.adapter_alive_count(),
        0,
        "no restart while poisoned"
    );
    assert!(matches!(
        registry.submit("mod-a", &venue, b"body".to_vec()).await,
        Err(VenueError::Unavailable(_))
    ));
}
