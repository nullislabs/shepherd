//! Multi-module supervisor.
//!
//! Loads every `[[modules]]` entry from `engine.toml`, instantiates
//! each as an `EventModule` binding against a dedicated wasmtime
//! `Store`, and routes the event types declared in each manifest's
//! `[[subscription]]` table.
//!
//! Trap handling: a wasmtime trap in `on_event`
//! marks the module `alive = false`, increments `failure_count`, and
//! schedules a `next_attempt` instant via `runtime::restart_policy::
//! backoff_for`. The next dispatch eligible after that instant
//! re-instantiates the component (fresh `Store` + bindings; the
//! wasm instance left by a trap is poisoned with "cannot enter
//! component instance") and re-calls `init`. On a successful
//! `on_event` the failure counter resets to 0.
//!
//! Modules whose `init` returned `Err(fault)` are dead with
//! `next_attempt = None` and never get scheduled - the init failure
//! is treated as a manifest / config bug, not a transient.
//!
//! Multi-chain isolation: `dispatch_block(block)` walks
//! every module but only enters those whose subscriptions match
//! `block.chain_id`. Per-module restart / poison / fuel limits are
//! independent across chains, so a poisoned module on chain A
//! cannot starve modules on chain B. The upstream WS reconnect
//! tasks own one per-chain backoff timer each, so a
//! chain-A connection drop does not block chain-B events.

use std::path::Path;
use std::sync::Arc;

use alloy_chains::Chain;
use anyhow::{Context, Error, Result, anyhow};
use tracing::{debug, error, info, warn};
use tracing_core::Level;
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{HostMonotonicClock, HostWallClock, WasiCtxBuilder};

use crate::bindings::{Config, EventModule, VenueAdapter, nexum};
use crate::engine_config::{
    AdapterEntry, EngineConfig, ModuleEntry, ModuleLimits, OutboundHttpLimits,
};
use crate::host::component::{Components, RuntimeTypes, StateHandle, StateStore};
use crate::host::extension::Extension;
use crate::host::http::HttpGate;
#[cfg(test)]
use crate::host::local_store_redb::LocalStore;
use crate::host::logs::{LogRecord, LogSource, RunId, StdioStream};
use crate::host::pool_router::{AdapterActor, PoolRouter, PoolRouterBuilder};
#[cfg(test)]
use crate::host::provider_pool::ProviderPool;
use crate::host::state::HostState;
use crate::manifest::{self, CapabilityRegistry, LoadedManifest, ModuleKind, Subscription};

/// Owns every loaded module and exposes the dispatch surface the
/// event loop needs. Generic over the [`RuntimeTypes`] lattice binding
/// the component seam backends.
pub struct Supervisor<T: RuntimeTypes> {
    modules: Vec<LoadedModule<T>>,
    /// The intent pool router: every installed venue adapter's serialising
    /// store, keyed by venue id. Cached so a module restart rebuilds a store
    /// carrying the same shared handle. Adapters boot through the same store,
    /// fuel, and memory machinery as modules but carry no subscriptions:
    /// modules reach them through this router, not through dispatch. Folding
    /// adapters into the restart and poison sweeps is still a later change.
    pool_router: PoolRouter,
    /// Venue adapters loaded at boot, whether or not `init` succeeded.
    adapters_total: usize,
    /// Adapters whose `init` succeeded and that are installed for routing.
    adapters_alive: usize,
    /// Cached for module restart: re-instantiating a trapped module
    /// requires a fresh wasmtime `Store` + `Linker`, which in turn need
    /// the shared backends. The `Components` bundle is cheaply cloned
    /// (Arc-backed members) so the supervisor takes an owned copy at boot.
    engine: Engine,
    components: Components<T>,
    /// Extensions wired at boot. Cached so the module-restart path can
    /// rebuild an identical linker (core interfaces plus every extension
    /// hook) without re-consulting the composition root.
    extensions: Vec<Extension<T>>,
    /// Poison-pill thresholds resolved from `[limits.poison]` at boot
    /// (production defaults: 5 failures / 10 min).
    poison_policy: crate::runtime::poison_policy::PoisonPolicy,
    /// Optional WASI clock override applied to every module store,
    /// including the ones rebuilt on restart. `None` leaves the ambient
    /// host clocks.
    clocks: Option<WasiClockOverride>,
}

/// Core-only lattice for the runtime's own tests: the reference core
/// backends with an empty extension slot (`Ext = ()`). Domain-extension
/// boot coverage lives in the extension crate that owns the backend.
#[cfg(test)]
#[derive(Clone, Copy, Default)]
pub(crate) struct TestTypes;

#[cfg(test)]
impl RuntimeTypes for TestTypes {
    type Chain = ProviderPool;
    type Store = LocalStore;
    type Ext = ();
}

/// The supervisor the runtime's own tests drive. The launch path infers
/// its lattice from the composition root instead.
#[cfg(test)]
pub(crate) type DefaultSupervisor = Supervisor<TestTypes>;

/// A wasmtime `Store` holding the lattice `HostState`. Named so the
/// module and helper signatures stay legible.
type HostStore<T> = Store<HostState<T>>;

/// Per-store WASI clock override applied to every module store.
///
/// Threaded from the assembly through the boot paths onto each store's
/// `WasiCtxBuilder`. The shared wall and monotonic sources let a test handle
/// drive guest-visible time; leaving it `None` keeps the ambient host clocks,
/// so the default is behaviour-neutral. `RunId.started_at` is host wall-clock
/// and is unaffected.
#[derive(Clone)]
pub struct WasiClockOverride {
    wall: Arc<dyn HostWallClock + Send + Sync>,
    monotonic: Arc<dyn HostMonotonicClock + Send + Sync>,
}

impl WasiClockOverride {
    /// Pair a shared wall clock with a shared monotonic clock.
    pub fn new(
        wall: Arc<dyn HostWallClock + Send + Sync>,
        monotonic: Arc<dyn HostMonotonicClock + Send + Sync>,
    ) -> Self {
        Self { wall, monotonic }
    }
}

/// Adapts a shared wall clock into the by-value `HostWallClock` the
/// `WasiCtxBuilder` takes ownership of per store.
struct SharedWallClock(Arc<dyn HostWallClock + Send + Sync>);

impl HostWallClock for SharedWallClock {
    fn resolution(&self) -> std::time::Duration {
        self.0.resolution()
    }

    fn now(&self) -> std::time::Duration {
        self.0.now()
    }
}

/// Adapts a shared monotonic clock into the by-value `HostMonotonicClock` the
/// `WasiCtxBuilder` takes ownership of per store.
struct SharedMonotonicClock(Arc<dyn HostMonotonicClock + Send + Sync>);

impl HostMonotonicClock for SharedMonotonicClock {
    fn resolution(&self) -> u64 {
        self.0.resolution()
    }

    fn now(&self) -> u64 {
        self.0.now()
    }
}

struct LoadedModule<T: RuntimeTypes> {
    name: String,
    bindings: EventModule,
    store: HostStore<T>,
    /// The run this store instantiates. Restarts mint a fresh `RunId`
    /// with an incremented sequence; the supervisor's death path stamps
    /// the synthesized panic record with it.
    run: RunId,
    /// Subscriptions copied from `module.toml`. The supervisor reads
    /// these on every event to decide whether to dispatch.
    subscriptions: Vec<Subscription>,
    /// Fuel budget refilled before each `on_event` invocation.
    fuel_per_event: u64,
    /// Memory cap applied to the wasmtime store on reinstantiation.
    memory_limit: usize,
    /// Cached for restart: re-instantiating from the original
    /// wasm bytes avoids re-reading the file on every restart. The
    /// `Component` itself is internally `Arc`-backed by wasmtime.
    component: Component,
    /// Cached for restart: the manifest's `[config]` we pass
    /// to `Guest::init`. Cloning a `Vec<(String, String)>` is cheap.
    init_config: Config,
    /// Cached for restart: HTTP allowlist baked into the
    /// `HostState` we rebuild on each re-instantiation.
    http_allowlist: Vec<String>,
    /// Cached for restart: outbound HTTP limits baked into the
    /// `HostState` we rebuild on each re-instantiation.
    http_limits: OutboundHttpLimits,
    /// Set to `false` when `on_event` traps. Dead modules are
    /// excluded from dispatch until `next_attempt` is in the past.
    /// Modules whose `init` failed have `alive = false`
    /// + `next_attempt = None`, so they never come back.
    alive: bool,
    /// Number of consecutive trap-style failures since the last
    /// successful dispatch. Resets to 0 on success. Drives the
    /// exponential backoff via `restart_policy::backoff_for`.
    failure_count: u32,
    /// Earliest instant at which the supervisor may retry this
    /// module after a trap. `None` for healthy modules + for modules
    /// whose `init` failed (the latter never get scheduled because
    /// the dispatch fast-path checks `next_attempt` *and* requires
    /// `alive = false` before flipping back).
    next_attempt: Option<std::time::Instant>,
    /// Sliding-window record of recent trap timestamps for the
    /// poison-pill check. Entries older than the
    /// `PoisonPolicy.window` are dropped on each push.
    failure_timestamps: std::collections::VecDeque<std::time::Instant>,
    /// Once `true` the module is permanently quarantined: no restart
    /// attempts, no dispatches, no metric churn. Recovery requires
    /// an operator-driven full engine restart with the module
    /// removed from `engine.toml::[[modules]]`.
    poisoned: bool,
}

/// A venue adapter instantiated into a supervised store, ready to install in
/// the pool router. It boots through the same store, fuel, and memory
/// machinery as a module but carries no subscriptions: modules reach it
/// through the router, not through dispatch. Adapter restart and poison
/// handling are still a later change; an `init` failure leaves `alive` false
/// so the adapter is loaded but not routable.
struct LoadedAdapter<T: RuntimeTypes> {
    /// Venue id the adapter answers for (its manifest name).
    venue_id: String,
    /// The refuelable adapter store, ready to serialise behind a router mutex.
    actor: AdapterActor<T>,
    /// Whether `init` succeeded; a failed adapter is not installed for routing.
    alive: bool,
}

impl<T: RuntimeTypes> Supervisor<T> {
    /// Compile + instantiate every module declared in
    /// `engine_cfg.modules`. The wasmtime `Engine` + `Linker` are
    /// passed in so `main.rs` can build them once.
    pub async fn boot(
        engine: &Engine,
        linker: &Linker<HostState<T>>,
        engine_cfg: &EngineConfig,
        components: &Components<T>,
        extensions: &[Extension<T>],
        clocks: Option<WasiClockOverride>,
    ) -> Result<Self> {
        let registry = capability_registry(extensions);
        // Adapters instantiate first: the pool router must contain them before
        // any module store (which carries the built router) is built. Adapters
        // link only their scoped transport, against a dedicated linker built
        // from the same core backends, and their own stores carry an empty
        // router since an adapter cannot call pool.
        let adapter_linker = build_adapter_linker::<T>(engine)?;
        let adapter_registry = CapabilityRegistry::adapter();
        let mut router_builder = PoolRouterBuilder::new(engine_cfg.limits.quota());
        let adapters_total = engine_cfg.adapters.len();
        let mut adapters_alive = 0;
        for entry in &engine_cfg.adapters {
            let loaded = Self::load_adapter(
                engine,
                &adapter_linker,
                entry,
                components,
                &engine_cfg.limits,
                &adapter_registry,
                clocks.as_ref(),
            )
            .await
            .with_context(|| format!("load adapter {}", entry.path.display()))?;
            if loaded.alive {
                adapters_alive += 1;
                router_builder
                    .install(loaded.venue_id.clone(), loaded.actor)
                    .with_context(|| format!("install adapter {}", loaded.venue_id))?;
            } else {
                warn!(
                    adapter = %loaded.venue_id,
                    "adapter init failed - not installed for routing",
                );
            }
        }
        let pool_router = router_builder.build();

        let mut modules = Vec::with_capacity(engine_cfg.modules.len());
        for entry in &engine_cfg.modules {
            let loaded = Self::load_one(
                engine,
                linker,
                entry,
                components,
                &engine_cfg.limits,
                &registry,
                clocks.as_ref(),
                pool_router.clone(),
            )
            .await
            .with_context(|| format!("load module {}", entry.path.display()))?;
            modules.push(loaded);
        }
        let alive = modules.iter().filter(|m| m.alive).count();
        info!(
            loaded = modules.len(),
            alive,
            adapters = adapters_total,
            adapters_alive,
            "supervisor up"
        );
        Ok(Self {
            modules,
            pool_router,
            adapters_total,
            adapters_alive,
            engine: engine.clone(),
            components: components.clone(),
            extensions: extensions.to_vec(),
            poison_policy: engine_cfg.limits.poison(),
            clocks,
        })
    }

    /// One-shot construction from a single ad-hoc `(component, manifest)`
    /// pair. Used by the CLI-positional invocation so `just run`
    /// against the example module keeps working without an
    /// `engine.toml`.
    // One flat argument per shared backend and resource knob, plus the
    // optional clock override; bundling would obscure the call site.
    #[allow(clippy::too_many_arguments)]
    pub async fn boot_single(
        engine: &Engine,
        linker: &Linker<HostState<T>>,
        wasm: &Path,
        manifest: Option<&Path>,
        components: &Components<T>,
        limits: &ModuleLimits,
        extensions: &[Extension<T>],
        clocks: Option<WasiClockOverride>,
    ) -> Result<Self> {
        let registry = capability_registry(extensions);
        let entry = ModuleEntry {
            path: wasm.to_path_buf(),
            manifest: manifest.map(Path::to_path_buf),
        };
        // The single-module override path serves `just run`; adapters are
        // configured through `engine.toml`, so the router is empty here and
        // every pool call resolves to `unknown-venue`.
        let pool_router = PoolRouter::empty();
        let loaded = Self::load_one(
            engine,
            linker,
            &entry,
            components,
            limits,
            &registry,
            clocks.as_ref(),
            pool_router.clone(),
        )
        .await?;
        Ok(Self {
            modules: vec![loaded],
            pool_router,
            adapters_total: 0,
            adapters_alive: 0,
            engine: engine.clone(),
            components: components.clone(),
            extensions: extensions.to_vec(),
            poison_policy: limits.poison(),
            clocks,
        })
    }

    /// Build a fresh wasmtime `Store` wired to the shared backends, with
    /// the per-run namespace, allowlist, memory cap, and fuel applied.
    /// Shared by `load_one` and `reinstantiate_one`; each call takes a
    /// freshly minted [`RunId`] so a restart's store is a distinct run.
    // One flat argument per resource knob threaded onto the store, plus the
    // optional clock override.
    #[allow(clippy::too_many_arguments)]
    fn build_store(
        engine: &Engine,
        components: &Components<T>,
        run: RunId,
        http_allowlist: Vec<String>,
        http_limits: OutboundHttpLimits,
        messaging_topics: Vec<String>,
        memory_limit: usize,
        fuel: u64,
        clocks: Option<&WasiClockOverride>,
        pool_router: PoolRouter,
    ) -> Result<HostStore<T>> {
        let namespace: &str = &run.module;
        // Capture guest stdout/stderr per store instead of inheriting the
        // host's: each pipe is line-buffered and routed as run- and
        // source-tagged log records. Stdin is deliberately left at the
        // default closed stream rather than inherited; a sandboxed
        // event-driven module has no host console to read. The ctx grants
        // no network
        // (`inherit_network` is never called), which keeps the ambient
        // wasi:sockets bindings inert and the allowlisted wasi:http gate
        // the only live network path. WASI clocks default to ambient;
        // `WasiClockOverride`, when present, is the per-store
        // virtualization point for deterministic guest time in tests and
        // replay.
        let router = components.logs.router();
        let mut builder = WasiCtxBuilder::new();
        builder
            .stdout(StdioStream::new(
                router.clone(),
                run.clone(),
                LogSource::Stdout,
            ))
            .stderr(StdioStream::new(
                router.clone(),
                run.clone(),
                LogSource::Stderr,
            ));
        if let Some(clocks) = clocks {
            builder.wall_clock(SharedWallClock(clocks.wall.clone()));
            builder.monotonic_clock(SharedMonotonicClock(clocks.monotonic.clone()));
        }
        let wasi = builder.build();
        let limits = wasmtime::StoreLimitsBuilder::new()
            .memory_size(memory_limit)
            .build();
        let module_store = components
            .store
            .module(namespace)
            .map_err(|e| anyhow!("local-store namespace for {namespace}: {e}"))?;
        let mut store = Store::new(
            engine,
            HostState {
                wasi,
                table: ResourceTable::new(),
                limits,
                http_ctx: wasmtime_wasi_http::WasiHttpCtx::new(),
                http_gate: HttpGate::new(namespace, http_allowlist, http_limits),
                messaging_topics,
                run,
                log_router: router,
                ext: components.ext.clone(),
                chain: components.chain.clone(),
                store: module_store,
                pool_router,
            },
        );
        store.limiter(|state| &mut state.limits);
        store.set_fuel(fuel)?;
        Ok(store)
    }

    // One flat argument per shared input threaded onto the store, plus the
    // pool router the module's `nexum:intent/pool` import dispatches to.
    #[allow(clippy::too_many_arguments)]
    async fn load_one(
        engine: &Engine,
        linker: &Linker<HostState<T>>,
        entry: &ModuleEntry,
        components: &Components<T>,
        limits_cfg: &ModuleLimits,
        registry: &CapabilityRegistry,
        clocks: Option<&WasiClockOverride>,
        pool_router: PoolRouter,
    ) -> Result<LoadedModule<T>> {
        let manifest_path = resolve_manifest_path(&entry.path, entry.manifest.as_deref());
        let loaded_manifest: LoadedManifest = match manifest_path.as_deref() {
            Some(p) if p.exists() => {
                info!(manifest = %p.display(), "loading module manifest");
                manifest::load(p, registry)?
            }
            _ => {
                warn!(
                    component = %entry.path.display(),
                    "no module.toml - falling back to anonymous module"
                );
                manifest::fallback_manifest()
            }
        };

        // Compile + instantiate.
        info!(component = %entry.path.display(), "compiling component");
        let component = Component::from_file(engine, &entry.path)
            .map_err(Error::from)
            .with_context(|| format!("compile {}", entry.path.display()))?;

        // Enforce capability declarations before spending time on instantiation.
        manifest::enforce_capabilities(
            &loaded_manifest,
            component.component_type().imports(engine).map(|(n, _)| n),
            registry,
        )
        .with_context(|| format!("capability violation in {}", entry.path.display()))?;
        let module_namespace = if loaded_manifest.manifest.module.name.is_empty() {
            "module".to_owned()
        } else {
            loaded_manifest.manifest.module.name.clone()
        };
        info!(
            module = %module_namespace,
            fuel = limits_cfg.fuel(),
            memory_bytes = limits_cfg.memory(),
            "applied module resource limits",
        );
        // First run of this module: sequence 0. Restarts increment it.
        let run = RunId::new(module_namespace.clone(), 0);
        let mut store = Self::build_store(
            engine,
            components,
            run.clone(),
            loaded_manifest.http_allowlist.clone(),
            limits_cfg.http(),
            // Event modules are unscoped for messaging; only venue
            // adapters carry a topic grant.
            Vec::new(),
            limits_cfg.memory(),
            limits_cfg.fuel(),
            clocks,
            pool_router,
        )?;
        let bindings = EventModule::instantiate_async(&mut store, &component, linker)
            .await
            .map_err(Error::from)
            .with_context(|| format!("instantiate {}", entry.path.display()))?;

        // Call `init` with the manifest's `[config]`.
        let config: Config = if loaded_manifest.config.is_empty() {
            vec![("name".into(), module_namespace.clone())]
        } else {
            loaded_manifest.config.clone()
        };
        // Whether `init` returned `Ok(())`. When `init` returns
        // `Err(fault)` the module's strategy state (e.g. an
        // `OnceLock<Settings>`) is left uninitialised. Existing M3
        // example modules short-circuit on the missing state via
        // `SETTINGS.get().is_none() -> return Ok(())`, but future
        // modules without that guard could panic, and even with the
        // guard each dispatch wastes fuel + an RPC subscription tick
        // on a no-op. The `LoadedModule.alive` flag below is set from
        // this result so the dispatcher skips the failed module
        // without surfacing it to the dispatch fast-path.
        let init_succeeded = match bindings
            .call_init(&mut store, &config)
            .await
            .map_err(Error::from)?
        {
            Ok(()) => {
                info!(module = %module_namespace, "init succeeded");
                true
            }
            Err(e) => {
                warn!(
                    module = %module_namespace,
                    kind = crate::host::error::fault_label(&e),
                    message = crate::host::error::fault_message(&e),
                    "init failed - module loaded but marked dead; dispatcher will skip it",
                );
                false
            }
        };
        // Refuel after init so the first on_event starts with a full budget.
        store.set_fuel(limits_cfg.fuel())?;

        // Surface any `[[subscription]]` entries the host cannot
        // service yet, so an operator running 0.2 against a 0.3
        // manifest does not silently drop events.
        for sub in &loaded_manifest.manifest.subscriptions {
            if matches!(sub, Subscription::Cron { .. }) {
                warn!(
                    module = %module_namespace,
                    "cron subscriptions are declared but inert in 0.2 (lands in 0.3)",
                );
            }
        }

        Ok(LoadedModule {
            name: module_namespace,
            bindings,
            store,
            run,
            subscriptions: loaded_manifest.manifest.subscriptions.clone(),
            fuel_per_event: limits_cfg.fuel(),
            memory_limit: limits_cfg.memory(),
            alive: init_succeeded,
            failure_count: 0,
            next_attempt: None,
            component,
            init_config: config,
            http_allowlist: loaded_manifest.http_allowlist.clone(),
            http_limits: limits_cfg.http(),
            failure_timestamps: std::collections::VecDeque::new(),
            poisoned: false,
        })
    }

    /// Load one `[[adapters]]` entry: resolve its manifest, verify it
    /// declares the venue-adapter kind, enforce the scoped-transport
    /// capability set, build a supervised store carrying the operator's
    /// HTTP and messaging grants, instantiate the `VenueAdapter` bindings
    /// against the adapter linker, and run `init`. Nothing dispatches to
    /// the result yet; it boots so the router can later reach it.
    async fn load_adapter(
        engine: &Engine,
        linker: &Linker<HostState<T>>,
        entry: &AdapterEntry,
        components: &Components<T>,
        limits_cfg: &ModuleLimits,
        registry: &CapabilityRegistry,
        clocks: Option<&WasiClockOverride>,
    ) -> Result<LoadedAdapter<T>> {
        let manifest_path = resolve_manifest_path(&entry.path, entry.manifest.as_deref());
        let loaded_manifest: LoadedManifest = match manifest_path.as_deref() {
            Some(p) if p.exists() => {
                info!(manifest = %p.display(), "loading adapter manifest");
                manifest::load(p, registry)?
            }
            _ => {
                warn!(
                    component = %entry.path.display(),
                    "no module.toml - falling back to anonymous adapter"
                );
                manifest::fallback_manifest()
            }
        };

        // The manifest kind is the discriminator: an [[adapters]] entry
        // whose manifest is (or defaults to) an event-module is a config
        // error, caught here before instantiation. A fallback manifest has
        // the default event-module kind, so an adapter must ship a
        // module.toml that declares the venue-adapter kind explicitly.
        let kind = loaded_manifest.manifest.module.kind;
        if kind != ModuleKind::VenueAdapter {
            return Err(anyhow!(
                "adapter {} declares module kind {kind:?}; an [[adapters]] entry requires \
                 a module.toml with [module] kind = \"venue-adapter\"",
                entry.path.display(),
            ));
        }

        info!(component = %entry.path.display(), "compiling adapter component");
        let component = Component::from_file(engine, &entry.path)
            .map_err(Error::from)
            .with_context(|| format!("compile {}", entry.path.display()))?;

        // Enforce the scoped-transport capability set: `registry` is the
        // adapter registry, so a declaration of any core-only interface
        // fails at manifest load, and an undeclared transport import fails
        // here. The linker withholds the same core-only interfaces, so an
        // adapter reaching for one also fails to instantiate below.
        manifest::enforce_capabilities(
            &loaded_manifest,
            component.component_type().imports(engine).map(|(n, _)| n),
            registry,
        )
        .with_context(|| format!("capability violation in {}", entry.path.display()))?;

        let adapter_namespace = if loaded_manifest.manifest.module.name.is_empty() {
            "adapter".to_owned()
        } else {
            loaded_manifest.manifest.module.name.clone()
        };
        info!(
            adapter = %adapter_namespace,
            fuel = limits_cfg.fuel(),
            memory_bytes = limits_cfg.memory(),
            http_allow = entry.http_allow.len(),
            messaging_topics = entry.messaging_topics.len(),
            "applied adapter resource limits and transport scope",
        );

        let run = RunId::new(adapter_namespace.clone(), 0);
        // An adapter store cannot call pool, so it carries an empty router;
        // this also keeps the real router out of the adapter's `HostState`,
        // so there is no reference cycle back into the router that owns it.
        let mut store = Self::build_store(
            engine,
            components,
            run.clone(),
            entry.http_allow.clone(),
            limits_cfg.http(),
            entry.messaging_topics.clone(),
            limits_cfg.memory(),
            limits_cfg.fuel(),
            clocks,
            PoolRouter::empty(),
        )?;
        let bindings = VenueAdapter::instantiate_async(&mut store, &component, linker)
            .await
            .map_err(Error::from)
            .with_context(|| format!("instantiate {}", entry.path.display()))?;

        let config: Config = if loaded_manifest.config.is_empty() {
            vec![("name".into(), adapter_namespace.clone())]
        } else {
            loaded_manifest.config.clone()
        };
        let init_succeeded = match bindings
            .call_init(&mut store, &config)
            .await
            .map_err(Error::from)?
        {
            Ok(()) => {
                info!(adapter = %adapter_namespace, "adapter init succeeded");
                true
            }
            Err(e) => {
                warn!(
                    adapter = %adapter_namespace,
                    kind = crate::host::error::fault_label(&e),
                    message = crate::host::error::fault_message(&e),
                    "adapter init failed - loaded but marked dead",
                );
                false
            }
        };
        // Refuel after init so the first routed call starts with a full budget.
        store.set_fuel(limits_cfg.fuel())?;

        Ok(LoadedAdapter {
            venue_id: adapter_namespace,
            actor: AdapterActor::new(store, bindings, limits_cfg.fuel()),
            alive: init_succeeded,
        })
    }

    /// Number of modules currently loaded.
    pub fn module_count(&self) -> usize {
        self.modules.len()
    }

    /// Number of venue adapters loaded at boot, alive or not.
    pub fn adapter_count(&self) -> usize {
        self.adapters_total
    }

    /// Number of adapters whose `init` succeeded and that are installed in the
    /// pool router for routing.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn adapter_alive_count(&self) -> usize {
        self.adapters_alive
    }

    /// Chains any module asked for block events on. The caller opens
    /// one shared block subscription per chain and routes through
    /// `dispatch_block`. Sorted by numeric id and deduped (`Chain` is
    /// not `Ord`, so this is not a `BTreeSet`).
    pub fn block_chains(&self) -> Vec<Chain> {
        let mut out: Vec<Chain> = Vec::new();
        for module in &self.modules {
            for sub in &module.subscriptions {
                if let Subscription::Block { chain_id } = sub {
                    out.push(Chain::from_id(*chain_id));
                }
            }
        }
        out.sort_by_key(|c| c.id());
        out.dedup();
        out
    }

    /// Per-module chain-log subscriptions. Each entry is a `(module_name,
    /// chain, filter)` triple the event loop opens against the
    /// matching alloy provider; the resulting stream tags every log
    /// with `module_name` so `dispatch_chain_log` routes correctly.
    pub fn chain_log_subscriptions(&self) -> Vec<(String, Chain, alloy_rpc_types_eth::Filter)> {
        let mut out = Vec::new();
        for module in &self.modules {
            for sub in &module.subscriptions {
                if let Subscription::ChainLog {
                    chain_id,
                    address,
                    event_signature,
                } = sub
                {
                    match build_alloy_filter(address.as_deref(), event_signature.as_deref()) {
                        Ok(filter) => {
                            out.push((module.name.clone(), Chain::from_id(*chain_id), filter))
                        }
                        Err(err) => warn!(
                            module = %module.name,
                            chain_id,
                            error = %err,
                            "invalid chain-log subscription - skipping",
                        ),
                    }
                }
            }
        }
        out
    }

    /// Dispatch a block event to every module subscribed to
    /// `block.chain_id`. Returns the number of modules invoked.
    /// Modules that trap are marked dead and excluded from future dispatch.
    /// Rebuild a module from its cached `Component` + `init_config`
    /// after a wasmtime trap. A trap leaves the original
    /// `Store` + component instance in a poisoned state ("cannot
    /// enter component instance" on the next call); the only way to
    /// recover is to create a fresh `Store` + re-instantiate. The
    /// `LoadedModule.subscriptions` and `LoadedModule.name` are
    /// preserved so the dispatch routing keeps working.
    ///
    /// On success the module's `alive` flag is left for the caller
    /// to flip; on failure (e.g. `init` returns Err again) the
    /// module stays dead and the failure_count keeps climbing.
    async fn reinstantiate_one(&mut self, idx: usize) -> Result<()> {
        // Re-build the linker: core interfaces plus every extension hook,
        // identical to the boot-time linker. Cheap `add_to_linker` calls
        // against the cached `Engine`.
        let linker = build_linker::<T>(&self.engine, &self.extensions)?;

        // Borrowed before the `&mut self.modules[idx]` reborrow so the restart
        // path applies the same clock override and the same shared pool router
        // as the initial boot.
        let clocks = self.clocks.clone();
        let pool_router = self.pool_router.clone();
        let module = &mut self.modules[idx];
        // A restart is a new run: bump the sequence so its logs key
        // apart from the dead run's, which stays readable until evicted.
        let run = RunId::new(module.name.clone(), module.run.seq + 1);
        let mut store = Self::build_store(
            &self.engine,
            &self.components,
            run.clone(),
            module.http_allowlist.clone(),
            module.http_limits,
            Vec::new(),
            module.memory_limit,
            module.fuel_per_event,
            clocks.as_ref(),
            pool_router,
        )?;
        let bindings = EventModule::instantiate_async(&mut store, &module.component, &linker)
            .await
            .map_err(Error::from)
            .with_context(|| format!("reinstantiate {}", module.name))?;
        match bindings.call_init(&mut store, &module.init_config).await? {
            Ok(()) => {}
            Err(e) => {
                return Err(anyhow!(
                    "init returned fault on restart: {} ({})",
                    crate::host::error::fault_message(&e),
                    crate::host::error::fault_label(&e),
                ));
            }
        }
        module.bindings = bindings;
        module.store = store;
        module.run = run;
        Ok(())
    }

    pub async fn dispatch_block(&mut self, block: nexum::host::types::Block) -> usize {
        let chain = Chain::from_id(block.chain_id);
        let chain_id = chain.id();
        let block_number = block.number;
        let event = nexum::host::types::Event::Block(block);
        let now = std::time::Instant::now();
        // Hoist the local-store reference out so the per-module
        // borrow checker is happy when we write the progress
        // marker after a successful dispatch.
        let local_store = self.components.store.clone();

        // Phase 1: find dead modules whose backoff window
        // has elapsed and re-instantiate them in place. The wasmtime
        // store + component instance left by a trap is poisoned
        // ("cannot enter component instance" on the next call), so
        // recovery requires a fresh Store + re-instantiated bindings.
        //
        // Poisoned modules are excluded from the restart
        // sweep entirely. Once quarantined they stay dead until
        // an operator removes them from `engine.toml::[[modules]]`
        // and restarts the engine.
        let restart_candidates: Vec<usize> = (0..self.modules.len())
            .filter(|&i| {
                let m = &self.modules[i];
                !m.poisoned && !m.alive && m.next_attempt.is_some_and(|t| t <= now)
            })
            .collect();
        for idx in restart_candidates {
            self.try_restart(idx).await;
        }

        let mut dispatched = 0;
        let candidate_indices: Vec<usize> = (0..self.modules.len())
            .filter(|&i| {
                let m = &self.modules[i];
                if m.poisoned || !m.alive {
                    return false;
                }
                m.subscriptions
                    .iter()
                    .any(|s| matches!(s, Subscription::Block { chain_id: cid } if chain == *cid))
            })
            .collect();
        for idx in candidate_indices {
            if matches!(
                self.dispatch_to(idx, chain_id, "block", block_number, &event)
                    .await,
                DispatchOutcome::Ok,
            ) {
                // Persist the per-module-per-chain progress
                // marker so a graceful restart (or even a crash)
                // leaves a paper trail. Writes failure is best-
                // effort; a warn is enough.
                let module_name = self.modules[idx].name.clone();
                let key = progress_key(chain);
                match local_store.module(&module_name) {
                    Ok(ms) => {
                        if let Err(e) = ms.set(&key, &block_number.to_le_bytes()) {
                            warn!(
                                module = %module_name,
                                chain_id,
                                error = %e,
                                "failed to persist last_dispatched_block marker",
                            );
                        }
                    }
                    Err(e) => {
                        warn!(
                            module = %module_name,
                            chain_id,
                            error = %e,
                            "failed to open module store for progress marker",
                        );
                    }
                }
                dispatched += 1;
            }
        }
        dispatched
    }

    /// Dispatch a chain-log event to the specific module that opened the
    /// subscription. Returns `true` when the module accepted the dispatch;
    /// `false` when the module is dead, not found, or its callback failed.
    /// A trapping module is marked dead and excluded from future dispatch.
    pub async fn dispatch_chain_log(
        &mut self,
        module_name: &str,
        chain: Chain,
        log: alloy_rpc_types_eth::Log,
    ) -> bool {
        let now = std::time::Instant::now();
        let Some(idx) = self.modules.iter().position(|m| m.name == module_name) else {
            warn!(module = %module_name, "no such module - dropping chain-log");
            return false;
        };

        // Poison-pill: quarantined modules get no chain-log
        // dispatches at all - same as block. The check happens
        // before the restart sweep so a poisoned module never
        // triggers a restart attempt.
        if self.modules[idx].poisoned {
            return false;
        }

        // Restart-on-trap: re-instantiate before dispatch
        // if the backoff window elapsed. See `dispatch_block` for
        // the symmetric path.
        let needs_restart = {
            let m = &self.modules[idx];
            !m.alive && m.next_attempt.is_some_and(|t| t <= now)
        };
        if needs_restart {
            self.try_restart(idx).await;
        }

        if !self.modules[idx].alive {
            return false;
        }

        let block_number = log.block_number.unwrap_or_default();
        let event = nexum::host::types::Event::ChainLogs(nexum::host::types::ChainLogs {
            chain_id: chain.id(),
            logs: vec![nexum::host::types::ChainLog::from(&log)],
        });
        matches!(
            self.dispatch_to(idx, chain.id(), "chain-log", block_number, &event)
                .await,
            DispatchOutcome::Ok,
        )
    }

    /// Dispatch a router-observed intent status transition to every module
    /// subscribed to `intent-status` events whose venue filter admits the
    /// update's venue. Returns the number of modules invoked. Mirrors
    /// `dispatch_block`: dead modules past their backoff are restarted
    /// first, poisoned modules are skipped.
    pub async fn dispatch_intent_status(
        &mut self,
        update: nexum::host::types::IntentStatusUpdate,
    ) -> usize {
        let now = std::time::Instant::now();
        let restart_candidates: Vec<usize> = (0..self.modules.len())
            .filter(|&i| {
                let m = &self.modules[i];
                !m.poisoned && !m.alive && m.next_attempt.is_some_and(|t| t <= now)
            })
            .collect();
        for idx in restart_candidates {
            self.try_restart(idx).await;
        }

        let candidate_indices: Vec<usize> = (0..self.modules.len())
            .filter(|&i| {
                let m = &self.modules[i];
                if m.poisoned || !m.alive {
                    return false;
                }
                m.subscriptions.iter().any(|s| {
                    matches!(
                        s,
                        Subscription::IntentStatus { venue }
                            if venue.as_deref().is_none_or(|v| v == update.venue)
                    )
                })
            })
            .collect();
        let event = nexum::host::types::Event::IntentStatus(update);
        let mut dispatched = 0;
        for idx in candidate_indices {
            // Status transitions are venue-scoped, not chain-scoped: the
            // telemetry chain id and block number carry the 0 sentinel.
            if matches!(
                self.dispatch_to(idx, 0, "intent-status", 0, &event).await,
                DispatchOutcome::Ok,
            ) {
                dispatched += 1;
            }
        }
        dispatched
    }

    /// Whether any loaded module subscribes to `intent-status` events.
    /// The launcher polls adapter statuses only when this holds: with no
    /// subscriber every transition would be dropped on arrival.
    pub fn has_intent_status_subscribers(&self) -> bool {
        self.modules.iter().any(|m| {
            m.subscriptions
                .iter()
                .any(|s| matches!(s, Subscription::IntentStatus { .. }))
        })
    }

    /// The shared intent pool router carried by every module store.
    pub fn pool_router(&self) -> PoolRouter {
        self.pool_router.clone()
    }

    /// Shared per-module dispatch path: refuel, call `on_event`, and
    /// process the three outcomes (ok / fault / trap) with the
    /// same telemetry + lifecycle bookkeeping. Returns whether the
    /// guest call succeeded; the caller layers any path-specific
    /// follow-up (e.g. the progress marker on `dispatch_block`).
    /// `chain_id` is telemetry only; chain-less event kinds pass 0.
    async fn dispatch_to(
        &mut self,
        idx: usize,
        chain_id: u64,
        event_kind: &'static str,
        block_number: u64,
        event: &nexum::host::types::Event,
    ) -> DispatchOutcome {
        let poison_policy = self.poison_policy;
        // Hoisted before the per-module borrow so the trap arm can
        // synthesize a panic record without re-borrowing `self`.
        let router = self.components.logs.router();
        let module = &mut self.modules[idx];
        if let Err(e) = module.store.set_fuel(module.fuel_per_event) {
            error!(
                module = %module.name,
                chain_id,
                event_kind,
                error = %e,
                "set_fuel failed - skipping"
            );
            return DispatchOutcome::Skipped;
        }
        let start = std::time::Instant::now();
        match module
            .bindings
            .call_on_event(&mut module.store, event)
            .await
        {
            Ok(Ok(())) => {
                let elapsed = start.elapsed();
                let latency_ms = elapsed.as_millis() as u64;
                debug!(
                    module = %module.name,
                    chain_id,
                    event_kind,
                    block_number,
                    latency_ms,
                    "dispatch ok"
                );
                metrics::histogram!(
                    "shepherd_event_latency_seconds",
                    "module" => module.name.clone(),
                    "event_kind" => event_kind,
                )
                .record(elapsed.as_secs_f64());
                // Successful dispatch clears the failure
                // history. A module that recovered after N traps
                // lands back in the steady-state schedule with no
                // further delay.
                module.failure_count = 0;
                module.next_attempt = None;
                DispatchOutcome::Ok
            }
            Ok(Err(fault)) => {
                let elapsed = start.elapsed();
                let latency_ms = elapsed.as_millis() as u64;
                let kind = crate::host::error::fault_label(&fault);
                warn!(
                    module = %module.name,
                    chain_id,
                    event_kind,
                    block_number,
                    latency_ms,
                    kind,
                    message = crate::host::error::fault_message(&fault),
                    "on-event returned fault",
                );
                metrics::counter!(
                    "shepherd_module_errors_total",
                    "module" => module.name.clone(),
                    "error_kind" => kind,
                )
                .increment(1);
                DispatchOutcome::Fault
            }
            Err(trap) => {
                let elapsed = start.elapsed();
                let latency_ms = elapsed.as_millis() as u64;
                module.failure_count = module.failure_count.saturating_add(1);
                let backoff = crate::runtime::restart_policy::backoff_for(module.failure_count);
                let next_attempt = std::time::Instant::now() + backoff;
                error!(
                    module = %module.name,
                    chain_id,
                    event_kind,
                    block_number,
                    latency_ms,
                    failure_count = module.failure_count,
                    backoff_ms = backoff.as_millis() as u64,
                    error = %trap,
                    "on-event trapped - module marked dead; will retry after backoff",
                );
                metrics::counter!(
                    "shepherd_module_errors_total",
                    "module" => module.name.clone(),
                    "error_kind" => "trap",
                )
                .increment(1);
                module.alive = false;
                module.next_attempt = Some(next_attempt);
                // Death diagnosis: leave a retrievable panic record on the
                // dead run so an operator sees why it terminated even
                // after the store is torn down. The record carries the
                // trap's root cause only; the full trap with its wasm
                // frame list already went to host tracing above.
                router.record(LogRecord::now(
                    module.run.clone(),
                    LogSource::Panic,
                    Level::ERROR,
                    format!("run terminated abnormally: {}", trap.root_cause()),
                ));
                record_failure_and_maybe_poison(module, poison_policy, &trap.to_string());
                DispatchOutcome::Trapped
            }
        }
    }

    /// Attempt to re-instantiate a dead module in place. On success
    /// the module is marked `alive`; on failure the failure counter
    /// is bumped and `next_attempt` slides further out per the
    /// restart-policy backoff. Used by both dispatch paths.
    async fn try_restart(&mut self, idx: usize) {
        let name = self.modules[idx].name.clone();
        let failure_count = self.modules[idx].failure_count;
        info!(module = %name, failure_count, "restart attempt");
        metrics::counter!(
            "shepherd_module_restarts_total",
            "module" => name.clone(),
        )
        .increment(1);
        match self.reinstantiate_one(idx).await {
            Ok(()) => {
                self.modules[idx].alive = true;
                info!(module = %name, "restart succeeded");
            }
            Err(e) => {
                // Re-instantiation failed: bump the backoff again so
                // the next attempt is further out.
                let m = &mut self.modules[idx];
                m.failure_count = m.failure_count.saturating_add(1);
                let backoff = crate::runtime::restart_policy::backoff_for(m.failure_count);
                m.next_attempt = Some(std::time::Instant::now() + backoff);
                error!(
                    module = %name,
                    failure_count = m.failure_count,
                    backoff_ms = backoff.as_millis() as u64,
                    error = %e,
                    "restart failed - will retry after backoff",
                );
            }
        }
    }

    /// Count of modules currently alive (not dead due to traps).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn alive_count(&self) -> usize {
        self.modules.iter().filter(|m| m.alive).count()
    }

    /// Also expose a per-module poisoned state for
    /// metrics + integration tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn poisoned_count(&self) -> usize {
        self.modules.iter().filter(|m| m.poisoned).count()
    }
}

/// Build a `Linker` binding the core `event-module` interfaces plus every
/// extension's own interfaces for `HostState<T>`. Shared by the supervisor
/// restart path and the bootstrap launch path.
///
/// Extension hooks run after the core interfaces. A module that imports an
/// extension interface instantiates only if that extension's hook is
/// present here, so the same `extensions` slice must drive both this linker
/// and capability enforcement via the crate-internal `capability_registry`.
pub fn build_linker<T: RuntimeTypes>(
    engine: &Engine,
    extensions: &[Extension<T>],
) -> anyhow::Result<Linker<HostState<T>>> {
    let mut linker = Linker::<HostState<T>>::new(engine);
    EventModule::add_to_linker::<HostState<T>, HasSelf<HostState<T>>>(&mut linker, |state| state)?;
    // The intent pool import is linked into every module linker; it dispatches
    // to the shared router carried in each store's `HostState`. Modules that do
    // not import it are unaffected.
    crate::bindings::pool::add_to_linker::<HostState<T>, HasSelf<HostState<T>>>(
        &mut linker,
        |state| state,
    )?;
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    // wasi:http only; the p2 call above already covers the shared
    // wasi:io/wasi:clocks interfaces.
    wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)?;
    for ext in extensions {
        (ext.link)(&mut linker)?;
    }
    Ok(linker)
}

/// Build a `Linker` for the `venue-adapter` world: only the scoped
/// transport an adapter may reach - `chain`, `messaging`, and the
/// allowlisted `wasi:http` - plus the ambient WASI base. The core
/// `nexum:host` interfaces an adapter must not touch (local-store,
/// remote-store, identity, logging) are deliberately withheld, so an
/// adapter that imports one of them fails to instantiate rather than
/// silently gaining reach. Extensions are not linked into adapters: an
/// adapter speaks its venue's protocol over the standard transport, not a
/// domain extension surface.
pub fn build_adapter_linker<T: RuntimeTypes>(
    engine: &Engine,
) -> anyhow::Result<Linker<HostState<T>>> {
    let mut linker = Linker::<HostState<T>>::new(engine);
    nexum::host::chain::add_to_linker::<HostState<T>, HasSelf<HostState<T>>>(
        &mut linker,
        |state| state,
    )?;
    nexum::host::messaging::add_to_linker::<HostState<T>, HasSelf<HostState<T>>>(
        &mut linker,
        |state| state,
    )?;
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)?;
    Ok(linker)
}

/// Resolve a component's manifest path: the explicit `manifest` override
/// wins, else a sibling `module.toml`, else the deprecated `nexum.toml`
/// with a rename warning. `None` when neither sibling exists. Shared by the
/// module and adapter load paths.
fn resolve_manifest_path(component: &Path, explicit: Option<&Path>) -> Option<std::path::PathBuf> {
    if let Some(path) = explicit {
        return Some(path.to_path_buf());
    }
    // Canonical name is module.toml (ADR-0001). nexum.toml is accepted
    // with a deprecation warning during the 0.1->0.2 transition.
    let dir = component.parent()?.to_owned();
    let canonical = dir.join("module.toml");
    if canonical.exists() {
        return Some(canonical);
    }
    let legacy = dir.join("nexum.toml");
    if legacy.exists() {
        warn!(
            target: "manifest",
            path = %legacy.display(),
            "nexum.toml is deprecated; rename to module.toml \
             (ADR-0001). Support will be removed in 0.3."
        );
        return Some(legacy);
    }
    None
}

/// Assemble the capability registry from the core namespace plus every
/// extension's namespace. The result must agree with the linker built from
/// the same `extensions`: enforcement recognises an extension import as a
/// declared capability only when its namespace is registered here.
pub(crate) fn capability_registry<T: RuntimeTypes>(
    extensions: &[Extension<T>],
) -> CapabilityRegistry {
    let mut registry = CapabilityRegistry::core();
    for ext in extensions {
        registry.register(ext.capabilities);
    }
    registry
}

/// Outcome of [`Supervisor::dispatch_to`] for a single module.
///
/// Returned to the caller so path-specific follow-ups (e.g. the
/// progress marker on the block path) can branch on whether
/// the guest actually ran cleanly. Kept private; only the two
/// `dispatch_*` entry points consume it.
#[derive(Debug, Eq, PartialEq)]
enum DispatchOutcome {
    /// Guest returned `Ok(())`.
    Ok,
    /// Guest returned a typed `fault` via WIT.
    Fault,
    /// Guest trapped (panic / OOM / fuel exhaustion / etc.). Module
    /// has been marked dead and may be quarantined per the
    /// poison-policy.
    Trapped,
    /// `set_fuel` failed before the call. Module is left alive but
    /// this event is skipped.
    Skipped,
}

/// Push the current trap timestamp into the module's
/// failure-window ring, drop entries older than the policy window,
/// and flip `poisoned = true` once the window holds more than
/// `policy.max_failures` traps. The first transition emits the
/// `shepherd_module_poisoned` gauge + a structured WARN.
fn record_failure_and_maybe_poison<T: RuntimeTypes>(
    module: &mut LoadedModule<T>,
    policy: crate::runtime::poison_policy::PoisonPolicy,
    last_error: &str,
) {
    let now = std::time::Instant::now();
    // Prune entries outside the window.
    while let Some(&front) = module.failure_timestamps.front() {
        if now.duration_since(front) > policy.window {
            module.failure_timestamps.pop_front();
        } else {
            break;
        }
    }
    module.failure_timestamps.push_back(now);
    let recent = module.failure_timestamps.len() as u32;
    if crate::runtime::poison_policy::should_poison(policy, recent) && !module.poisoned {
        module.poisoned = true;
        warn!(
            module = %module.name,
            recent_failures = recent,
            window_secs = policy.window.as_secs(),
            last_error,
            "module poisoned - quarantined; remove from engine.toml + restart to clear",
        );
        metrics::gauge!(
            "shepherd_module_poisoned",
            "module" => module.name.clone(),
        )
        .set(1.0);
    }
}

/// Persisted per-chain progress key; must stay numeric for data compat.
fn progress_key(chain: Chain) -> String {
    format!("last_dispatched_block:{}", chain.id())
}

impl From<&alloy_rpc_types_eth::Log> for nexum::host::types::ChainLog {
    /// Project an alloy `Log` onto the WIT `chain-log` record, preserving every
    /// RPC field so the guest reconstructs the alloy log without loss. The chain
    /// id is not on the alloy log; the subscription context supplies it at the
    /// `chain-logs` batch level.
    fn from(log: &alloy_rpc_types_eth::Log) -> Self {
        Self {
            address: log.address().as_slice().to_vec(),
            topics: log.topics().iter().map(|t| t.as_slice().to_vec()).collect(),
            data: log.inner.data.data.to_vec(),
            block_hash: log.block_hash.map(|h| h.as_slice().to_vec()),
            block_number: log.block_number,
            block_timestamp: log.block_timestamp,
            transaction_hash: log.transaction_hash.map(|h| h.as_slice().to_vec()),
            transaction_index: log.transaction_index,
            log_index: log.log_index,
            removed: log.removed,
        }
    }
}

/// Errors surfaced by [`build_alloy_filter`].
///
/// Variants thread the underlying alloy parse error via `#[source]`
/// instead of `to_string()`-ing it - keeps the typed chain intact for
/// the supervisor's `tracing::warn!(error = %err, ...)` log line at
/// the call site (where the `Display` chain prints the parse detail).
///
/// `IntoStaticStr` exposes the snake_case variant name as a
/// `&'static str` so the warn log can carry
/// `error_kind = address | topic` without a match-ladder.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
enum FilterError {
    /// `[[subscriptions]].address` did not parse as an EVM address.
    #[error("invalid chain-log address {address:?}: {source}")]
    Address {
        /// Raw operator-supplied hex string.
        address: String,
        /// Underlying alloy parse failure.
        #[source]
        source: alloy_primitives::hex::FromHexError,
    },
    /// `[[subscriptions]].event_signature` did not parse as a 32-byte topic.
    #[error("invalid topic {topic:?}: {source}")]
    Topic {
        /// Raw operator-supplied hex string.
        topic: String,
        /// Underlying alloy parse failure.
        #[source]
        source: alloy_primitives::hex::FromHexError,
    },
}

/// Translate a `[[subscription]]` chain-log entry into an alloy `Filter`.
fn build_alloy_filter(
    address: Option<&str>,
    event_signature: Option<&str>,
) -> std::result::Result<alloy_rpc_types_eth::Filter, FilterError> {
    use alloy_primitives::{Address, B256};
    let mut filter = alloy_rpc_types_eth::Filter::new();
    if let Some(addr_hex) = address {
        let addr: Address = addr_hex.parse().map_err(|source| FilterError::Address {
            address: addr_hex.to_owned(),
            source,
        })?;
        filter = filter.address(addr);
    }
    if let Some(topic_hex) = event_signature {
        let topic: B256 = topic_hex.parse().map_err(|source| FilterError::Topic {
            topic: topic_hex.to_owned(),
            source,
        })?;
        filter = filter.event_signature(topic);
    }
    Ok(filter)
}

#[cfg(test)]
mod tests;
