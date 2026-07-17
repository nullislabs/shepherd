//! Boot-order coverage for the cow-api extension: a module that imports
//! `shepherd:cow/cow-api` boots and dispatches once the extension is wired
//! at the composition root, and fails to boot without it.
//!
//! These exercise the real wit-bindgen + supervisor path against pre-built
//! wasm artefacts and skip gracefully when the artefact is absent.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use alloy_chains::Chain;
use nexum_runtime::bindings::nexum;
use nexum_runtime::engine_config::{EngineConfig, ModuleLimits};
use nexum_runtime::host::component::{Components, RuntimeTypes};
use nexum_runtime::host::extension::Extension;
use nexum_runtime::host::local_store_redb::LocalStore;
use nexum_runtime::host::provider_pool::ProviderPool;
use nexum_runtime::host::state::HostState;
use nexum_runtime::supervisor::{Supervisor, build_linker};
use shepherd_cow_host::{OrderBookPool, ReferenceExt, extension};
use wasmtime::component::Linker;

const SEPOLIA: u64 = 11_155_111;

/// Reference-shaped lattice: the core backends plus the cow-api payload in
/// the extension slot, matching what the CLI composition root assembles.
#[derive(Debug, Clone, Copy, Default)]
struct CowTestTypes;

impl nexum_runtime::sealed::SealedRuntimeTypes for CowTestTypes {}

impl RuntimeTypes for CowTestTypes {
    type Chain = ProviderPool;
    type Store = LocalStore;
    type Ext = ReferenceExt;
}

fn cow_extensions() -> Vec<Arc<dyn Extension<CowTestTypes>>> {
    vec![extension::<CowTestTypes>()]
}

fn make_wasmtime_engine() -> wasmtime::Engine {
    let mut config = wasmtime::Config::new();
    config.wasm_component_model(true);
    config.consume_fuel(true);
    wasmtime::Engine::new(&config).expect("wasmtime engine")
}

fn make_linker(engine: &wasmtime::Engine) -> Linker<HostState<CowTestTypes>> {
    build_linker::<CowTestTypes>(engine, &cow_extensions()).expect("build_linker")
}

/// A chainless provider pool: no `[chains]` entries, so every
/// `chain::request` surfaces `UnknownChain`. Enough to prove boot and
/// dispatch without a live RPC endpoint.
async fn chainless_pool() -> ProviderPool {
    ProviderPool::from_config(&EngineConfig::default())
        .await
        .expect("chainless provider pool")
}

async fn test_components(store: LocalStore) -> Components<CowTestTypes> {
    Components {
        chain: chainless_pool().await,
        store,
        ext: ReferenceExt {
            cow: OrderBookPool::default(),
        },
        logs: nexum_runtime::host::logs::LogPipeline::in_memory(ModuleLimits::default().logs()),
    }
}

fn temp_local_store() -> (tempfile::TempDir, LocalStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ls.redb");
    let store = LocalStore::open(path).expect("local store");
    (dir, store)
}

/// Path to a module's `.wasm` artefact under the workspace target dir.
/// `CARGO_MANIFEST_DIR` is `crates/shepherd-cow-host`; two parents up is
/// the workspace root, mirroring the runtime's own helper.
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

async fn boot_production_module(
    engine: &wasmtime::Engine,
    linker: &Linker<HostState<CowTestTypes>>,
    local_store: &LocalStore,
    wasm: &Path,
    manifest: &Path,
) -> Supervisor<CowTestTypes> {
    let components = test_components(local_store.clone()).await;
    let limits = ModuleLimits::default();
    Supervisor::boot_single(
        engine,
        linker,
        wasm,
        Some(manifest),
        &components,
        &limits,
        &cow_extensions(),
        None,
    )
    .await
    .expect("boot_single")
}

/// ethflow-watcher imports `shepherd:cow/cow-api` and subscribes to logs;
/// it boots with the cow extension and a synthetic log is delivered.
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

    // A log with an unrecognised topic is silently skipped by the module's
    // decoder (returns `None` from `decode_order_placement`), so the test
    // only proves: supervisor delivered, module did not trap, module stayed
    // alive.
    let synthetic_log = alloy_rpc_types_eth::Log::default();
    let dispatched = supervisor
        .dispatch_chain_log(
            "ethflow-watcher",
            Chain::from_id(SEPOLIA),
            synthetic_log,
            None,
        )
        .await;
    assert!(dispatched);
    assert_eq!(supervisor.alive_count(), 1);
}

/// stop-loss imports `shepherd:cow/cow-api`; it boots with the cow
/// extension and a block dispatch reaches it.
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

/// The boot-order invariant, exercised (not merely asserted in prose):
/// a module that imports `shepherd:cow/cow-api` (ethflow-watcher) must
/// NOT boot when the cow extension is absent from the linker AND the
/// capability registry. The paired linker-hook + capability-namespace
/// registration is what makes the same module boot in the tests above;
/// drop the pairing and boot fails.
#[tokio::test]
async fn ethflow_watcher_without_cow_extension_fails_to_boot() {
    let Some(wasm) = module_wasm_or_skip("ethflow-watcher") else {
        return;
    };
    let manifest = production_module_toml("modules/ethflow-watcher/module.toml");
    let engine = make_wasmtime_engine();
    // Core-only: no cow linker hook, no cow capability namespace.
    let linker = build_linker::<CowTestTypes>(&engine, &[]).expect("build_linker");
    let (_dir, store) = temp_local_store();
    let components = test_components(store).await;
    let limits = ModuleLimits::default();

    let result = Supervisor::boot_single(
        &engine,
        &linker,
        &wasm,
        Some(&manifest),
        &components,
        &limits,
        &[],
        None,
    )
    .await;

    let err = result
        .err()
        .expect("cow-importing module must not boot without the cow extension registered");
    // Pin the failure to its specific cause: ethflow-watcher declares
    // the cow-api capability, which a core-only registry does not
    // recognise (registering it is exactly what the cow extension does).
    // Rules out an unrelated failure masquerading as the invariant.
    let chain = format!("{err:#}");
    assert!(
        chain.contains(r#"unknown capability "cow-api""#),
        "expected the cow-api unknown-capability failure, got: {chain}",
    );
}
