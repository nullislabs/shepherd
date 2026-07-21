use std::path::{Path, PathBuf};

use super::*;
use crate::engine_config::ModuleLimits;
use crate::manifest::ResourceSection;

#[test]
fn module_limits_default_to_engine_limits_when_unset() {
    let cfg = ModuleLimits::default();
    let resolved = resolve_module_limits(&ResourceSection::default(), &cfg);
    assert_eq!(resolved.fuel, cfg.fuel());
    assert_eq!(resolved.memory, cfg.memory());
    assert_eq!(resolved.state_bytes, cfg.state_bytes());
}

#[test]
fn manifest_resource_overrides_take_effect_and_are_field_local() {
    let cfg = ModuleLimits::default();
    // Only fuel is overridden; memory + state keep the engine defaults.
    let res = ResourceSection {
        max_memory_bytes: None,
        max_fuel_per_event: Some(100_000),
        max_state_bytes: Some(2048),
    };
    let resolved = resolve_module_limits(&res, &cfg);
    assert_eq!(resolved.fuel, 100_000);
    assert_eq!(resolved.memory, cfg.memory());
    assert_eq!(resolved.state_bytes, 2048);
}

/// A manifest section a wired extension claims passes; an unclaimed one
/// (a typo, or a section for an unwired extension) is refused.
#[test]
fn extension_sections_must_be_claimed() {
    struct Claiming;
    impl Extension<TestTypes> for Claiming {
        fn namespace(&self) -> &'static str {
            "acme"
        }
        fn capabilities(&self) -> crate::manifest::NamespaceCaps {
            crate::manifest::NamespaceCaps {
                prefix: "acme:ext/",
                ifaces: &[],
            }
        }
        fn link(&self, _linker: &mut Linker<HostState<TestTypes>>) -> anyhow::Result<()> {
            Ok(())
        }
        fn manifest_sections(&self) -> &'static [&'static str] {
            &["venue"]
        }
    }
    let extensions: Vec<Arc<dyn Extension<TestTypes>>> = vec![Arc::new(Claiming)];

    let mut sections = manifest::ExtensionSections::new();
    sections.insert("venue".into(), toml::Value::Boolean(true));
    enforce_extension_sections("keeper", &sections, &extensions).expect("claimed section");

    sections.insert("venu".into(), toml::Value::Boolean(true));
    let err = enforce_extension_sections("keeper", &sections, &extensions)
        .expect_err("unclaimed section");
    assert!(err.to_string().contains("[venu]"), "{err}");
    assert!(err.to_string().contains("keeper"), "{err}");
}

/// Two extensions colliding on a subscription kind or a manifest section
/// are refused at boot; a non-colliding set passes the uniqueness pass.
#[test]
fn extension_claims_must_be_unique() {
    struct Claiming {
        namespace: &'static str,
        subscriptions: &'static [&'static str],
        sections: &'static [&'static str],
    }
    impl Extension<TestTypes> for Claiming {
        fn namespace(&self) -> &'static str {
            self.namespace
        }
        fn capabilities(&self) -> crate::manifest::NamespaceCaps {
            crate::manifest::NamespaceCaps {
                prefix: "acme:ext/",
                ifaces: &[],
            }
        }
        fn link(&self, _linker: &mut Linker<HostState<TestTypes>>) -> anyhow::Result<()> {
            Ok(())
        }
        fn subscriptions(&self) -> &'static [&'static str] {
            self.subscriptions
        }
        fn manifest_sections(&self) -> &'static [&'static str] {
            self.sections
        }
    }
    fn ext(
        namespace: &'static str,
        subscriptions: &'static [&'static str],
        sections: &'static [&'static str],
    ) -> Arc<dyn Extension<TestTypes>> {
        Arc::new(Claiming {
            namespace,
            subscriptions,
            sections,
        })
    }

    enforce_extension_uniqueness(&[
        ext("a", &["orders"], &["venue"]),
        ext("b", &["fills"], &["pool"]),
    ])
    .expect("non-colliding set boots");

    let err = enforce_extension_uniqueness(&[
        ext("a", &["orders"], &["venue"]),
        ext("b", &["orders"], &["pool"]),
    ])
    .expect_err("duplicate subscription kind");
    assert!(err.to_string().contains("orders"), "{err}");

    let err = enforce_extension_uniqueness(&[
        ext("a", &["orders"], &["venue"]),
        ext("b", &["fills"], &["venue"]),
    ])
    .expect_err("duplicate manifest section");
    assert!(err.to_string().contains("[venue]"), "{err}");
}

#[tokio::test]
async fn empty_supervisor_returns_no_subscriptions() {
    let engine = make_wasmtime_engine();
    let sup = boot_mock_supervisor(&engine).await;
    assert!(sup.block_chains().is_empty());
    assert!(sup.chain_log_subscriptions().is_empty());
    assert_eq!(sup.module_count(), 0);
}

/// Data-compat guard: the persisted progress marker keys on the numeric
/// chain id, never the `Chain` `Display` name. A named chain must still
/// yield `last_dispatched_block:11155111`, not `...:sepolia`, so existing
/// redb entries keep resolving after this refactor.
#[test]
fn progress_marker_key_uses_numeric_chain_id() {
    let chain = Chain::from_id(11_155_111);
    assert_eq!(progress_key(chain), "last_dispatched_block:11155111");
}

/// Regression guard: engines whose modules only declare
/// `[[subscription]] kind = "block"` (or only `kind = "chain-log"`) must not
/// bail at boot. Previously `select_all` on an empty `Vec` yielded
/// `None` immediately and the "stream ended -> shut down" arm fired
/// before any event flowed. The fix in `runtime/event_loop.rs`
/// substitutes `stream::pending()` when the Vec is empty so the
/// corresponding select arm is never selected.
///
/// Surfaced when wiring up `engine.m3.toml` for the M3 testnet runbook:
/// the 3 M3 example modules (price-alert, balance-tracker, stop-loss)
/// all subscribe to blocks only, no logs. The engine bailed within
/// ~50 ms of `supervisor ready` until this fix landed.
#[tokio::test]
async fn run_does_not_bail_when_both_stream_kinds_are_empty() {
    use std::time::{Duration, Instant};

    let engine = make_wasmtime_engine();
    let mut supervisor = boot_mock_supervisor(&engine).await;
    let started = Instant::now();
    let shutdown = tokio::time::sleep(Duration::from_millis(50));

    crate::runtime::event_loop::run(
        &mut supervisor,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        nexum_tasks::TaskSet::new(),
        shutdown,
    )
    .await;

    // If the bug were present, `run` returns ~0 ms (the empty `logs`
    // stream's first `.next()` yields `None` and the loop bails on
    // the bail-on-None arm). With the fix, `run` blocks on `shutdown`
    // for the full 50 ms.
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(40),
        "run returned in {elapsed:?}, expected >= ~50ms (shutdown timer)",
    );
}

// ── event_loop integration tests (#56 + #58) ─────────────────────────
//
// Verify the stream-open + run() + shutdown lifecycle end to end at the
// supervisor boundary, without loading a real wasm module.

/// Block and chain-log streams are both consumed within the same `run()`
/// session — the `biased` select does not starve either event kind. One
/// item of each kind is queued before the loop starts; `run()`'s returned
/// tally must show both were drained. A regression that breaks either
/// select arm (or reorders the `biased` polling so one side never fires)
/// leaves its count at 0 and fails the assertion. Issue #56.
#[tokio::test]
async fn run_delivers_block_and_chain_log_events_without_starvation() {
    use std::time::Duration;

    use alloy_chains::Chain;
    use alloy_rpc_types_eth::Filter;

    use crate::runtime::event_loop::{open_block_streams, open_chain_log_streams, run};
    use crate::test_utils::MockChainProvider;
    use nexum_tasks::{TaskManager, TaskSet};

    let engine = make_wasmtime_engine();
    let mut supervisor = boot_mock_supervisor(&engine).await;
    let pool = MockChainProvider::new();
    let manager = TaskManager::new();
    let executor = manager.executor();
    let mut tasks = TaskSet::new();

    // Pre-push one event of each kind before the loop starts so both mpsc
    // channels have an item for `run()` to drain on its first pass.
    pool.push_block(alloy_rpc_types_eth::Header::default());
    pool.push_chain_log(alloy_rpc_types_eth::Log::default());

    let block_streams = open_block_streams(&pool, &[Chain::mainnet()], &executor, &mut tasks);
    let log_subs = vec![crate::supervisor::ChainLogSub {
        module: "test-module".to_string(),
        chain: Chain::mainnet(),
        filter: Filter::default(),
        cursor_key: None,
        initial_cursor: None,
        max_lookback: None,
    }];
    let chain_log_streams = open_chain_log_streams(&pool, log_subs, &executor, &mut tasks);

    // The shutdown window only bounds wall time; the assertion is on the
    // tally, not on timing. 500 ms is orders of magnitude more than the
    // two channel hops need, so a miss means a broken select arm, not a
    // slow scheduler.
    let shutdown = tokio::time::sleep(Duration::from_millis(500));
    let (blocks, chain_logs) = tokio::time::timeout(
        Duration::from_secs(10),
        run(
            &mut supervisor,
            block_streams,
            chain_log_streams,
            Vec::new(),
            tasks,
            shutdown,
        ),
    )
    .await
    .expect("run() must return once shutdown fires");
    assert_eq!(blocks, 1, "the queued block must be drained and dispatched");
    assert_eq!(
        chain_logs, 1,
        "the queued chain-log must be drained and dispatched",
    );
}

/// After `run()` returns on the shutdown path, all reconnect tasks are
/// drained: the Shutdown arm calls `tasks.shutdown()`, which aborts every
/// handle and then joins each one, so no task detaches and outlives the
/// engine. (The companion contract — a task parked on a dropped receiver
/// exits with `ReceiverGone` on its own — is asserted directly in
/// `event_loop::tests::reconnect_task_exits_receiver_gone_when_receiver_drops`;
/// it cannot be observed here because `TaskSet::shutdown` aborts first.)
/// Issue #58.
#[tokio::test]
async fn run_drains_reconnect_tasks_cleanly_on_shutdown() {
    use std::time::Duration;

    use alloy_chains::Chain;

    use crate::runtime::event_loop::{open_block_streams, run};
    use crate::test_utils::MockChainProvider;
    use nexum_tasks::{TaskManager, TaskSet};

    let engine = make_wasmtime_engine();
    let mut supervisor = boot_mock_supervisor(&engine).await;
    let pool = MockChainProvider::new();
    let manager = TaskManager::new();
    let executor = manager.executor();
    let mut tasks = TaskSet::new();

    // Two subscription tasks — both must drain before `run()` returns.
    let block_streams = open_block_streams(
        &pool,
        &[Chain::mainnet(), Chain::from_id(100)],
        &executor,
        &mut tasks,
    );

    let shutdown = tokio::time::sleep(Duration::from_millis(10));
    // If the drain were absent, the spawned reconnect tasks would detach
    // and outlive the supervisor; if the drain hung, the timeout fails
    // fast instead of stalling the suite until the CI job limit.
    tokio::time::timeout(
        Duration::from_secs(10),
        run(
            &mut supervisor,
            block_streams,
            vec![],
            Vec::new(),
            tasks,
            shutdown,
        ),
    )
    .await
    .expect("run() + task drain must complete promptly after shutdown");
}

// ── E2E helpers ───────────────────────────────────────────────────────

/// Path to the pre-built example WASM component. Tests that need it
/// call `example_wasm_or_skip()` which skips gracefully if absent.
fn example_wasm() -> PathBuf {
    // CARGO_MANIFEST_DIR → crates/nexum-runtime
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("target/wasm32-wasip2/release/example.wasm")
}

fn example_module_toml() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("modules/example/module.toml")
}

/// Returns `None` and prints a skip message if the fixture isn't built.
fn example_wasm_or_skip() -> Option<PathBuf> {
    let p = example_wasm();
    if p.exists() {
        Some(p)
    } else {
        eprintln!(
            "SKIP: {} not found - run `just build-module` to enable E2E tests",
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

/// The core-only extension set: no domain extensions. Domain-extension
/// boot coverage lives in the extension crate that owns the backend.
fn core_extensions() -> Vec<Arc<dyn crate::host::extension::Extension<TestTypes>>> {
    Vec::new()
}

fn make_linker(engine: &wasmtime::Engine) -> Linker<HostState<TestTypes>> {
    crate::supervisor::build_linker::<TestTypes>(engine, &core_extensions()).expect("build_linker")
}

/// Synthetic component bundle for tests: an empty chain pool, an empty
/// extension slot, and the given store.
fn test_components(store: crate::host::local_store_redb::LocalStore) -> Components<TestTypes> {
    Components {
        chain: ProviderPool::empty(),
        store,
        ext: (),
        logs: crate::test_utils::in_memory_logs(),
    }
}

/// Return `(dir, store)` so the test holds the `TempDir` for the
/// duration of the test scope and cleans it up on drop. Forgetting
/// the dir (the old `ManuallyDrop` approach) leaks it for the
/// entire process lifetime.
fn temp_local_store() -> (tempfile::TempDir, crate::host::local_store_redb::LocalStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ls.redb");
    let store = crate::host::local_store_redb::LocalStore::open(path).expect("local store");
    (dir, store)
}

/// Boot a zero-module supervisor over the in-process mock backends via the
/// real `boot` path. The default config declares no modules, so `boot`
/// returns with an empty module set, touching neither disk nor network.
async fn boot_mock_supervisor(
    engine: &wasmtime::Engine,
) -> Supervisor<crate::test_utils::MockTypes> {
    let components = crate::test_utils::mock_components();
    let config = EngineConfig::default();
    let linker = crate::supervisor::build_linker::<crate::test_utils::MockTypes>(engine, &[])
        .expect("build_linker");
    Supervisor::boot(engine, &linker, &config, &components, &[], None)
        .await
        .expect("boot mock supervisor")
}

// ── E2E tests ─────────────────────────────────────────────────────────

/// Boot supervisor with the example module; verify it starts alive.
#[tokio::test]
async fn e2e_supervisor_boots_example_module() {
    let Some(wasm) = example_wasm_or_skip() else {
        return;
    };
    let engine = make_wasmtime_engine();
    let linker = make_linker(&engine);
    let (_dir, local_store) = temp_local_store();
    let components = test_components(local_store);

    let limits = ModuleLimits::default();
    let supervisor = Supervisor::boot_single(
        &engine,
        &linker,
        &wasm,
        Some(example_module_toml()).as_deref(),
        &components,
        &limits,
        &core_extensions(),
        None,
    )
    .await
    .expect("boot_single");

    assert_eq!(supervisor.module_count(), 1);
    assert_eq!(supervisor.alive_count(), 1);
}

/// The per-module world contract: the example component's
/// capability-bearing imports are exactly what its manifest declares
/// (`logging`), by construction of the emitted world rather than by
/// the toolchain eliding unused imports of a blanket world.
#[test]
fn e2e_example_component_imports_equal_declared_capabilities() {
    let Some(wasm) = example_wasm_or_skip() else {
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
        std::collections::BTreeSet::from(["logging"]),
        "imports were: {imports:?}"
    );

    // No extension interface leaks in either: the per-module world holds
    // exactly what the manifest declared.
    assert!(
        imports
            .iter()
            .all(|name| name.starts_with("nexum:host/") || name.starts_with("wasi:")),
        "imports were: {imports:?}"
    );
}

/// Boot with a manifest that subscribes to block events; dispatch one
/// block event and verify the module was invoked and stayed alive.
#[tokio::test]
async fn e2e_block_subscription_dispatched() {
    let Some(wasm) = example_wasm_or_skip() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let manifest = dir.path().join("module.toml");
    std::fs::write(
        &manifest,
        r#"
[module]
name = "example"

[capabilities]
required = ["logging"]

[[subscription]]
kind     = "block"
chain_id = 1
"#,
    )
    .unwrap();

    let engine = make_wasmtime_engine();
    let linker = make_linker(&engine);
    let (_dir, local_store) = temp_local_store();
    let components = test_components(local_store);
    let limits = ModuleLimits::default();

    let mut supervisor = Supervisor::boot_single(
        &engine,
        &linker,
        &wasm,
        Some(&manifest),
        &components,
        &limits,
        &core_extensions(),
        None,
    )
    .await
    .expect("boot_single");

    let block = nexum::host::types::Block {
        chain_id: 1,
        number: 19_000_000,
        hash: vec![0xab; 32],
        timestamp: 1_700_000_000_000,
    };
    let dispatched = supervisor.dispatch_block(block).await;
    assert_eq!(dispatched, 1, "one module subscribed to chain 1 blocks");
    assert_eq!(supervisor.alive_count(), 1, "module must remain alive");
}

/// A `ManualClock` override threads through `boot_single` onto the module
/// store and is behaviour-neutral: the module boots, dispatches a block, and
/// stays alive exactly as it does on the ambient clock. Locks the plumbing so
/// the seam keeps reaching the store on the boot path.
#[cfg(feature = "test-utils")]
#[tokio::test]
async fn e2e_manual_clock_override_boots_and_dispatches() {
    use std::time::{Duration, UNIX_EPOCH};

    use crate::test_utils::clock::ManualClock;

    let Some(wasm) = example_wasm_or_skip() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let manifest = dir.path().join("module.toml");
    std::fs::write(
        &manifest,
        r#"
[module]
name = "example"

[capabilities]
required = ["logging"]

[[subscription]]
kind     = "block"
chain_id = 1
"#,
    )
    .unwrap();

    let engine = make_wasmtime_engine();
    let linker = make_linker(&engine);
    let (_dir, local_store) = temp_local_store();
    let components = test_components(local_store);
    let limits = ModuleLimits::default();

    let clock = ManualClock::new();
    clock.set(UNIX_EPOCH + Duration::from_secs(1_700_000_000));

    let mut supervisor = Supervisor::boot_single(
        &engine,
        &linker,
        &wasm,
        Some(&manifest),
        &components,
        &limits,
        &core_extensions(),
        Some(clock.as_override()),
    )
    .await
    .expect("boot_single with a manual clock override");

    let block = nexum::host::types::Block {
        chain_id: 1,
        number: 19_000_000,
        hash: vec![0xab; 32],
        timestamp: 1_700_000_000_000,
    };
    let dispatched = supervisor.dispatch_block(block).await;
    assert_eq!(dispatched, 1, "the overridden-clock module dispatched");
    assert_eq!(supervisor.alive_count(), 1, "module must remain alive");

    // Advancing the shared handle is observable on the same source the store
    // reads; the boot path did not clone away from it.
    clock.advance(Duration::from_secs(1));
    assert_eq!(
        wasmtime_wasi::HostWallClock::now(&clock),
        Duration::from_secs(1_700_000_001),
    );
}

// ── Production module integration tests ────────────────────
//
// One test per module that goes through the real wit-bindgen +
// WitBindgenHost adapter + supervisor dispatch path, not just the
// strategy-level MockHost coverage. Mirrors the example-module e2e
// shape above; each test is guarded by `module_wasm_or_skip()` so
// local runs without a fresh `--target wasm32-wasip2 --release`
// build are skipped rather than failing.

const SEPOLIA: u64 = 11_155_111;

/// Path to a production module's .wasm artefact under the workspace
/// target dir. `Cargo` writes the artefact as `<name>.wasm` with
/// hyphens replaced by underscores, so the helper mirrors that.
fn module_wasm(module_name: &str) -> PathBuf {
    let artifact = module_name.replace('-', "_");
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(format!("target/wasm32-wasip2/release/{artifact}.wasm"))
}

fn module_wasm_or_skip(module_name: &str) -> Option<PathBuf> {
    let p = module_wasm(module_name);
    if p.exists() {
        Some(p)
    } else if std::env::var_os("CI").is_some() {
        // The CI test job builds every module wasm before running the
        // suite, so a missing artifact here means the pipeline regressed.
        // Fail loudly rather than skip into a hollow green.
        panic!(
            "{} not found under CI - the test job must build the module wasms before the suite runs",
            p.display()
        );
    } else {
        eprintln!(
            "SKIP: {} not found - build with `cargo build -p {module_name} --target wasm32-wasip2 --release`",
            p.display()
        );
        None
    }
}

/// Resolve a real `module.toml` for one of the production modules.
/// Looking up the real manifest (rather than synthesising one) keeps
/// the integration test honest about the capability set + subscription
/// shape each module actually ships.
fn production_module_toml(relative_path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(relative_path)
}

fn synthetic_sepolia_block() -> nexum::host::types::Block {
    nexum::host::types::Block {
        chain_id: SEPOLIA,
        number: 19_000_000,
        hash: vec![0xab; 32],
        timestamp: 1_700_000_000_000,
    }
}

/// Boot a single module from `(wasm, manifest)` and return the live
/// supervisor. Shared body across the 5 integration tests.
async fn boot_production_module(
    engine: &wasmtime::Engine,
    linker: &Linker<HostState<TestTypes>>,
    local_store: &crate::host::local_store_redb::LocalStore,
    wasm: &Path,
    manifest: &Path,
) -> DefaultSupervisor {
    let components = test_components(local_store.clone());
    let limits = ModuleLimits::default();
    Supervisor::boot_single(
        engine,
        linker,
        wasm,
        Some(manifest),
        &components,
        &limits,
        &core_extensions(),
        None,
    )
    .await
    .expect("boot_single")
}

#[tokio::test]
async fn e2e_price_alert_block_dispatch() {
    let Some(wasm) = module_wasm_or_skip("price-alert") else {
        return;
    };
    let manifest = production_module_toml("modules/examples/price-alert/module.toml");
    let engine = make_wasmtime_engine();
    let linker = make_linker(&engine);
    let (_dir, store) = temp_local_store();

    let mut supervisor = boot_production_module(&engine, &linker, &store, &wasm, &manifest).await;
    let dispatched = supervisor.dispatch_block(synthetic_sepolia_block()).await;
    assert_eq!(dispatched, 1);
    assert_eq!(supervisor.alive_count(), 1);
}

#[tokio::test]
async fn e2e_balance_tracker_block_dispatch() {
    let Some(wasm) = module_wasm_or_skip("balance-tracker") else {
        return;
    };
    let manifest = production_module_toml("modules/examples/balance-tracker/module.toml");
    let engine = make_wasmtime_engine();
    let linker = make_linker(&engine);
    let (_dir, store) = temp_local_store();

    let mut supervisor = boot_production_module(&engine, &linker, &store, &wasm, &manifest).await;
    let dispatched = supervisor.dispatch_block(synthetic_sepolia_block()).await;
    assert_eq!(dispatched, 1);
    assert_eq!(supervisor.alive_count(), 1);
}

/// End-to-end wasi:http path: http-probe fetches a loopback server
/// admitted by its allowlist, then fetches an off-list host and
/// requires the HTTP-request-denied outcome inside the guest. The
/// module returns `Ok` from `on_event` only when both legs hold, so
/// `dispatched == 1` asserts the success AND denied paths together.
/// The off-list host is never resolved or dialled (the gate denies
/// before any connection), so the test needs no external network.
#[tokio::test]
async fn e2e_http_probe_allowlisted_fetch_and_denied_path() {
    let Some(wasm) = module_wasm_or_skip("http-probe") else {
        return;
    };
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/status"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let manifest = dir.path().join("module.toml");
    std::fs::write(
        &manifest,
        format!(
            r#"
[module]
name = "http-probe"

[capabilities]
required = ["logging", "http"]

[capabilities.http]
allow = ["127.0.0.1"]

[[subscription]]
kind     = "block"
chain_id = 1

[config]
probe_url  = "{}/status"
denied_url = "http://denied.invalid/"
"#,
            server.uri(),
        ),
    )
    .unwrap();

    let engine = make_wasmtime_engine();
    let linker = make_linker(&engine);
    let (_store_dir, store) = temp_local_store();

    let mut supervisor = boot_production_module(&engine, &linker, &store, &wasm, &manifest).await;
    let block = nexum::host::types::Block {
        chain_id: 1,
        number: 19_000_000,
        hash: vec![0xab; 32],
        timestamp: 1_700_000_000_000,
    };
    let dispatched = supervisor.dispatch_block(block).await;
    assert_eq!(
        dispatched, 1,
        "both http-probe legs (allowlisted fetch + denied off-list fetch) must succeed",
    );
    assert_eq!(supervisor.alive_count(), 1);
}

// ── Init-failed modules must be marked dead ────────────────

/// Drive `Supervisor::boot_single` with a module whose `[config]`
/// carries a malformed `threshold` value (`"not-a-number"`). The
/// module's `init` returns `Err(fault.invalid-input)`.
/// Previously the supervisor still marked the module
/// `alive = true`, so it received block dispatches forever. The fix
/// flips `alive = false` when `init` fails.
///
/// Surfaced live on Sepolia in
/// `docs/operations/m3-edge-case-validation.md` scenario 1.4.
#[tokio::test]
async fn init_failure_marks_module_dead_and_excludes_from_dispatch() {
    let Some(wasm) = module_wasm_or_skip("price-alert") else {
        return;
    };

    // Synthesise a manifest with the same shape as the real
    // price-alert module but with a `threshold` that the strategy
    // rejects in `parse_config`.
    let dir = tempfile::tempdir().unwrap();
    let manifest = dir.path().join("module.toml");
    std::fs::write(
        &manifest,
        r#"
[module]
name = "price-alert"

[capabilities]
required = ["logging", "chain"]

[[subscription]]
kind     = "block"
chain_id = 11155111

[config]
oracle_address = "0x694AA1769357215DE4FAC081bf1f309aDC325306"
decimals       = "8"
threshold      = "not-a-number"
direction      = "below"
every_n_blocks = "1"
"#,
    )
    .unwrap();

    let engine = make_wasmtime_engine();
    let linker = make_linker(&engine);
    let (_dir, store) = temp_local_store();

    let mut supervisor = boot_production_module(&engine, &linker, &store, &wasm, &manifest).await;

    // The module loaded successfully (wasm compiled, capabilities
    // matched, manifest parsed) but `init` returned InvalidInput.
    assert_eq!(supervisor.module_count(), 1, "module is loaded");
    assert_eq!(
        supervisor.alive_count(),
        0,
        "init-failed module must be marked dead",
    );

    // Dispatch the synthetic block. The init-failed module must
    // not be reached by the dispatcher.
    let dispatched = supervisor.dispatch_block(synthetic_sepolia_block()).await;
    assert_eq!(
        dispatched, 0,
        "no live module is subscribed to chain 11155111 blocks",
    );
}

/// Dead modules (here: init-failed, `alive = false`) must not contribute
/// their chain to `block_chains()` or `chain_log_subscriptions()`. Without
/// the alive filter the builder opens live RPC subscriptions against chains
/// that will never dispatch to any module, wasting connections and emitting
/// zero-dispatch events until shutdown.
#[tokio::test]
async fn dead_modules_excluded_from_subscription_lists() {
    let Some(wasm) = module_wasm_or_skip("price-alert") else {
        return;
    };

    let dir = tempfile::tempdir().unwrap();
    let manifest = dir.path().join("module.toml");
    // Manifest declares both a block and a chain-log subscription so the
    // test genuinely exercises both filter paths — not just the trivially
    // empty chain_log case of a block-only module.
    std::fs::write(
        &manifest,
        r#"
[module]
name = "price-alert"

[capabilities]
required = ["logging", "chain"]

[[subscription]]
kind     = "block"
chain_id = 11155111

[[subscription]]
kind             = "chain-log"
chain_id         = 11155111
address          = "0xbA3cB449bD2B4ADddBc894D8697F5170800EAdeC"
event_signature  = "0xcf5f9de2984132265203b5c335b25727702ca77262ff622e136baa7362bf1da9"

[config]
oracle_address = "0x694AA1769357215DE4FAC081bf1f309aDC325306"
decimals       = "8"
threshold      = "not-a-number"
direction      = "below"
every_n_blocks = "1"
"#,
    )
    .unwrap();

    let engine = make_wasmtime_engine();
    let linker = make_linker(&engine);
    let (_dir, store) = temp_local_store();
    let supervisor = boot_production_module(&engine, &linker, &store, &wasm, &manifest).await;

    assert_eq!(supervisor.alive_count(), 0, "init-failed module is dead");
    assert!(
        supervisor.block_chains().is_empty(),
        "dead module must not contribute to block_chains()",
    );
    assert!(
        supervisor.chain_log_subscriptions().is_empty(),
        "dead module must not contribute to chain_log_subscriptions()",
    );
    assert!(
        supervisor.dead_modules_hold_subscriptions(),
        "the filtered-out subscriptions must be attributed to the dead module",
    );
}

/// Positive control for the alive filter: with one dead and one alive
/// module, the alive module's subscriptions must survive the filter.
/// Guards against a regression where the filter (or a manifest-schema
/// change) empties the lists for everyone, which the all-dead test
/// above cannot distinguish from correct filtering.
#[tokio::test]
async fn alive_module_subscriptions_survive_alongside_dead_module() {
    let Some(price_alert_wasm) = module_wasm_or_skip("price-alert") else {
        return;
    };
    let Some(example_wasm) = example_wasm_or_skip() else {
        return;
    };

    let engine = make_wasmtime_engine();
    let linker = make_linker(&engine);
    let (_dir, local_store) = temp_local_store();
    let components = test_components(local_store);

    let tmp = tempfile::tempdir().unwrap();
    // price-alert with an unparseable threshold: loads, then init fails.
    let dead_manifest = tmp.path().join("price-alert.toml");
    std::fs::write(
        &dead_manifest,
        r#"
[module]
name = "price-alert"

[capabilities]
required = ["logging", "chain"]

[[subscription]]
kind     = "block"
chain_id = 11155111

[config]
oracle_address = "0x694AA1769357215DE4FAC081bf1f309aDC325306"
decimals       = "8"
threshold      = "not-a-number"
direction      = "below"
every_n_blocks = "1"
"#,
    )
    .unwrap();
    // example module inits fine and subscribes to chain 1 blocks.
    let alive_manifest = tmp.path().join("example.toml");
    std::fs::write(
        &alive_manifest,
        r#"
[module]
name = "example"

[capabilities]
required = ["logging"]

[[subscription]]
kind     = "block"
chain_id = 1
"#,
    )
    .unwrap();

    let engine_cfg = crate::engine_config::EngineConfig {
        engine: crate::engine_config::EngineSection {
            state_dir: tmp.path().to_path_buf(),
            log_level: "info".into(),
            metrics: crate::engine_config::MetricsSection::default(),
            ..Default::default()
        },
        limits: crate::engine_config::ModuleLimits::default(),
        chains: std::collections::HashMap::new(),
        extensions: std::collections::HashMap::new(),
        modules: vec![
            crate::engine_config::ModuleEntry {
                path: price_alert_wasm,
                manifest: Some(dead_manifest),
            },
            crate::engine_config::ModuleEntry {
                path: example_wasm,
                manifest: Some(alive_manifest),
            },
        ],
        adapters: Vec::new(),
    };

    let supervisor = Supervisor::boot(
        &engine,
        &linker,
        &engine_cfg,
        &components,
        &core_extensions(),
        None,
    )
    .await
    .expect("boot");

    assert_eq!(supervisor.module_count(), 2);
    assert_eq!(supervisor.alive_count(), 1, "only the example is alive");
    let chains = supervisor.block_chains();
    assert_eq!(
        chains.iter().map(|c| c.id()).collect::<Vec<_>>(),
        vec![1],
        "the alive module's chain survives; the dead module's does not",
    );
    assert!(
        supervisor.dead_modules_hold_subscriptions(),
        "the dead module's dropped subscription is attributable",
    );
}

// `with_dispatch_deadline` bounds a dispatch in wall-clock, covering
// host-call time fuel cannot meter.

/// `with_dispatch_deadline` cancels rather than awaits an over-long future:
/// a sleep far past the deadline is dropped, not run. The end-to-end case is
/// `dispatch_deadline_cuts_off_a_blocked_host_call_and_recovers`.
#[tokio::test]
async fn dispatch_deadline_interrupts_a_sleeping_host_call() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let ran_to_completion = Arc::new(AtomicBool::new(false));
    let flag = ran_to_completion.clone();
    // Models a guest whose host call parks for an hour (a hung RPC / a
    // server that never answers). Without the deadline this future would
    // hold the dispatch for the full hour.
    let dispatch = async move {
        tokio::time::sleep(Duration::from_secs(3600)).await;
        flag.store(true, Ordering::SeqCst);
    };

    let result = with_dispatch_deadline(Duration::from_millis(50), dispatch).await;

    assert!(
        result.is_err(),
        "a host call sleeping 1h must be cut off by the 50ms deadline",
    );
    assert!(
        !ran_to_completion.load(Ordering::SeqCst),
        "the sleeping future must be cancelled, not left to run unbounded",
    );
}

/// The deadline does not punish a dispatch that finishes promptly: the
/// inner future's value is returned untouched.
#[tokio::test]
async fn dispatch_deadline_lets_a_prompt_call_finish() {
    let result = with_dispatch_deadline(Duration::from_secs(30), async { 7_u8 }).await;
    assert_eq!(result.expect("prompt call is well under the deadline"), 7);
}

/// The resolved deadline honours an override, falls back to the default
/// when unset, and saturates a degenerate `0` up to the 1s floor so it
/// cannot cut every dispatch off instantly.
#[test]
fn event_deadline_resolves_override_default_and_floor() {
    let default = ModuleLimits::default();
    assert_eq!(
        default.event_deadline(),
        Duration::from_secs(120),
        "unset resolves to the built-in default",
    );

    let overridden = ModuleLimits {
        event_deadline_secs: Some(5),
        ..ModuleLimits::default()
    };
    assert_eq!(overridden.event_deadline(), Duration::from_secs(5));

    let degenerate = ModuleLimits {
        event_deadline_secs: Some(0),
        ..ModuleLimits::default()
    };
    assert_eq!(
        degenerate.event_deadline(),
        Duration::from_secs(1),
        "a zero override saturates up to the 1s floor",
    );
}

/// A guest suspended inside a host call is cut off by the wall-clock
/// deadline, the poisoned store torn down and the module marked dead, then a
/// later dispatch reinstantiates it on a fresh store. The `slow-host` fixture
/// parks its first `chain::request` an hour past a 1s deadline override; the
/// park is one-shot, so the module recovers after the restart backoff.
#[tokio::test]
async fn dispatch_deadline_cuts_off_a_blocked_host_call_and_recovers() {
    use std::time::Instant;

    let Some(wasm) = module_wasm_or_skip("slow-host") else {
        return;
    };

    let engine = make_wasmtime_engine();
    let linker = crate::supervisor::build_linker::<crate::test_utils::MockTypes>(&engine, &[])
        .expect("build_linker");

    // Program the chain backend: the first request parks for an hour (a
    // hung node), every request answers `eth_blockNumber` once it runs.
    // The park is consumed when the first request begins, so the request
    // dropped at the deadline leaves the next one prompt.
    let chain = crate::test_utils::MockChainProvider::new();
    chain.on_method(
        crate::host::component::ChainMethod::EthBlockNumber,
        "\"0x1\"",
    );
    chain.delay_next_request(Duration::from_secs(3600));
    let components =
        crate::test_utils::mock_components_from(chain, crate::test_utils::MockStateStore::new());

    let manifest = fixture_module_toml("modules/fixtures/slow-host/module.toml");
    // 1s is the floor the resolver saturates up to; short enough to keep
    // the test quick, long enough to prove the call was cut off (the park
    // is an hour) rather than never started.
    let limits = ModuleLimits {
        event_deadline_secs: Some(1),
        ..ModuleLimits::default()
    };

    let mut supervisor = Supervisor::<crate::test_utils::MockTypes>::boot_single(
        &engine,
        &linker,
        &wasm,
        Some(&manifest),
        &components,
        &limits,
        &[],
        None,
    )
    .await
    .expect("boot_single");
    assert_eq!(supervisor.alive_count(), 1, "slow-host loads alive");

    let block = nexum::host::types::Block {
        chain_id: 1,
        number: 1,
        hash: vec![0; 32],
        timestamp: 1_700_000_000_000,
    };

    // First dispatch: the guest suspends inside the parked host call and
    // the 1s deadline cuts it off. It resolves in ~deadline wall-time, not
    // the hour the mock would otherwise park for.
    let started = Instant::now();
    let dispatched = supervisor.dispatch_block(block.clone()).await;
    let elapsed = started.elapsed();
    assert_eq!(dispatched, 0, "the deadline cut the blocked host call off");
    assert!(
        elapsed < Duration::from_secs(30),
        "cut off in ~deadline wall-time ({elapsed:?}), not the 1h park",
    );
    assert_eq!(
        supervisor.alive_count(),
        0,
        "the module is marked dead after the deadline, like a trap",
    );

    // Wait out the 1s restart backoff, then dispatch again. Phase 1 of the
    // dispatch reinstantiates the dead module on a fresh store (proving the
    // store poisoned by the dropped fiber was correctly torn down and
    // rebuilt); the guest's next request is prompt, so it dispatches Ok.
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    let dispatched_again = supervisor.dispatch_block(block).await;
    assert_eq!(
        dispatched_again, 1,
        "after backoff the module restarts on a fresh store and dispatches",
    );
    assert_eq!(
        supervisor.alive_count(),
        1,
        "the recovered module is alive again",
    );
}

// ── Resource-limit enforcement tests ───────────────────────
//
// Two evil-by-design fixtures under `modules/fixtures/` exercise the
// per-module fuel + memory caps (DEFAULT_FUEL_PER_EVENT
// + DEFAULT_MEMORY_LIMIT). The tests assert:
//
// 1. The host catches the trap (OutOfFuel / memory-grow rejection)
//    without panicking the supervisor.
// 2. The trapping module is marked dead (alive_count drops to 0 for a
//    single-module supervisor).
// 3. A subsequent dispatch does not re-enter the dead module + the
//    engine itself remains alive (dispatched count is 0, no crash).
//
// Locks the M1 fuel/memory wiring against regression so future
// changes to the supervisor cannot silently bypass the limits.

fn fixture_module_toml(relative_path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(relative_path)
}

/// Boot a single fixture (.wasm + module.toml) under the supervisor.
/// Shared body across the two resource-limit tests.
async fn boot_fixture(wasm: &Path, manifest_relative: &str) -> DefaultSupervisor {
    let engine = make_wasmtime_engine();
    let linker = make_linker(&engine);
    let (_dir, local_store) = temp_local_store();
    let components = test_components(local_store);
    let manifest = fixture_module_toml(manifest_relative);
    let limits = crate::engine_config::ModuleLimits::default();
    Supervisor::boot_single(
        &engine,
        &linker,
        wasm,
        Some(&manifest),
        &components,
        &limits,
        &core_extensions(),
        None,
    )
    .await
    .expect("boot_single")
}

#[tokio::test]
async fn resource_limit_fuel_bomb_traps_and_marks_module_dead() {
    let Some(wasm) = module_wasm_or_skip("fuel-bomb") else {
        return;
    };
    let mut supervisor = boot_fixture(&wasm, "modules/fixtures/fuel-bomb/module.toml").await;
    assert_eq!(supervisor.module_count(), 1);
    assert_eq!(supervisor.alive_count(), 1, "loads alive");

    // First dispatch enters the fuel-bomb's unbounded loop. wasmtime
    // burns through the per-event fuel budget; the call returns Err
    // (a trap), the supervisor catches it and marks the module dead.
    let block = nexum::host::types::Block {
        chain_id: 1,
        number: 1,
        hash: vec![0; 32],
        timestamp: 1_700_000_000_000,
    };
    let dispatched = supervisor.dispatch_block(block.clone()).await;
    assert_eq!(
        dispatched, 0,
        "fuel-bomb trapped, no module accepted the dispatch",
    );
    assert_eq!(
        supervisor.alive_count(),
        0,
        "fuel-bomb is marked dead after the trap",
    );

    // Engine is still healthy for further dispatches.
    let dispatched_again = supervisor.dispatch_block(block).await;
    assert_eq!(
        dispatched_again, 0,
        "dead module excluded from second dispatch",
    );
}

#[tokio::test]
async fn resource_limit_dead_bomb_does_not_starve_healthy_module() {
    // Strongest assertion of the isolation invariant: load fuel-bomb
    // + the M1 example module side-by-side. After the bomb traps,
    // dispatch a second block and confirm the example module still
    // receives it (dispatched == 1, alive_count == 1 because only
    // one of the two is alive).
    let Some(bomb_wasm) = module_wasm_or_skip("fuel-bomb") else {
        return;
    };
    let Some(example_wasm) = example_wasm_or_skip() else {
        return;
    };

    let engine = make_wasmtime_engine();
    let linker = make_linker(&engine);
    let (_dir, local_store) = temp_local_store();
    let components = test_components(local_store);

    // Hand-build an EngineConfig with both modules subscribed to
    // chain 1 blocks. fuel-bomb's manifest already declares the
    // block subscription; the example module needs a synthesised
    // manifest because its on-disk manifest does not subscribe to
    // blocks by default.
    let tmp = tempfile::tempdir().unwrap();
    let example_manifest = tmp.path().join("example.toml");
    std::fs::write(
        &example_manifest,
        r#"
[module]
name = "example"

[capabilities]
required = ["logging"]

[[subscription]]
kind     = "block"
chain_id = 1
"#,
    )
    .unwrap();

    let engine_cfg = crate::engine_config::EngineConfig {
        engine: crate::engine_config::EngineSection {
            state_dir: tmp.path().to_path_buf(),
            log_level: "info".into(),
            metrics: crate::engine_config::MetricsSection::default(),
            ..Default::default()
        },
        limits: crate::engine_config::ModuleLimits::default(),
        chains: std::collections::HashMap::new(),
        extensions: std::collections::HashMap::new(),
        modules: vec![
            crate::engine_config::ModuleEntry {
                path: bomb_wasm.clone(),
                manifest: Some(fixture_module_toml(
                    "modules/fixtures/fuel-bomb/module.toml",
                )),
            },
            crate::engine_config::ModuleEntry {
                path: example_wasm.clone(),
                manifest: Some(example_manifest.clone()),
            },
        ],
        adapters: Vec::new(),
    };

    let mut supervisor = Supervisor::boot(
        &engine,
        &linker,
        &engine_cfg,
        &components,
        &core_extensions(),
        None,
    )
    .await
    .expect("boot");

    assert_eq!(supervisor.module_count(), 2);
    assert_eq!(supervisor.alive_count(), 2, "both load alive");

    // First dispatch: fuel-bomb burns through its budget + traps.
    // The example module dispatches normally on the same block. The
    // bomb is now dead.
    let block = nexum::host::types::Block {
        chain_id: 1,
        number: 1,
        hash: vec![0; 32],
        timestamp: 1_700_000_000_000,
    };
    let dispatched = supervisor.dispatch_block(block.clone()).await;
    assert_eq!(
        dispatched, 1,
        "example module received the dispatch even though fuel-bomb trapped",
    );
    assert_eq!(supervisor.alive_count(), 1, "only the example is alive");

    // Second dispatch: only the example accepts; the dead bomb is
    // skipped by the dispatch fast-path.
    let dispatched_again = supervisor.dispatch_block(block).await;
    assert_eq!(dispatched_again, 1);
    assert_eq!(supervisor.alive_count(), 1);
}

#[tokio::test]
async fn resource_limit_memory_bomb_traps_and_marks_module_dead() {
    let Some(wasm) = module_wasm_or_skip("memory-bomb") else {
        return;
    };
    let mut supervisor = boot_fixture(&wasm, "modules/fixtures/memory-bomb/module.toml").await;
    assert_eq!(supervisor.module_count(), 1);
    assert_eq!(supervisor.alive_count(), 1);

    // memory-bomb's on_event allocates 128 MiB which exceeds the
    // 64 MiB DEFAULT_MEMORY_LIMIT; wasmtime rejects the memory.grow
    // and propagates a trap.
    let block = nexum::host::types::Block {
        chain_id: 1,
        number: 1,
        hash: vec![0; 32],
        timestamp: 1_700_000_000_000,
    };
    let dispatched = supervisor.dispatch_block(block.clone()).await;
    assert_eq!(dispatched, 0);
    assert_eq!(supervisor.alive_count(), 0);

    let dispatched_again = supervisor.dispatch_block(block).await;
    assert_eq!(dispatched_again, 0);
}

// ── Supervisor auto-restart with exponential backoff ───────
//
// flaky-bomb traps on the first N events (via wasm `unreachable!`)
// and recovers on event N+1. Exercises the full restart lifecycle:
//
// 1. Dispatch 1: trap -> alive=false, failure_count=1, next_attempt=+1s.
// 2. Immediate redispatch: skipped (next_attempt in the future).
// 3. After 1.1s: alive flipped back on, dispatch retried.
// 4. With fail_first_n=1, the second attempt succeeds -> failure_count
//    resets to 0, next_attempt = None.
//
// Asserts the schedule shape end-to-end with real wall-clock.

#[tokio::test]
async fn restart_flaky_module_recovers_after_backoff() {
    let Some(wasm) = module_wasm_or_skip("flaky-bomb") else {
        return;
    };

    let dir = tempfile::tempdir().unwrap();
    let manifest = dir.path().join("module.toml");
    // fail_first_n = 1 so the module traps once and recovers on the
    // second dispatch attempt. Keeps the test wall-clock under 2 s.
    std::fs::write(
        &manifest,
        r#"
[module]
name = "flaky-bomb"

[capabilities]
required = ["logging", "local-store"]

[[subscription]]
kind     = "block"
chain_id = 1

[config]
fail_first_n = "1"
"#,
    )
    .unwrap();

    let engine = make_wasmtime_engine();
    let linker = make_linker(&engine);
    let (_dir, store) = temp_local_store();
    let components = test_components(store);
    let limits = crate::engine_config::ModuleLimits::default();
    let mut supervisor = Supervisor::boot_single(
        &engine,
        &linker,
        &wasm,
        Some(&manifest),
        &components,
        &limits,
        &core_extensions(),
        None,
    )
    .await
    .expect("boot_single");
    assert_eq!(supervisor.alive_count(), 1);

    let block = nexum::host::types::Block {
        chain_id: 1,
        number: 1,
        hash: vec![0; 32],
        timestamp: 1_700_000_000_000,
    };

    // Dispatch 1: trap. Module marked dead with a +1s backoff.
    let dispatched = supervisor.dispatch_block(block.clone()).await;
    assert_eq!(dispatched, 0, "first dispatch trapped, no module accepted");
    assert_eq!(supervisor.alive_count(), 0, "module marked dead");

    // Immediate redispatch (under the 1s backoff): still skipped.
    let dispatched_immediate = supervisor.dispatch_block(block.clone()).await;
    assert_eq!(
        dispatched_immediate, 0,
        "in-backoff module not eligible for redispatch yet",
    );
    assert_eq!(supervisor.alive_count(), 0);

    // Wait for the 1s backoff window to elapse (+ a small fudge for
    // scheduler jitter).
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

    // Dispatch 3: now eligible. fail_first_n=1 was satisfied on
    // dispatch 1, so this attempt succeeds. The supervisor flips
    // alive back on, dispatch lands, failure_count resets.
    let dispatched_after_backoff = supervisor.dispatch_block(block.clone()).await;
    assert_eq!(
        dispatched_after_backoff, 1,
        "module recovered after the backoff window",
    );
    assert_eq!(supervisor.alive_count(), 1, "recovered + alive");

    // Dispatch 4: steady-state, no backoff in play. Module is happy.
    let dispatched_steady = supervisor.dispatch_block(block).await;
    assert_eq!(dispatched_steady, 1);
}

// ── Poison-pill quarantine ──────────────────────────────────
//
// fuel-bomb traps on every dispatch. With a
// tight poison policy (3 failures / 60 s) we can observe the
// supervisor escalate from "retry" to "permanent quarantine" inside
// ~4 s of wall clock:
//
//   trap 1: failure_count=1, next_attempt=+1s
//   sleep 1.1s
//   trap 2: failure_count=2, next_attempt=+2s
//   sleep 2.1s
//   trap 3: failure_count=3 -> POISONED. Recent failures hit the
//           window threshold; the supervisor stops attempting
//           restarts entirely. Subsequent dispatches skip the
//           module silently.
//
// Tests assert each transition + the post-quarantine no-op semantic.

#[tokio::test]
async fn poison_pill_quarantines_module_after_threshold() {
    let Some(wasm) = module_wasm_or_skip("fuel-bomb") else {
        return;
    };
    let manifest = production_module_toml("modules/fixtures/fuel-bomb/module.toml");
    let engine = make_wasmtime_engine();
    let linker = make_linker(&engine);
    let (_dir, store) = temp_local_store();
    let components = test_components(store);

    // Tight policy: 3 failures in 60 s -> quarantine. Keeps the
    // test wall-clock under 4 s. Set through `[limits.poison]`.
    let limits = crate::engine_config::ModuleLimits {
        poison: crate::engine_config::PoisonLimitsSection {
            max_failures: Some(3),
            window_secs: Some(60),
        },
        ..Default::default()
    };
    let mut supervisor = Supervisor::boot_single(
        &engine,
        &linker,
        &wasm,
        Some(&manifest),
        &components,
        &limits,
        &core_extensions(),
        None,
    )
    .await
    .expect("boot_single");

    assert_eq!(supervisor.module_count(), 1);
    assert_eq!(supervisor.alive_count(), 1);
    assert_eq!(supervisor.poisoned_count(), 0);

    let block = nexum::host::types::Block {
        chain_id: 1,
        number: 1,
        hash: vec![0; 32],
        timestamp: 1_700_000_000_000,
    };

    // Trap 1.
    let dispatched = supervisor.dispatch_block(block.clone()).await;
    assert_eq!(dispatched, 0);
    assert_eq!(supervisor.alive_count(), 0);
    assert_eq!(supervisor.poisoned_count(), 0, "1 trap < threshold");
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;

    // Trap 2.
    let dispatched = supervisor.dispatch_block(block.clone()).await;
    assert_eq!(dispatched, 0);
    assert_eq!(supervisor.poisoned_count(), 0, "2 traps < threshold");
    tokio::time::sleep(std::time::Duration::from_millis(2_100)).await;

    // Trap 3 -> POISONED.
    let dispatched = supervisor.dispatch_block(block.clone()).await;
    assert_eq!(dispatched, 0);
    assert_eq!(
        supervisor.poisoned_count(),
        1,
        "3 traps inside window -> module quarantined",
    );

    // Post-quarantine: immediately re-dispatch. A poisoned module
    // is excluded regardless of how much time has passed; the
    // backoff timer is no longer load-bearing. We do NOT wait for
    // the would-be next_attempt because the test just needs to
    // observe the "skipped silently" semantic, not the timing.
    let dispatched = supervisor.dispatch_block(block).await;
    assert_eq!(
        dispatched, 0,
        "poisoned module excluded from dispatch forever",
    );
    assert_eq!(supervisor.poisoned_count(), 1);
}

// ── Log pipeline ─────────────────────────────────────────────
//
// The typed pipeline captures from three points: the
// nexum:host/logging glue (HostInterface), the per-store
// stdout/stderr pipes (Stdout/Stderr), and the supervisor death
// path (Panic). These E2E tests prove a real run leaves retrievable
// records and that a dying run leaves a Panic record, both read back
// through the embedder-facing LogPipeline handle. Stdout/Stderr line
// splitting is covered at the unit level on the StdioStream writer.

/// Components plus a retained clone of the log pipeline so a test can
/// read runs and records back after dispatch.
fn components_with_logs(
    store: crate::host::local_store_redb::LocalStore,
) -> (Components<TestTypes>, crate::host::logs::LogPipeline) {
    let logs = crate::test_utils::in_memory_logs();
    let components = Components {
        chain: ProviderPool::empty(),
        store,
        ext: (),
        logs: logs.clone(),
    };
    (components, logs)
}

/// Ported to the [`TestRuntime`] harness: it replaces the hand-built
/// `boot_single` plus manual `dispatch_block` ceremony with an inline-manifest
/// launch, an injected header, and a polled log read, while holding the same
/// coverage. The example module logs via the host logging glue at init and on
/// the block, so its run holds retrievable HostInterface records after one
/// dispatch.
#[tokio::test]
async fn host_interface_records_are_retrievable_after_a_run() {
    let Some(wasm) = example_wasm_or_skip() else {
        return;
    };

    let mut rt = crate::test_utils::TestRuntime::builder(wasm)
        .manifest_inline(
            r#"
[module]
name = "example"

[capabilities]
required = ["logging"]

[[subscription]]
kind     = "block"
chain_id = 1
"#,
        )
        .launch()
        .await
        .expect("launch example over the harness");

    let mut header: alloy_rpc_types_eth::Header = alloy_rpc_types_eth::Header::default();
    header.inner.number = 19_000_000;
    rt.push_block(header);

    // The polled log read doubles as the dispatch barrier: the on_event line
    // only lands once the event loop has dispatched the injected block.
    rt.wait_for_log("example", "block 19000000")
        .await
        .expect("the on_event log line lands after dispatch");

    let runs = rt.logs().list_runs("example");
    assert_eq!(runs.len(), 1, "one run recorded for the example module");
    let run = runs[0].run.clone();
    assert_eq!(run.seq, 0, "the first run is sequence 0");
    let page = rt.logs().read(&run, 0);
    assert!(!page.records.is_empty(), "run left retrievable records");
    assert!(
        page.records
            .iter()
            .all(|r| r.source == LogSource::HostInterface),
        "the example module logs only through the host interface",
    );
    assert!(
        page.records
            .iter()
            .any(|r| r.message.contains("block 19000000")),
        "the on_event log line is retained",
    );

    rt.shutdown();
    rt.wait().await.expect("clean shutdown");
}

#[tokio::test]
async fn dying_run_leaves_a_panic_record() {
    let Some(wasm) = module_wasm_or_skip("fuel-bomb") else {
        return;
    };
    let engine = make_wasmtime_engine();
    let linker = make_linker(&engine);
    let (_dir, store) = temp_local_store();
    let (components, logs) = components_with_logs(store);
    let manifest = fixture_module_toml("modules/fixtures/fuel-bomb/module.toml");
    let limits = ModuleLimits::default();
    let mut supervisor = Supervisor::boot_single(
        &engine,
        &linker,
        &wasm,
        Some(&manifest),
        &components,
        &limits,
        &core_extensions(),
        None,
    )
    .await
    .expect("boot_single");

    let block = nexum::host::types::Block {
        chain_id: 1,
        number: 1,
        hash: vec![0; 32],
        timestamp: 1_700_000_000_000,
    };
    // fuel-bomb traps on the first event; the supervisor synthesizes a
    // Panic record on the dead run.
    assert_eq!(
        supervisor.dispatch_block(block).await,
        0,
        "the bomb trapped"
    );

    let runs = logs.list_runs("fuel-bomb");
    assert_eq!(runs.len(), 1);
    let page = logs.read(&runs[0].run, 0);
    let panic = page
        .records
        .iter()
        .find(|r| r.source == LogSource::Panic)
        .expect("a panic record on the dead run");
    assert_eq!(panic.level, Level::ERROR);
    assert!(panic.message.contains("terminated"));
    assert_eq!(
        panic.message.lines().count(),
        1,
        "the panic record carries the trap's root cause, not the frame list",
    );
}

#[tokio::test]
async fn facade_panic_leaves_stderr_host_interface_and_panic_records() {
    let Some(wasm) = module_wasm_or_skip("panic-bomb") else {
        return;
    };
    let engine = make_wasmtime_engine();
    let linker = make_linker(&engine);
    let (_dir, store) = temp_local_store();
    let (components, logs) = components_with_logs(store);
    let manifest = fixture_module_toml("modules/fixtures/panic-bomb/module.toml");
    let limits = ModuleLimits::default();
    let mut supervisor = Supervisor::boot_single(
        &engine,
        &linker,
        &wasm,
        Some(&manifest),
        &components,
        &limits,
        &core_extensions(),
        None,
    )
    .await
    .expect("boot_single");

    let block = nexum::host::types::Block {
        chain_id: 1,
        number: 1,
        hash: vec![0; 32],
        timestamp: 1_700_000_000_000,
    };
    assert_eq!(
        supervisor.dispatch_block(block).await,
        0,
        "the bomb panicked"
    );

    // The facade panic hook writes to stderr and reports over the host
    // logging call before the trap surfaces, and the supervisor
    // synthesizes the death record: one dead run, three capture points.
    let runs = logs.list_runs("panic-bomb");
    assert_eq!(runs.len(), 1);
    let page = logs.read(&runs[0].run, 0);
    let find = |source: LogSource, needle: &str| {
        page.records
            .iter()
            .find(|r| r.source == source && r.message.contains(needle))
    };
    let stderr = find(LogSource::Stderr, "detonated").expect("the hook's stderr line was captured");
    assert_eq!(stderr.level, Level::WARN, "stderr copy is warn");
    let host =
        find(LogSource::HostInterface, "detonated").expect("the hook's sink call was captured");
    assert_eq!(host.level, Level::ERROR, "sink copy is error");
    let death =
        find(LogSource::Panic, "terminated").expect("the supervisor synthesized the death record");
    assert_eq!(death.level, Level::ERROR, "death record is error");
}

// ── Multi-chain isolation ───────────────────────────────────
//
// The supervisor's dispatch path is per-chain: `dispatch_block(block)`
// walks every module but only invokes those whose
// `[[subscription]] kind = "block"` matches `block.chain_id`. A
// module on chain A receives nothing when a chain-B block arrives,
// and vice versa. Combined with the per-module restart / poison
// state, this gives the engine multi-chain isolation by
// construction: a poisoned module on one chain cannot starve
// modules on any other chain.
//
// The WS reconnect tasks add the upstream symmetry: each
// chain owns its own subscription task + backoff timer, so a chain-A
// WS drop never blocks chain-B events.

#[tokio::test]
async fn multi_chain_dispatch_isolates_modules_by_chain() {
    // Two example modules on two different chains. Confirm dispatch
    // on chain A reaches only the chain-A module and vice versa.
    let Some(wasm) = example_wasm_or_skip() else {
        return;
    };

    let dir = tempfile::tempdir().unwrap();
    let chain_a_manifest = dir.path().join("a.toml");
    let chain_b_manifest = dir.path().join("b.toml");
    std::fs::write(
        &chain_a_manifest,
        r#"
[module]
name = "module-a"

[capabilities]
required = ["logging"]

[[subscription]]
kind     = "block"
chain_id = 1
"#,
    )
    .unwrap();
    std::fs::write(
        &chain_b_manifest,
        r#"
[module]
name = "module-b"

[capabilities]
required = ["logging"]

[[subscription]]
kind     = "block"
chain_id = 100
"#,
    )
    .unwrap();

    let engine = make_wasmtime_engine();
    let linker = make_linker(&engine);
    let (_dir, local_store) = temp_local_store();
    let components = test_components(local_store);

    let engine_cfg = crate::engine_config::EngineConfig {
        engine: crate::engine_config::EngineSection {
            state_dir: dir.path().to_path_buf(),
            log_level: "info".into(),
            metrics: crate::engine_config::MetricsSection::default(),
            ..Default::default()
        },
        limits: crate::engine_config::ModuleLimits::default(),
        chains: std::collections::HashMap::new(),
        extensions: std::collections::HashMap::new(),
        modules: vec![
            crate::engine_config::ModuleEntry {
                path: wasm.clone(),
                manifest: Some(chain_a_manifest),
            },
            crate::engine_config::ModuleEntry {
                path: wasm,
                manifest: Some(chain_b_manifest),
            },
        ],
        adapters: Vec::new(),
    };

    let mut supervisor = Supervisor::boot(
        &engine,
        &linker,
        &engine_cfg,
        &components,
        &core_extensions(),
        None,
    )
    .await
    .expect("boot");
    assert_eq!(supervisor.module_count(), 2);
    assert_eq!(supervisor.alive_count(), 2);

    let block_a = nexum::host::types::Block {
        chain_id: 1,
        number: 1,
        hash: vec![0; 32],
        timestamp: 1_700_000_000_000,
    };
    let block_b = nexum::host::types::Block {
        chain_id: 100,
        number: 1,
        hash: vec![0; 32],
        timestamp: 1_700_000_000_000,
    };

    // Chain A block reaches only module-a.
    let dispatched = supervisor.dispatch_block(block_a).await;
    assert_eq!(dispatched, 1, "only module-a subscribed to chain 1");
    assert_eq!(supervisor.alive_count(), 2);

    // Chain B block reaches only module-b.
    let dispatched = supervisor.dispatch_block(block_b).await;
    assert_eq!(dispatched, 1, "only module-b subscribed to chain 100");
    assert_eq!(supervisor.alive_count(), 2);
}

/// Acceptance criterion for the per-handler dispatch rate limit: a
/// source flooding one module is throttled at the dispatch boundary
/// (over-rate events dropped) while a second module on another chain
/// still gets every dispatch. Two healthy example modules; a tiny
/// `[limits.dispatch]` (burst = 2, refill = 1/s) so the flood drains
/// the first module's bucket almost immediately.
#[tokio::test]
async fn dispatch_rate_limit_throttles_a_flood_without_starving_others() {
    let Some(wasm) = example_wasm_or_skip() else {
        return;
    };

    let dir = tempfile::tempdir().unwrap();
    let flood_manifest = dir.path().join("flood.toml");
    let calm_manifest = dir.path().join("calm.toml");
    std::fs::write(
        &flood_manifest,
        r#"
[module]
name = "flood"

[capabilities]
required = ["logging"]

[[subscription]]
kind     = "block"
chain_id = 1
"#,
    )
    .unwrap();
    std::fs::write(
        &calm_manifest,
        r#"
[module]
name = "calm"

[capabilities]
required = ["logging"]

[[subscription]]
kind     = "block"
chain_id = 100
"#,
    )
    .unwrap();

    let engine = make_wasmtime_engine();
    let linker = make_linker(&engine);
    let (_dir, local_store) = temp_local_store();
    let components = test_components(local_store);

    let engine_cfg = crate::engine_config::EngineConfig {
        engine: crate::engine_config::EngineSection {
            state_dir: dir.path().to_path_buf(),
            log_level: "info".into(),
            metrics: crate::engine_config::MetricsSection::default(),
            ..Default::default()
        },
        limits: crate::engine_config::ModuleLimits {
            dispatch: crate::engine_config::DispatchLimitsSection {
                burst: Some(2),
                refill_per_sec: Some(1),
            },
            ..Default::default()
        },
        chains: std::collections::HashMap::new(),
        extensions: std::collections::HashMap::new(),
        modules: vec![
            crate::engine_config::ModuleEntry {
                path: wasm.clone(),
                manifest: Some(flood_manifest),
            },
            crate::engine_config::ModuleEntry {
                path: wasm,
                manifest: Some(calm_manifest),
            },
        ],
        adapters: Vec::new(),
    };

    let mut supervisor = Supervisor::boot(
        &engine,
        &linker,
        &engine_cfg,
        &components,
        &core_extensions(),
        None,
    )
    .await
    .expect("boot");
    assert_eq!(supervisor.alive_count(), 2);

    // Flood chain 1 with far more blocks than the burst allowance. The
    // loop runs in well under a second, so refill (1 token/s) adds at
    // most one or two tokens: the flood module is dispatched only a
    // handful of times and the rest are dropped.
    const FLOOD: u64 = 20;
    let mut flood_dispatched = 0;
    for number in 0..FLOOD {
        flood_dispatched += supervisor
            .dispatch_block(nexum::host::types::Block {
                chain_id: 1,
                number,
                hash: vec![0; 32],
                timestamp: 1_700_000_000_000,
            })
            .await;
    }
    assert!(
        flood_dispatched >= 2,
        "the burst allowance ({flood_dispatched}) must clear before throttling",
    );
    assert!(
        flood_dispatched < FLOOD as usize,
        "the flood must be throttled: {flood_dispatched} of {FLOOD} got through",
    );

    // The calm module on chain 100 has its own untouched bucket, so a
    // block on its chain still dispatches even though the flood module
    // is being throttled. This is the per-module fairness guarantee.
    let calm_dispatched = supervisor
        .dispatch_block(nexum::host::types::Block {
            chain_id: 100,
            number: 1,
            hash: vec![0; 32],
            timestamp: 1_700_000_000_000,
        })
        .await;
    assert_eq!(
        calm_dispatched, 1,
        "the calm module is served in full - a flood on another module never starves it",
    );

    // Neither module died: rate limiting is a benign drop, not a fault.
    assert_eq!(
        supervisor.alive_count(),
        2,
        "rate limiting must not kill modules"
    );
    assert_eq!(supervisor.poisoned_count(), 0);
}

#[tokio::test]
async fn multi_chain_poisoned_module_does_not_affect_other_chains() {
    // fuel-bomb (always-traps) on chain 1, example (healthy) on
    // chain 100. Trap the bomb a few times with a tight poison
    // policy so it gets quarantined; verify the example keeps
    // dispatching on chain 100 throughout.
    let Some(bomb_wasm) = module_wasm_or_skip("fuel-bomb") else {
        return;
    };
    let Some(example_wasm) = example_wasm_or_skip() else {
        return;
    };

    let dir = tempfile::tempdir().unwrap();
    let example_manifest = dir.path().join("example.toml");
    std::fs::write(
        &example_manifest,
        r#"
[module]
name = "example"

[capabilities]
required = ["logging"]

[[subscription]]
kind     = "block"
chain_id = 100
"#,
    )
    .unwrap();

    let engine = make_wasmtime_engine();
    let linker = make_linker(&engine);
    let (_dir, local_store) = temp_local_store();
    let components = test_components(local_store);

    let engine_cfg = crate::engine_config::EngineConfig {
        engine: crate::engine_config::EngineSection {
            state_dir: dir.path().to_path_buf(),
            log_level: "info".into(),
            metrics: crate::engine_config::MetricsSection::default(),
            ..Default::default()
        },
        // Tight policy: 2 failures in 60 s -> quarantine, set through
        // `[limits.poison]`.
        limits: crate::engine_config::ModuleLimits {
            poison: crate::engine_config::PoisonLimitsSection {
                max_failures: Some(2),
                window_secs: Some(60),
            },
            ..Default::default()
        },
        chains: std::collections::HashMap::new(),
        extensions: std::collections::HashMap::new(),
        modules: vec![
            crate::engine_config::ModuleEntry {
                path: bomb_wasm,
                manifest: Some(fixture_module_toml(
                    "modules/fixtures/fuel-bomb/module.toml",
                )),
            },
            crate::engine_config::ModuleEntry {
                path: example_wasm,
                manifest: Some(example_manifest),
            },
        ],
        adapters: Vec::new(),
    };

    let mut supervisor = Supervisor::boot(
        &engine,
        &linker,
        &engine_cfg,
        &components,
        &core_extensions(),
        None,
    )
    .await
    .expect("boot");
    assert_eq!(supervisor.module_count(), 2);
    assert_eq!(supervisor.alive_count(), 2);

    let block_bomb_chain = nexum::host::types::Block {
        chain_id: 1, // fuel-bomb's manifest declares chain 1
        number: 1,
        hash: vec![0; 32],
        timestamp: 1_700_000_000_000,
    };
    let block_healthy_chain = nexum::host::types::Block {
        chain_id: 100,
        number: 1,
        hash: vec![0; 32],
        timestamp: 1_700_000_000_000,
    };

    // Trap #1 on the bomb's chain: bomb dies, example untouched.
    supervisor.dispatch_block(block_bomb_chain.clone()).await;
    assert_eq!(supervisor.poisoned_count(), 0);

    // Example keeps dispatching on its own chain - confirm before
    // the bomb hits the poison threshold.
    let dispatched_b = supervisor.dispatch_block(block_healthy_chain.clone()).await;
    assert_eq!(dispatched_b, 1, "module-b receives chain-100 blocks");

    // Wait out the bomb's backoff so trap #2 can land.
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
    supervisor.dispatch_block(block_bomb_chain).await;
    assert_eq!(
        supervisor.poisoned_count(),
        1,
        "bomb quarantined at 2 failures",
    );

    // POST-poison: bomb stays dead, example still healthy.
    let dispatched_after = supervisor.dispatch_block(block_healthy_chain).await;
    assert_eq!(
        dispatched_after, 1,
        "chain-100 module unaffected by chain-1 poison",
    );
    assert_eq!(supervisor.alive_count(), 1, "only example is alive");
    assert_eq!(supervisor.poisoned_count(), 1);
}

// ── build_alloy_filter ────────────────────────────────────────────────

#[test]
fn alloy_filter_with_address_and_topic() {
    let addr = "0xC92E8bdf79f0507f65a392b0ab4667716BFE0110";
    let topic = "0x237e158222e3e6968b72b9db0d8043aacf074ad9f650f0d1606b4d82ee432c00";
    let filter = build_alloy_filter(Some(addr), Some(topic)).unwrap();
    // Check address is set (alloy Filter doesn't expose a simple getter,
    // but we can verify the filter serialises the address field).
    let serialised = serde_json::to_value(&filter).unwrap();
    let addr_field = serialised
        .get("address")
        .unwrap()
        .to_string()
        .to_lowercase();
    assert!(addr_field.contains(&addr.to_lowercase()[2..])); // strip 0x
}

#[test]
fn alloy_filter_no_address_no_topic() {
    let filter = build_alloy_filter(None, None).unwrap();
    let serialised = serde_json::to_value(&filter).unwrap();
    // Address and topics should be absent or null.
    assert!(
        serialised.get("address").is_none()
            || serialised["address"].is_null()
            || serialised["address"] == serde_json::json!([])
    );
}

#[test]
fn alloy_filter_rejects_bad_address() {
    let err = build_alloy_filter(Some("not-an-address"), None);
    assert!(err.is_err());
}

#[test]
fn alloy_filter_rejects_bad_topic() {
    let addr = "0xC92E8bdf79f0507f65a392b0ab4667716BFE0110";
    let err = build_alloy_filter(Some(addr), Some("not-a-topic"));
    assert!(err.is_err());
}

/// A mined log carries every block-scoped field; the host projection must
/// preserve each one so the guest rebuilds the native alloy log losslessly.
#[test]
fn project_chain_log_preserves_mined_log() {
    use alloy_primitives::{Address, B256, Bytes};

    let address = Address::repeat_byte(0x11);
    let topics = vec![B256::repeat_byte(0x22), B256::repeat_byte(0x33)];
    let data = Bytes::from(vec![0xde, 0xad, 0xbe, 0xef]);
    let inner = alloy_primitives::Log::new_unchecked(address, topics.clone(), data.clone());

    let log = alloy_rpc_types_eth::Log {
        inner,
        block_hash: Some(B256::repeat_byte(0x44)),
        block_number: Some(0x1234),
        block_timestamp: Some(0x5678),
        transaction_hash: Some(B256::repeat_byte(0x55)),
        transaction_index: Some(7),
        log_index: Some(9),
        removed: true,
    };

    let projected = nexum::host::types::ChainLog::from(&log);

    assert_eq!(projected.address, address.as_slice().to_vec());
    assert_eq!(
        projected.topics,
        topics
            .iter()
            .map(|t| t.as_slice().to_vec())
            .collect::<Vec<_>>(),
    );
    assert_eq!(projected.data, data.to_vec());
    assert_eq!(
        projected.block_hash.as_deref(),
        Some(B256::repeat_byte(0x44).as_slice()),
    );
    assert_eq!(projected.block_number, Some(0x1234));
    assert_eq!(projected.block_timestamp, Some(0x5678));
    assert_eq!(
        projected.transaction_hash.as_deref(),
        Some(B256::repeat_byte(0x55).as_slice()),
    );
    assert_eq!(projected.transaction_index, Some(7));
    assert_eq!(projected.log_index, Some(9));
    assert!(projected.removed);
}

/// A pending log has no block-scoped fields; the projection must leave each
/// one `None` rather than collapsing an absent value onto a zero default.
#[test]
fn project_chain_log_leaves_pending_fields_none() {
    use alloy_primitives::{Address, Bytes};

    let inner =
        alloy_primitives::Log::new_unchecked(Address::repeat_byte(0xab), Vec::new(), Bytes::new());
    let log = alloy_rpc_types_eth::Log {
        inner,
        block_hash: None,
        block_number: None,
        block_timestamp: None,
        transaction_hash: None,
        transaction_index: None,
        log_index: None,
        removed: false,
    };

    let projected = nexum::host::types::ChainLog::from(&log);

    assert!(projected.block_hash.is_none());
    assert!(projected.block_number.is_none());
    assert!(projected.block_timestamp.is_none());
    assert!(projected.transaction_hash.is_none());
    assert!(projected.transaction_index.is_none());
    assert!(projected.log_index.is_none());
    assert!(projected.topics.is_empty());
    assert!(projected.data.is_empty());
    assert!(!projected.removed);
}

#[test]
fn chainlog_cursor_key_is_stable_and_case_insensitive() {
    // The durable key must be reproducible across restarts (unlike the
    // alloy `Filter` hash, which uses a process-randomized HashSet) and
    // must normalise hex case.
    let a = chainlog_cursor_key(Chain::from_id(1), Some("0xAbC"), Some("0xDeF"));
    let b = chainlog_cursor_key(Chain::from_id(1), Some("0xabc"), Some("0xdef"));
    assert_eq!(a, b, "hex case must not change the key");
    assert!(
        a.starts_with("chainlog_cursor:"),
        "key carries the prefix: {a}"
    );
}

#[test]
fn chainlog_cursor_key_differs_by_each_input() {
    let base = chainlog_cursor_key(Chain::from_id(1), Some("0xabc"), Some("0xdef"));
    assert_ne!(
        base,
        chainlog_cursor_key(Chain::from_id(10), Some("0xabc"), Some("0xdef")),
        "chain id is part of the key",
    );
    assert_ne!(
        base,
        chainlog_cursor_key(Chain::from_id(1), Some("0x999"), Some("0xdef")),
        "address is part of the key",
    );
    assert_ne!(
        base,
        chainlog_cursor_key(Chain::from_id(1), Some("0xabc"), None),
        "topic presence changes the key",
    );
    assert_ne!(
        base,
        chainlog_cursor_key(Chain::from_id(1), None, Some("0xdef")),
        "address presence changes the key",
    );
}

// ── provider boot gating ──────────────────────────────────────────────

/// A stub extension registering the `acme-adapter` provider kind behind a
/// unit service, so the boot-gate tests exercise the generic kind loop
/// without a real provider component.
struct AcmeService;
impl crate::host::extension::HostService for AcmeService {}

struct AcmeKind;

#[async_trait::async_trait]
impl ProviderKind<crate::test_utils::MockTypes> for AcmeKind {
    fn kind(&self) -> &'static str {
        "acme-adapter"
    }

    fn link(
        &self,
        _linker: &mut Linker<HostState<crate::test_utils::MockTypes>>,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn install(
        &self,
        _instance: ProviderInstance<'_, crate::test_utils::MockTypes>,
        _service: &Arc<dyn HostService>,
    ) -> anyhow::Result<Installed> {
        Ok(Installed::Live)
    }
}

struct AcmeExtension;

impl Extension<crate::test_utils::MockTypes> for AcmeExtension {
    fn namespace(&self) -> &'static str {
        "acme"
    }

    fn capabilities(&self) -> manifest::NamespaceCaps {
        manifest::NamespaceCaps {
            prefix: "test:acme/",
            ifaces: &[],
        }
    }

    fn link(
        &self,
        _linker: &mut Linker<HostState<crate::test_utils::MockTypes>>,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    fn service(&self) -> Option<Arc<dyn HostService>> {
        Some(Arc::new(AcmeService))
    }

    fn provider(&self) -> Option<Box<dyn ProviderKind<crate::test_utils::MockTypes>>> {
        Some(Box::new(AcmeKind))
    }
}

/// The stub extension set registering the `acme-adapter` kind.
fn acme_extensions() -> Vec<Arc<dyn Extension<crate::test_utils::MockTypes>>> {
    vec![Arc::new(AcmeExtension)]
}

/// The module-kind discriminator gates the provider load path: an
/// `[[adapters]]` entry whose manifest is (or defaults to) an event-module
/// is rejected before instantiation with a message naming the registered
/// kinds.
#[tokio::test]
async fn boot_rejects_provider_whose_manifest_is_an_event_module() {
    let engine = make_wasmtime_engine();
    let components = crate::test_utils::mock_components();
    let extensions = acme_extensions();
    let linker =
        crate::supervisor::build_linker::<crate::test_utils::MockTypes>(&engine, &extensions)
            .expect("build_linker");

    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = dir.path().join("module.toml");
    std::fs::write(
        &manifest,
        "[module]\nname = \"acme\"\nkind = \"event-module\"\n",
    )
    .expect("write manifest");

    let config = EngineConfig {
        adapters: vec![crate::engine_config::AdapterEntry {
            path: dir.path().join("acme.wasm"),
            manifest: Some(manifest),
            http_allow: Vec::new(),
            messaging_topics: Vec::new(),
        }],
        ..Default::default()
    };

    let err =
        match Supervisor::boot(&engine, &linker, &config, &components, &extensions, None).await {
            Ok(_) => panic!("event-module manifest in an [[adapters]] slot must be rejected"),
            Err(err) => err,
        };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("acme-adapter"),
        "the kind gate names the registered kinds: {msg}",
    );
}

/// A kind spelling no extension registered is refused at boot with a
/// message naming the registered kinds.
#[tokio::test]
async fn boot_rejects_an_unregistered_provider_kind() {
    let engine = make_wasmtime_engine();
    let components = crate::test_utils::mock_components();
    let extensions = acme_extensions();
    let linker =
        crate::supervisor::build_linker::<crate::test_utils::MockTypes>(&engine, &extensions)
            .expect("build_linker");

    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = dir.path().join("module.toml");
    std::fs::write(&manifest, "[module]\nname = \"bad\"\nkind = \"gadget\"\n")
        .expect("write manifest");

    let config = EngineConfig {
        adapters: vec![crate::engine_config::AdapterEntry {
            path: dir.path().join("gadget.wasm"),
            manifest: Some(manifest),
            http_allow: Vec::new(),
            messaging_topics: Vec::new(),
        }],
        ..Default::default()
    };

    let err =
        match Supervisor::boot(&engine, &linker, &config, &components, &extensions, None).await {
            Ok(_) => panic!("an unregistered provider kind must be refused"),
            Err(err) => err,
        };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("unregistered provider kind gadget") && msg.contains("acme-adapter"),
        "the refusal names the unknown spelling and the registered kinds: {msg}",
    );
}

/// A registered kind clears the discriminator; boot then reaches the
/// compile step and fails only because the referenced wasm is absent. This
/// proves the discriminator routed the entry to the provider load path
/// rather than rejecting it on kind.
#[tokio::test]
async fn boot_admits_a_registered_provider_kind_past_the_kind_gate() {
    let engine = make_wasmtime_engine();
    let components = crate::test_utils::mock_components();
    let extensions = acme_extensions();
    let linker =
        crate::supervisor::build_linker::<crate::test_utils::MockTypes>(&engine, &extensions)
            .expect("build_linker");

    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = dir.path().join("module.toml");
    std::fs::write(
        &manifest,
        "[module]\nname = \"acme\"\nkind = \"acme-adapter\"\n\n\
         [capabilities]\nrequired = [\"chain\"]\n",
    )
    .expect("write manifest");

    let config = EngineConfig {
        adapters: vec![crate::engine_config::AdapterEntry {
            path: dir.path().join("missing-acme.wasm"),
            manifest: Some(manifest),
            http_allow: vec!["api.acme.example".into()],
            messaging_topics: vec!["/nexum/1/acme-orders/proto".into()],
        }],
        ..Default::default()
    };

    let err =
        match Supervisor::boot(&engine, &linker, &config, &components, &extensions, None).await {
            Ok(_) => panic!("absent provider wasm must fail the compile step"),
            Err(err) => err,
        };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("compile") || msg.contains("missing-acme"),
        "boot reached the compile step past the kind gate: {msg}",
    );
    assert!(
        !msg.contains("requires a module.toml"),
        "the kind gate passed rather than rejecting: {msg}",
    );
}

/// A module subscribing to an extension kind no wired extension declares
/// is refused at boot, preserving the unknown-kind fail-fast.
#[tokio::test]
async fn boot_refuses_an_undeclared_extension_subscription_kind() {
    let Some(wasm) = example_wasm_or_skip() else {
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = dir.path().join("module.toml");
    std::fs::write(
        &manifest,
        r#"
[module]
name = "example"

[capabilities]
required = ["logging"]

[[subscription]]
kind = "acme-status"
"#,
    )
    .expect("write manifest");

    let engine = make_wasmtime_engine();
    let linker = make_linker(&engine);
    let (_dir, local_store) = temp_local_store();
    let components = test_components(local_store);
    let limits = ModuleLimits::default();

    let result = Supervisor::boot_single(
        &engine,
        &linker,
        &wasm,
        Some(&manifest),
        &components,
        &limits,
        &core_extensions(),
        None,
    )
    .await;
    let err = result
        .err()
        .expect("an undeclared extension subscription kind must refuse boot");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("unknown event kind acme-status"),
        "the refusal names the kind: {msg}",
    );
}
