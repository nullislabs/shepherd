use std::path::{Path, PathBuf};

use super::*;
use crate::engine_config::ModuleLimits;

#[test]
fn empty_supervisor_returns_no_subscriptions() {
    let engine = make_wasmtime_engine();
    let (_dir, store) = temp_local_store();
    let sup = Supervisor::empty_for_test(&engine, store);
    assert!(sup.block_chains().is_empty());
    assert!(sup.log_subscriptions().is_empty());
    assert_eq!(sup.module_count(), 0);
}

/// Regression guard: engines whose modules only declare
/// `[[subscription]] kind = "block"` (or only `kind = "log"`) must not
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
    let (_dir, store) = temp_local_store();
    let mut supervisor = Supervisor::empty_for_test(&engine, store);
    let started = Instant::now();
    let shutdown = tokio::time::sleep(Duration::from_millis(50));

    crate::runtime::event_loop::run(
        &mut supervisor,
        Vec::new(),
        Vec::new(),
        tokio::task::JoinSet::new(),
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

// ── E2E helpers ───────────────────────────────────────────────────────

/// Path to the pre-built example WASM component. Tests that need it
/// call `example_wasm_or_skip()` which skips gracefully if absent.
fn example_wasm() -> PathBuf {
    // CARGO_MANIFEST_DIR → crates/nexum-engine
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

fn make_linker(engine: &wasmtime::Engine) -> Linker<crate::HostState> {
    let mut linker = Linker::<crate::HostState>::new(engine);
    crate::Shepherd::add_to_linker::<
        crate::HostState,
        wasmtime::component::HasSelf<crate::HostState>,
    >(&mut linker, |s| s)
    .expect("add_to_linker");
    wasmtime_wasi::p2::add_to_linker_async(&mut linker).expect("add_wasi");
    linker
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

// ── E2E tests ─────────────────────────────────────────────────────────

/// Boot supervisor with the example module; verify it starts alive.
#[tokio::test]
async fn e2e_supervisor_boots_example_module() {
    let Some(wasm) = example_wasm_or_skip() else {
        return;
    };
    let engine = make_wasmtime_engine();
    let linker = make_linker(&engine);
    let cow_pool = crate::host::cow_orderbook::OrderBookPool::default();
    let provider_pool = crate::host::provider_pool::ProviderPool::empty();
    let (_dir, local_store) = temp_local_store();

    let limits = ModuleLimits::default();
    let supervisor = Supervisor::boot_single(
        &engine,
        &linker,
        &wasm,
        Some(example_module_toml()).as_deref(),
        &cow_pool,
        &provider_pool,
        &local_store,
        &limits,
    )
    .await
    .expect("boot_single");

    assert_eq!(supervisor.module_count(), 1);
    assert_eq!(supervisor.alive_count(), 1);
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
    let cow_pool = crate::host::cow_orderbook::OrderBookPool::default();
    let provider_pool = crate::host::provider_pool::ProviderPool::empty();
    let (_dir, local_store) = temp_local_store();
    let limits = ModuleLimits::default();

    let mut supervisor = Supervisor::boot_single(
        &engine,
        &linker,
        &wasm,
        Some(&manifest),
        &cow_pool,
        &provider_pool,
        &local_store,
        &limits,
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
    linker: &Linker<crate::HostState>,
    local_store: &crate::host::local_store_redb::LocalStore,
    wasm: &Path,
    manifest: &Path,
) -> Supervisor {
    let cow_pool = crate::host::cow_orderbook::OrderBookPool::default();
    let provider_pool = crate::host::provider_pool::ProviderPool::empty();
    let limits = ModuleLimits::default();
    Supervisor::boot_single(
        engine,
        linker,
        wasm,
        Some(manifest),
        &cow_pool,
        &provider_pool,
        local_store,
        &limits,
    )
    .await
    .expect("boot_single")
}

#[tokio::test]
async fn e2e_twap_monitor_block_dispatch() {
    let Some(wasm) = module_wasm_or_skip("twap-monitor") else {
        return;
    };
    let manifest = production_module_toml("modules/twap-monitor/module.toml");
    let engine = make_wasmtime_engine();
    let linker = make_linker(&engine);
    let (_dir, store) = temp_local_store();

    let mut supervisor = boot_production_module(&engine, &linker, &store, &wasm, &manifest).await;
    assert_eq!(supervisor.module_count(), 1);
    assert_eq!(supervisor.alive_count(), 1);

    // twap-monitor subscribes to Sepolia blocks (poll path). A real
    // poll would call chain::request, which ProviderPool::empty() does
    // not satisfy - the module surfaces a host-error and warns; the
    // supervisor must keep the module alive because the strategy
    // catches the error and returns Ok(()).
    let dispatched = supervisor.dispatch_block(synthetic_sepolia_block()).await;
    assert_eq!(dispatched, 1);
    assert_eq!(supervisor.alive_count(), 1);
}

#[tokio::test]
async fn e2e_ethflow_watcher_log_dispatch() {
    let Some(wasm) = module_wasm_or_skip("ethflow-watcher") else {
        return;
    };
    let manifest = production_module_toml("modules/ethflow-watcher/module.toml");
    let engine = make_wasmtime_engine();
    let linker = make_linker(&engine);
    let (_dir, store) = temp_local_store();

    let mut supervisor = boot_production_module(&engine, &linker, &store, &wasm, &manifest).await;
    assert_eq!(supervisor.alive_count(), 1);

    // A log with an unrecognised topic is silently skipped by the
    // module's decoder (returns `None` from `decode_order_placement`),
    // so the test only proves: supervisor delivered, module did not
    // trap, module stayed alive. Stronger asserts (submitted:{uid}
    // markers etc.) require a hand-crafted ABI-encoded OrderPlacement
    // payload and the real ETH_FLOW_PRODUCTION address, deferred to
    // Testnet integration.
    let synthetic_log = alloy_rpc_types_eth::Log::default();
    let dispatched = supervisor
        .dispatch_log("ethflow-watcher", SEPOLIA, synthetic_log)
        .await;
    assert!(dispatched);
    assert_eq!(supervisor.alive_count(), 1);
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

#[tokio::test]
async fn e2e_stop_loss_block_dispatch() {
    let Some(wasm) = module_wasm_or_skip("stop-loss") else {
        return;
    };
    let manifest = production_module_toml("modules/examples/stop-loss/module.toml");
    let engine = make_wasmtime_engine();
    let linker = make_linker(&engine);
    let (_dir, store) = temp_local_store();

    let mut supervisor = boot_production_module(&engine, &linker, &store, &wasm, &manifest).await;
    let dispatched = supervisor.dispatch_block(synthetic_sepolia_block()).await;
    assert_eq!(dispatched, 1);
    assert_eq!(supervisor.alive_count(), 1);
}

// ── Init-failed modules must be marked dead ────────────────

/// Drive `Supervisor::boot_single` with a module whose `[config]`
/// carries a malformed `threshold` value (`"not-a-number"`). The
/// module's `init` returns `Err(HostError { kind: InvalidInput })`.
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
async fn boot_fixture(wasm: &Path, manifest_relative: &str) -> Supervisor {
    let engine = make_wasmtime_engine();
    let linker = make_linker(&engine);
    let cow_pool = crate::host::cow_orderbook::OrderBookPool::default();
    let provider_pool = crate::host::provider_pool::ProviderPool::empty();
    let (_dir, local_store) = temp_local_store();
    let manifest = fixture_module_toml(manifest_relative);
    let limits = crate::engine_config::ModuleLimits::default();
    Supervisor::boot_single(
        &engine,
        &linker,
        wasm,
        Some(&manifest),
        &cow_pool,
        &provider_pool,
        &local_store,
        &limits,
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
    let cow_pool = crate::host::cow_orderbook::OrderBookPool::default();
    let provider_pool = crate::host::provider_pool::ProviderPool::empty();
    let (_dir, local_store) = temp_local_store();

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
        },
        limits: crate::engine_config::ModuleLimits::default(),
        chains: std::collections::BTreeMap::new(),
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
    };

    let mut supervisor = Supervisor::boot(
        &engine,
        &linker,
        &engine_cfg,
        &cow_pool,
        &provider_pool,
        &local_store,
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
    let cow_pool = crate::host::cow_orderbook::OrderBookPool::default();
    let provider_pool = crate::host::provider_pool::ProviderPool::empty();
    let (_dir, store) = temp_local_store();
    let limits = crate::engine_config::ModuleLimits::default();
    let mut supervisor = Supervisor::boot_single(
        &engine,
        &linker,
        &wasm,
        Some(&manifest),
        &cow_pool,
        &provider_pool,
        &store,
        &limits,
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
    let cow_pool = crate::host::cow_orderbook::OrderBookPool::default();
    let provider_pool = crate::host::provider_pool::ProviderPool::empty();
    let (_dir, store) = temp_local_store();

    // Tight policy: 3 failures in 60 s -> quarantine. Keeps the
    // test wall-clock under 4 s.
    let policy =
        crate::runtime::poison_policy::PoisonPolicy::new(3, std::time::Duration::from_secs(60));
    let limits = crate::engine_config::ModuleLimits::default();
    let mut supervisor = Supervisor::boot_single(
        &engine,
        &linker,
        &wasm,
        Some(&manifest),
        &cow_pool,
        &provider_pool,
        &store,
        &limits,
    )
    .await
    .expect("boot_single")
    .with_poison_policy(policy);

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
    let cow_pool = crate::host::cow_orderbook::OrderBookPool::default();
    let provider_pool = crate::host::provider_pool::ProviderPool::empty();
    let (_dir, local_store) = temp_local_store();

    let engine_cfg = crate::engine_config::EngineConfig {
        engine: crate::engine_config::EngineSection {
            state_dir: dir.path().to_path_buf(),
            log_level: "info".into(),
            metrics: crate::engine_config::MetricsSection::default(),
        },
        limits: crate::engine_config::ModuleLimits::default(),
        chains: std::collections::BTreeMap::new(),
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
    };

    let mut supervisor = Supervisor::boot(
        &engine,
        &linker,
        &engine_cfg,
        &cow_pool,
        &provider_pool,
        &local_store,
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
    let cow_pool = crate::host::cow_orderbook::OrderBookPool::default();
    let provider_pool = crate::host::provider_pool::ProviderPool::empty();
    let (_dir, local_store) = temp_local_store();

    let engine_cfg = crate::engine_config::EngineConfig {
        engine: crate::engine_config::EngineSection {
            state_dir: dir.path().to_path_buf(),
            log_level: "info".into(),
            metrics: crate::engine_config::MetricsSection::default(),
        },
        limits: crate::engine_config::ModuleLimits::default(),
        chains: std::collections::BTreeMap::new(),
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
    };

    let policy =
        crate::runtime::poison_policy::PoisonPolicy::new(2, std::time::Duration::from_secs(60));
    let mut supervisor = Supervisor::boot(
        &engine,
        &linker,
        &engine_cfg,
        &cow_pool,
        &provider_pool,
        &local_store,
    )
    .await
    .expect("boot")
    .with_poison_policy(policy);
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
