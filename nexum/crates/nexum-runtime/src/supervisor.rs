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
//! Providers ride the same sweeps: a trap inside a routed call flips
//! the [`Liveness`] their actor shares with the supervisor, the owning
//! service reports the instance unavailable while dead, and the sweep
//! reinstalls the provider after the same backoff and poison policies.
//!
//! Multi-chain isolation: `dispatch_block(block)` walks
//! every module but only enters those whose subscriptions match
//! `block.chain_id`. Per-module restart / poison / fuel limits are
//! independent across chains, so a poisoned module on chain A
//! cannot starve modules on chain B. The upstream WS reconnect
//! tasks own one per-chain backoff timer each, so a
//! chain-A connection drop does not block chain-B events.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use alloy_chains::Chain;
use anyhow::{Context, Error, Result, anyhow};
use tracing::{debug, error, info, warn};
use tracing_core::Level;
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{HostMonotonicClock, HostWallClock, WasiCtxBuilder};

use crate::bindings::{Config, EventModule, nexum};
use crate::engine_config::{
    AdapterEntry, EngineConfig, ModuleEntry, ModuleLimits, OutboundHttpLimits,
};
use crate::host::actor::Liveness;
use crate::host::component::{Components, RuntimeTypes, StateHandle, StateStore};
use crate::host::extension::ExtensionEvent;
use crate::host::extension::{
    Extension, HostService, HostServices, Installed, ProviderInstance, ProviderKind,
    ProviderManifest,
};
use crate::host::http::HttpGate;
#[cfg(test)]
use crate::host::local_store_redb::LocalStore;
use crate::host::logs::{LogRecord, LogSource, RunId, StdioStream};
#[cfg(test)]
use crate::host::provider_pool::ProviderPool;
use crate::host::state::HostState;
use crate::manifest::{
    self, CapabilityRegistry, ComponentKind, LoadedManifest, ResourceSection, Subscription,
};

/// Owns every loaded module and exposes the dispatch surface the
/// event loop needs. Generic over the [`RuntimeTypes`] lattice binding
/// the component seam backends.
pub struct Supervisor<T: RuntimeTypes> {
    modules: Vec<LoadedModule<T>>,
    /// Providers loaded at boot, whether or not `init` succeeded. Swept
    /// for restart and poison alongside the modules.
    providers: Vec<LoadedProvider>,
    /// Registered provider kinds paired with their services, kept for the
    /// provider restart sweep to reinstall through.
    kinds: ProviderKinds<T>,
    /// Cached for module restart: re-instantiating a trapped module
    /// requires a fresh wasmtime `Store` + `Linker`, which in turn need
    /// the shared backends. The `Components` bundle is cheaply cloned
    /// (Arc-backed members) so the supervisor takes an owned copy at boot.
    engine: Engine,
    components: Components<T>,
    /// Extensions wired at boot. Cached so the module-restart path can
    /// rebuild an identical linker (core interfaces plus every extension
    /// hook) without re-consulting the composition root.
    extensions: Vec<Arc<dyn Extension<T>>>,
    /// Extension-owned host services, built once at boot from the same
    /// extension set and carried by every store.
    services: HostServices,
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
impl crate::sealed::SealedRuntimeTypes for TestTypes {}

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

/// A module's resource budget after layering its `[module.resources]`
/// overrides onto the engine `[limits]` defaults.
struct ResolvedLimits {
    fuel: u64,
    memory: usize,
    state_bytes: u64,
}

/// Layer a manifest's `[module.resources]` over the engine `[limits]`
/// defaults: each unset override field keeps the engine default.
fn resolve_module_limits(res: &ResourceSection, cfg: &ModuleLimits) -> ResolvedLimits {
    ResolvedLimits {
        fuel: res.max_fuel_per_event.unwrap_or(cfg.fuel()),
        memory: res.max_memory_bytes.unwrap_or(cfg.memory()),
        state_bytes: res.max_state_bytes.unwrap_or(cfg.state_bytes()),
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
    /// Wall-clock deadline for a whole dispatch, guest plus every host
    /// call it awaits. Fuel bounds only guest instructions, so this is
    /// the backstop against a dispatch parked in a slow or blocked host
    /// call (see [`crate::runtime::limits`]).
    event_deadline: Duration,
    /// Memory cap applied to the wasmtime store on reinstantiation.
    memory_limit: usize,
    /// Local-store byte quota applied to the module store on reinstantiation.
    local_store_bytes: u64,
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
    /// Cached for restart: chain response size cap baked into the
    /// `HostState` we rebuild on each re-instantiation.
    chain_response_max_bytes: usize,
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
    /// Per-module dispatch rate limiter. Checked in `dispatch_to`
    /// before the guest runs, so an event flood on this module's
    /// source is throttled at the dispatch boundary without touching
    /// any other module's bucket. Over-rate events are dropped and
    /// counted.
    dispatch_bucket: crate::runtime::dispatch_rate::TokenBucket,
}

/// One loaded provider. Mirrors [`LoadedModule`]'s restart and poison
/// bookkeeping; liveness is shared with the installed actor, which marks
/// it dead on a trap, and read back by the sweep.
struct LoadedProvider {
    /// The provider's namespace: its manifest name, and the id its kind
    /// installs it under.
    name: String,
    /// Registered kind spelling the restart sweep reinstalls through.
    kind: &'static str,
    /// Extension-owned manifest sections, as the worker install
    /// predicates see them.
    sections: manifest::ExtensionSections,
    /// Cached for restart, like a module's.
    component: Component,
    /// Cached for restart: the manifest `[config]` handed to `init`.
    init_config: Config,
    /// Cached for restart: the operator's transport grants.
    http_allow: Vec<String>,
    messaging_topics: Vec<String>,
    /// Cached for restart: the engine `[limits]` applied at boot.
    http_limits: OutboundHttpLimits,
    fuel_per_call: u64,
    memory_limit: usize,
    chain_response_max_bytes: usize,
    local_store_bytes: u64,
    /// Trap flag shared with the installed actor.
    liveness: Liveness,
    /// Sequence of the run currently installed; restarts increment it.
    run_seq: u64,
    /// The sweep's view of `liveness`: a `true` here against a dead
    /// liveness is an unrecorded trap. Boot init failure leaves it `false`
    /// with `next_attempt = None`, permanent like a module's.
    alive: bool,
    failure_count: u32,
    next_attempt: Option<std::time::Instant>,
    failure_timestamps: std::collections::VecDeque<std::time::Instant>,
    poisoned: bool,
}

/// One registered provider kind paired with the service its installs bind to.
type ProviderRow<T> = (Box<dyn ProviderKind<T>>, Arc<dyn HostService>);

/// Registered provider kinds, keyed by their manifest spelling.
type ProviderKinds<T> = BTreeMap<&'static str, ProviderRow<T>>;

/// Collect each extension's provider kind paired with that extension's
/// service. Refuses a duplicate spelling and a provider whose extension
/// owns no service to install into.
fn provider_kinds<T: RuntimeTypes>(
    extensions: &[Arc<dyn Extension<T>>],
    services: &HostServices,
) -> Result<ProviderKinds<T>> {
    let mut kinds = ProviderKinds::new();
    for ext in extensions {
        let Some(provider) = ext.provider() else {
            continue;
        };
        let service = services.raw(ext.namespace()).cloned().ok_or_else(|| {
            anyhow!(
                "extension {} registers provider kind {} without a host service",
                ext.namespace(),
                provider.kind(),
            )
        })?;
        register_kind(&mut kinds, provider, service)?;
    }
    Ok(kinds)
}

/// The union of subscription kinds the wired extensions declare; a
/// manifest subscription of any other non-core kind fails the load.
fn extension_subscription_vocabulary<T: RuntimeTypes>(
    extensions: &[Arc<dyn Extension<T>>],
) -> BTreeSet<&'static str> {
    extensions
        .iter()
        .flat_map(|ext| ext.subscriptions().iter().copied())
        .collect()
}

/// Refuse a manifest section no wired extension claims, so a typo'd
/// section fails loudly instead of silently skipping its extension's
/// install predicate.
fn enforce_extension_sections<T: RuntimeTypes>(
    owner: &str,
    sections: &manifest::ExtensionSections,
    extensions: &[Arc<dyn Extension<T>>],
) -> Result<()> {
    for key in sections.keys() {
        let claimed = extensions
            .iter()
            .any(|ext| ext.manifest_sections().contains(&key.as_str()));
        if !claimed {
            return Err(anyhow!(
                "{owner} declares manifest section [{key}]; no wired extension claims it"
            ));
        }
    }
    Ok(())
}

/// Insert one kind row, refusing a duplicate manifest spelling.
fn register_kind<T: RuntimeTypes>(
    kinds: &mut ProviderKinds<T>,
    provider: Box<dyn ProviderKind<T>>,
    service: Arc<dyn HostService>,
) -> Result<()> {
    let kind = provider.kind();
    if kinds.insert(kind, (provider, service)).is_some() {
        return Err(anyhow!("provider kind {kind} is registered twice"));
    }
    Ok(())
}

/// Comma-joined registered provider kind spellings, for boot errors.
fn registered_kinds<T: RuntimeTypes>(kinds: &ProviderKinds<T>) -> String {
    kinds.keys().copied().collect::<Vec<_>>().join(", ")
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
        extensions: &[Arc<dyn Extension<T>>],
        clocks: Option<WasiClockOverride>,
    ) -> Result<Self> {
        let registry = capability_registry(extensions);
        let services = HostServices::from_extensions(extensions)?;
        // Provider kinds the boot loop resolves manifest kinds against.
        let kinds = provider_kinds(extensions, &services)?;
        // Providers boot first into their extension-owned services, so
        // every module store built below already routes to the installed
        // instances. Providers link only their kind's scoped imports.
        let provider_registry = CapabilityRegistry::provider();
        let mut providers = Vec::with_capacity(engine_cfg.adapters.len());
        for entry in &engine_cfg.adapters {
            let loaded = Self::load_provider(
                engine,
                entry,
                components,
                &engine_cfg.limits,
                &provider_registry,
                clocks.as_ref(),
                &kinds,
                extensions,
            )
            .await
            .with_context(|| format!("load provider {}", entry.path.display()))?;
            providers.push(loaded);
        }
        // The loaded providers' manifests, as the worker install
        // predicates see them.
        let provider_manifests: Vec<ProviderManifest> = providers
            .iter()
            .map(|p| ProviderManifest {
                name: p.name.clone(),
                kind: p.kind,
                sections: p.sections.clone(),
            })
            .collect();

        let extension_kinds = extension_subscription_vocabulary(extensions);
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
                services.clone(),
                &extension_kinds,
                extensions,
                &provider_manifests,
            )
            .await
            .with_context(|| format!("load module {}", entry.path.display()))?;
            modules.push(loaded);
        }
        let alive = modules.iter().filter(|m| m.alive).count();
        let adapters_alive = providers.iter().filter(|p| p.alive).count();
        info!(
            loaded = modules.len(),
            alive,
            adapters = providers.len(),
            adapters_alive,
            "supervisor up"
        );
        Ok(Self {
            modules,
            providers,
            kinds,
            engine: engine.clone(),
            components: components.clone(),
            extensions: extensions.to_vec(),
            services,
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
        extensions: &[Arc<dyn Extension<T>>],
        clocks: Option<WasiClockOverride>,
    ) -> Result<Self> {
        let registry = capability_registry(extensions);
        let services = HostServices::from_extensions(extensions)?;
        let entry = ModuleEntry {
            path: wasm.to_path_buf(),
            manifest: manifest.map(Path::to_path_buf),
        };
        // The single-module override path serves `just run`; providers
        // are configured through `engine.toml`, so none boot here.
        let extension_kinds = extension_subscription_vocabulary(extensions);
        let loaded = Self::load_one(
            engine,
            linker,
            &entry,
            components,
            limits,
            &registry,
            clocks.as_ref(),
            services.clone(),
            &extension_kinds,
            extensions,
            &[],
        )
        .await?;
        Ok(Self {
            modules: vec![loaded],
            providers: Vec::new(),
            kinds: ProviderKinds::new(),
            engine: engine.clone(),
            components: components.clone(),
            extensions: extensions.to_vec(),
            services,
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
        chain_response_max_bytes: usize,
        state_quota: u64,
        clocks: Option<&WasiClockOverride>,
        services: HostServices,
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
        // Intentionally no inherit_env: the guest environment stays empty, so
        // wasi:cli/environment leaks nothing of the host's.
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
            .map_err(|e| anyhow!("local-store namespace for {namespace}: {e}"))?
            .with_quota(state_quota);
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
                chain_response_max_bytes,
                store: module_store,
                remote: components.remote.clone(),
                services,
            },
        );
        store.limiter(|state| &mut state.limits);
        store.set_fuel(fuel)?;
        Ok(store)
    }

    // One flat argument per shared input threaded onto the store.
    #[allow(clippy::too_many_arguments)]
    async fn load_one(
        engine: &Engine,
        linker: &Linker<HostState<T>>,
        entry: &ModuleEntry,
        components: &Components<T>,
        limits_cfg: &ModuleLimits,
        registry: &CapabilityRegistry,
        clocks: Option<&WasiClockOverride>,
        services: HostServices,
        extension_kinds: &BTreeSet<&'static str>,
        extensions: &[Arc<dyn Extension<T>>],
        provider_manifests: &[ProviderManifest],
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
        let module_namespace = if loaded_manifest.manifest.module.name.is_empty() {
            "module".to_owned()
        } else {
            loaded_manifest.manifest.module.name.clone()
        };

        // Run the extension install predicates before any compile cost:
        // every section must be claimed, and every claiming extension
        // must admit the worker against the loaded providers' manifests.
        let sections = &loaded_manifest.manifest.extensions;
        enforce_extension_sections(&module_namespace, sections, extensions)?;
        for ext in extensions {
            ext.admit_worker(&module_namespace, sections, provider_manifests)
                .with_context(|| format!("install refused for {}", entry.path.display()))?;
        }

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
        // Layer the manifest's `[module.resources]` over the engine `[limits]`
        // defaults: an unset override field keeps the engine default.
        let ResolvedLimits {
            fuel,
            memory,
            state_bytes,
        } = resolve_module_limits(&loaded_manifest.manifest.module.resources, limits_cfg);
        info!(
            module = %module_namespace,
            fuel,
            memory_bytes = memory,
            state_bytes,
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
            // Event modules are unscoped for messaging; only providers
            // carry a topic grant.
            Vec::new(),
            memory,
            fuel,
            limits_cfg.chain_response_max_bytes(),
            state_bytes,
            clocks,
            services,
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
        // `init` runs guest code that may call host functions; bound it
        // in wall-clock like a dispatch so a hung host call during init
        // cannot park boot indefinitely. A deadline or trap propagates as
        // a load error.
        let init_outcome = with_dispatch_deadline(
            limits_cfg.event_deadline(),
            bindings.call_init(&mut store, &config),
        )
        .await
        .map_err(Error::from)?
        .map_err(Error::from)?;
        let init_succeeded = match init_outcome {
            Ok(()) => {
                info!(module = %module_namespace, "init succeeded");
                true
            }
            Err(e) => {
                warn!(
                    module = %module_namespace,
                    kind = crate::host::error::fault_label(&e),
                    message = %crate::host::error::fault_message(&e),
                    "init failed - module loaded but marked dead; dispatcher will skip it",
                );
                false
            }
        };
        // Refuel after init so the first on_event starts with a full budget.
        store.set_fuel(fuel)?;

        // Surface any `[[subscription]]` entries the host cannot
        // service yet, so an operator running 0.2 against a 0.3
        // manifest does not silently drop events, and refuse an
        // extension kind no wired extension declares.
        for sub in &loaded_manifest.manifest.subscriptions {
            match sub {
                Subscription::Cron { .. } => warn!(
                    module = %module_namespace,
                    "cron subscriptions are declared but inert in 0.2 (lands in 0.3)",
                ),
                Subscription::Extension { kind, .. }
                    if !extension_kinds.contains(kind.as_str()) =>
                {
                    return Err(anyhow!(
                        "module {module_namespace} subscribes to unknown event kind {kind}; \
                         no wired extension declares it"
                    ));
                }
                _ => {}
            }
        }

        Ok(LoadedModule {
            name: module_namespace,
            bindings,
            store,
            run,
            subscriptions: loaded_manifest.manifest.subscriptions.clone(),
            fuel_per_event: fuel,
            event_deadline: limits_cfg.event_deadline(),
            memory_limit: memory,
            local_store_bytes: state_bytes,
            alive: init_succeeded,
            failure_count: 0,
            next_attempt: None,
            component,
            init_config: config,
            http_allowlist: loaded_manifest.http_allowlist.clone(),
            http_limits: limits_cfg.http(),
            chain_response_max_bytes: limits_cfg.chain_response_max_bytes(),
            failure_timestamps: std::collections::VecDeque::new(),
            poisoned: false,
            dispatch_bucket: crate::runtime::dispatch_rate::TokenBucket::new(
                limits_cfg.dispatch_rate(),
                std::time::Instant::now(),
            ),
        })
    }

    /// Load one `[[adapters]]` entry: resolve its manifest, resolve the
    /// declared kind against the registered provider kinds, enforce the
    /// scoped-transport capability set, build a supervised store carrying
    /// the operator's HTTP and messaging grants, and hand the instance to
    /// its kind to instantiate and install. A failed guest `init` loads the
    /// provider dead and unroutable, permanently like a module's.
    // One flat argument per shared input threaded onto the store, matching
    // the module load path.
    #[allow(clippy::too_many_arguments)]
    async fn load_provider(
        engine: &Engine,
        entry: &AdapterEntry,
        components: &Components<T>,
        limits_cfg: &ModuleLimits,
        registry: &CapabilityRegistry,
        clocks: Option<&WasiClockOverride>,
        kinds: &ProviderKinds<T>,
        extensions: &[Arc<dyn Extension<T>>],
    ) -> Result<LoadedProvider> {
        let manifest_path = resolve_manifest_path(&entry.path, entry.manifest.as_deref());
        let loaded_manifest: LoadedManifest = match manifest_path.as_deref() {
            Some(p) if p.exists() => {
                info!(manifest = %p.display(), "loading provider manifest");
                manifest::load(p, registry)?
            }
            _ => {
                warn!(
                    component = %entry.path.display(),
                    "no module.toml - falling back to anonymous provider"
                );
                manifest::fallback_manifest()
            }
        };
        let namespace = if loaded_manifest.manifest.module.name.is_empty() {
            "provider".to_owned()
        } else {
            loaded_manifest.manifest.module.name.clone()
        };

        // Run the extension install predicates before any compile cost:
        // every section must be claimed, and every claiming extension
        // must admit the provider's own sections.
        let sections = loaded_manifest.manifest.extensions.clone();
        enforce_extension_sections(&namespace, &sections, extensions)?;
        for ext in extensions {
            ext.admit_provider(&namespace, &sections)
                .with_context(|| format!("install refused for {}", entry.path.display()))?;
        }

        // The manifest kind is the discriminator: an [[adapters]] entry
        // must name a registered provider kind, caught here before
        // instantiation. A fallback manifest has the default worker kind,
        // so a provider must ship a module.toml that declares its kind
        // explicitly.
        let (kind, service) = match &loaded_manifest.manifest.module.kind {
            ComponentKind::Worker => {
                return Err(anyhow!(
                    "{} declares the worker kind; an [[adapters]] entry requires a \
                     module.toml declaring a registered provider kind ({})",
                    entry.path.display(),
                    registered_kinds(kinds),
                ));
            }
            ComponentKind::Provider(spelling) => kinds.get(spelling.as_str()).ok_or_else(|| {
                anyhow!(
                    "{} declares unregistered provider kind {spelling}; registered \
                         kinds: {}",
                    entry.path.display(),
                    registered_kinds(kinds),
                )
            })?,
        };

        info!(
            component = %entry.path.display(),
            kind = kind.kind(),
            "compiling provider component",
        );
        let component = Component::from_file(engine, &entry.path)
            .map_err(Error::from)
            .with_context(|| format!("compile {}", entry.path.display()))?;

        // Enforce the scoped-transport capability set: `registry` is the
        // provider registry, so a declaration of any core-only interface
        // fails at manifest load, and an undeclared transport import fails
        // here. The linker withholds the same core-only interfaces, so a
        // provider reaching for one also fails to instantiate.
        manifest::enforce_capabilities(
            &loaded_manifest,
            component.component_type().imports(engine).map(|(n, _)| n),
            registry,
        )
        .with_context(|| format!("capability violation in {}", entry.path.display()))?;

        info!(
            provider = %namespace,
            kind = kind.kind(),
            fuel = limits_cfg.fuel(),
            memory_bytes = limits_cfg.memory(),
            http_allow = entry.http_allow.len(),
            messaging_topics = entry.messaging_topics.len(),
            "applied provider resource limits and transport scope",
        );

        let linker = build_provider_linker::<T>(engine, kind.as_ref())?;
        let run = RunId::new(namespace.clone(), 0);
        // A provider links no service-consuming import, so its store carries
        // an empty service map; the shared map holds the registry that owns
        // the provider's store, and carrying it here would cycle.
        let store = Self::build_store(
            engine,
            components,
            run,
            entry.http_allow.clone(),
            limits_cfg.http(),
            entry.messaging_topics.clone(),
            limits_cfg.memory(),
            limits_cfg.fuel(),
            limits_cfg.chain_response_max_bytes(),
            limits_cfg.state_bytes(),
            clocks,
            HostServices::default(),
        )?;

        let config: Config = if loaded_manifest.config.is_empty() {
            vec![("name".into(), namespace.clone())]
        } else {
            loaded_manifest.config.clone()
        };
        let liveness = Liveness::default();
        let installed = kind
            .install(
                ProviderInstance {
                    component: &component,
                    linker: &linker,
                    store,
                    config: config.clone(),
                    sections: &sections,
                    fuel_per_call: limits_cfg.fuel(),
                    liveness: liveness.clone(),
                },
                service,
            )
            .await
            .with_context(|| format!("install {}", entry.path.display()))?;
        if installed == Installed::Dead {
            liveness.mark_dead();
        }
        Ok(LoadedProvider {
            name: namespace,
            kind: kind.kind(),
            sections,
            component,
            init_config: config,
            http_allow: entry.http_allow.clone(),
            messaging_topics: entry.messaging_topics.clone(),
            http_limits: limits_cfg.http(),
            fuel_per_call: limits_cfg.fuel(),
            memory_limit: limits_cfg.memory(),
            chain_response_max_bytes: limits_cfg.chain_response_max_bytes(),
            local_store_bytes: limits_cfg.state_bytes(),
            liveness,
            run_seq: 0,
            alive: installed == Installed::Live,
            failure_count: 0,
            next_attempt: None,
            failure_timestamps: std::collections::VecDeque::new(),
            poisoned: false,
        })
    }

    /// Number of modules currently loaded.
    pub fn module_count(&self) -> usize {
        self.modules.len()
    }

    /// Number of providers loaded at boot, alive or not.
    pub fn adapter_count(&self) -> usize {
        self.providers.len()
    }

    /// Number of adapters currently alive and routable. Live: a trap drops
    /// it, the restart sweep raises it again.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn adapter_alive_count(&self) -> usize {
        self.providers
            .iter()
            .filter(|p| p.liveness.is_alive())
            .count()
    }

    /// Chains any **alive** module asked for block events on. Dead modules
    /// (init-failed or currently in trap-backoff) are excluded so the
    /// caller does not open live RPC subscriptions for chains with no
    /// reachable module. The caller opens one shared block subscription
    /// per chain and routes through `dispatch_block`. Sorted by numeric id
    /// and deduped (`Chain` is not `Ord`, so this is not a `BTreeSet`).
    pub fn block_chains(&self) -> Vec<Chain> {
        let mut out: Vec<Chain> = Vec::new();
        for module in self.modules.iter().filter(|m| m.alive) {
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

    /// Per-module chain-log subscriptions for **alive** modules only.
    /// Called once at launch, when a dead module can only mean init
    /// failure (trap-backoff cannot exist yet); excluding them keeps the
    /// caller from opening live log subscriptions no reachable module
    /// consumes. Each entry names the module, chain, and filter the event
    /// loop opens against the matching alloy provider; the resulting
    /// stream tags every log with `module_name` so `dispatch_chain_log`
    /// routes correctly.
    pub fn chain_log_subscriptions(&self) -> Vec<ChainLogSub> {
        let mut out = Vec::new();
        for module in self.modules.iter().filter(|m| m.alive) {
            for sub in &module.subscriptions {
                if let Subscription::ChainLog {
                    chain_id,
                    address,
                    event_signature,
                    resume,
                    max_lookback,
                } = sub
                {
                    match build_alloy_filter(address.as_deref(), event_signature.as_deref()) {
                        Ok(filter) => {
                            let chain = Chain::from_id(*chain_id);
                            // A `resume` subscription gets a durable cursor
                            // key and its persisted resume point, read once
                            // here at boot; others start at head as before.
                            let (cursor_key, initial_cursor) = if *resume {
                                let key = chainlog_cursor_key(
                                    chain,
                                    address.as_deref(),
                                    event_signature.as_deref(),
                                );
                                let seed = self.read_chain_log_cursor(&module.name, &key);
                                (Some(key), seed)
                            } else {
                                (None, None)
                            };
                            out.push(ChainLogSub {
                                module: module.name.clone(),
                                chain,
                                filter,
                                cursor_key,
                                initial_cursor,
                                max_lookback: *max_lookback,
                            });
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

    /// Read the persisted resume cursor for a chain-log subscription, or
    /// `None` when absent / unreadable - both treated as "start at head".
    fn read_chain_log_cursor(&self, module: &str, key: &str) -> Option<u64> {
        let handle = self.components.store.module(module).ok()?;
        let bytes = handle.get(key).ok()??;
        let arr: [u8; 8] = bytes.try_into().ok()?;
        Some(u64::from_le_bytes(arr))
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
        // path applies the same clock override and the same shared services
        // as the initial boot.
        let clocks = self.clocks.clone();
        let services = self.services.clone();
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
            module.chain_response_max_bytes,
            module.local_store_bytes,
            clocks.as_ref(),
            services,
        )?;
        let bindings = EventModule::instantiate_async(&mut store, &module.component, &linker)
            .await
            .map_err(Error::from)
            .with_context(|| format!("reinstantiate {}", module.name))?;
        let init_outcome = with_dispatch_deadline(
            module.event_deadline,
            bindings.call_init(&mut store, &module.init_config),
        )
        .await
        .map_err(Error::from)?
        .map_err(Error::from)?;
        match init_outcome {
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
        self.sweep_providers().await;

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
        cursor_key: Option<&str>,
    ) -> bool {
        let now = std::time::Instant::now();
        self.sweep_providers().await;
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

        let block_number = log.block_number;
        let event = nexum::host::types::Event::ChainLogs(nexum::host::types::ChainLogs {
            chain_id: chain.id(),
            logs: vec![nexum::host::types::ChainLog::from(&log)],
        });
        let ok = matches!(
            self.dispatch_to(
                idx,
                chain.id(),
                "chain-log",
                block_number.unwrap_or_default(),
                &event
            )
            .await,
            DispatchOutcome::Ok,
        );
        // Persist the resume cursor only after a successful dispatch, so a
        // block is never recorded as done before the module processed it.
        // Advancing to the highest dispatched block is enough; a re-dispatch
        // of the same block after a restart is idempotent (at-least-once).
        if ok && let (Some(key), Some(block)) = (cursor_key, block_number) {
            let store = self.components.store.clone();
            match store.module(module_name) {
                Ok(ms) => {
                    if let Err(e) = ms.set(key, &block.to_le_bytes()) {
                        warn!(
                            module = %module_name,
                            error = %e,
                            "failed to persist chain-log cursor",
                        );
                    }
                }
                Err(e) => warn!(
                    module = %module_name,
                    error = %e,
                    "failed to open module store for chain-log cursor",
                ),
            }
        }
        ok
    }

    /// Dispatch one extension-observed event to every module holding a
    /// subscription of its kind whose filters all match the event's
    /// attributes. Returns the number of modules invoked. Mirrors
    /// `dispatch_block`: dead modules past their backoff are restarted
    /// first, poisoned modules are skipped.
    pub async fn dispatch_extension_event(&mut self, event: ExtensionEvent) -> usize {
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
        self.sweep_providers().await;

        let candidate_indices: Vec<usize> = (0..self.modules.len())
            .filter(|&i| {
                let m = &self.modules[i];
                if m.poisoned || !m.alive {
                    return false;
                }
                m.subscriptions.iter().any(|s| {
                    matches!(
                        s,
                        Subscription::Extension { kind, filters }
                            if kind == event.kind && filters.iter().all(|(fk, fv)| {
                                event.attrs.iter().any(|(ak, av)| ak == fk && av == fv)
                            })
                    )
                })
            })
            .collect();
        let mut dispatched = 0;
        for idx in candidate_indices {
            // Extension events are not chain-scoped: the telemetry chain
            // id and block number carry the 0 sentinel.
            if matches!(
                self.dispatch_to(idx, 0, event.kind, 0, &event.event).await,
                DispatchOutcome::Ok,
            ) {
                dispatched += 1;
            }
        }
        dispatched
    }

    /// The extension subscription kinds at least one loaded module
    /// declares. An extension opens an event source only when its kind
    /// appears here: with no subscriber every event would be dropped on
    /// arrival.
    pub fn extension_subscription_kinds(&self) -> BTreeSet<String> {
        self.modules
            .iter()
            .flat_map(|m| m.subscriptions.iter())
            .filter_map(|s| match s {
                Subscription::Extension { kind, .. } => Some(kind.clone()),
                _ => None,
            })
            .collect()
    }

    /// The extension-owned services, as booted. Shared by every module
    /// store through the service map.
    pub fn services(&self) -> &HostServices {
        &self.services
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
        // Dispatch-boundary rate limit: throttle before spending any
        // fuel or entering the guest, so a flood of cheap-to-dispatch
        // events on this module's source cannot exhaust the host. The
        // bucket is per-module, so a throttled module never starves the
        // others. Over-rate events are dropped and counted; the module
        // stays alive and its failure / poison state is untouched.
        if !module
            .dispatch_bucket
            .try_acquire(std::time::Instant::now())
        {
            debug!(
                module = %module.name,
                chain_id,
                event_kind,
                block_number,
                "dispatch rate limit exceeded - dropping event",
            );
            metrics::counter!(
                "shepherd_dispatch_dropped_total",
                "module" => module.name.clone(),
                "event_kind" => event_kind,
            )
            .increment(1);
            return DispatchOutcome::RateLimited;
        }
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
        // Fuel bounds only guest instructions; time spent inside a host
        // call (chain RPC, redb, HTTP) is unmetered, so bound the whole
        // dispatch, guest plus every host call it awaits, in wall-clock.
        // A deadline hit is fatal like a trap: cancelling the call leaves
        // the store unusable, and the trap arm marks the module dead so
        // the restart sweep reinstantiates it on a fresh store.
        let deadline = module.event_deadline;
        let call = module.bindings.call_on_event(&mut module.store, event);
        let outcome = with_dispatch_deadline(deadline, call)
            .await
            .unwrap_or_else(|exceeded| Err(wasmtime::Error::from(exceeded)));
        match outcome {
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
                    message = %crate::host::error::fault_message(&fault),
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

    /// Fold providers into the recovery path: record any trap the shared
    /// liveness reports (backoff plus poison bookkeeping), then reinstall
    /// dead, unpoisoned providers whose backoff has elapsed. Runs at the
    /// head of every dispatch, beside the module restart sweep.
    async fn sweep_providers(&mut self) {
        let now = std::time::Instant::now();
        let policy = self.poison_policy;
        for idx in 0..self.providers.len() {
            let provider = &mut self.providers[idx];
            if provider.alive
                && let Some(died_at) = provider.liveness.dead_since()
            {
                provider.alive = false;
                provider.failure_count = provider.failure_count.saturating_add(1);
                let backoff = crate::runtime::restart_policy::backoff_for(provider.failure_count);
                // Backoff counts from the death, not from this sweep, so a
                // trap whose backoff already elapsed restarts right below.
                provider.next_attempt = Some(died_at.checked_add(backoff).unwrap_or(now));
                warn!(
                    adapter = %provider.name,
                    failure_count = provider.failure_count,
                    backoff_ms = backoff.as_millis() as u64,
                    "adapter trapped - marked dead; will restart after backoff",
                );
                metrics::counter!(
                    "shepherd_adapter_errors_total",
                    "adapter" => provider.name.clone(),
                    "error_kind" => "trap",
                )
                .increment(1);
                if let Some(recent) = poison_crossed(&mut provider.failure_timestamps, policy)
                    && !provider.poisoned
                {
                    provider.poisoned = true;
                    warn!(
                        adapter = %provider.name,
                        recent_failures = recent,
                        window_secs = policy.window.as_secs(),
                        "adapter poisoned - quarantined; remove from engine.toml + restart to clear",
                    );
                    metrics::gauge!(
                        "shepherd_adapter_poisoned",
                        "adapter" => provider.name.clone(),
                    )
                    .set(1.0);
                }
            }
            let provider = &self.providers[idx];
            if !provider.poisoned
                && !provider.alive
                && provider.next_attempt.is_some_and(|t| t <= now)
            {
                self.try_restart_provider(idx).await;
            }
        }
    }

    /// Attempt to reinstall a dead provider in place: fresh store, fresh
    /// instance, `init`, and a re-install replacing the dead slot. On
    /// success the shared liveness is revived; on failure the backoff
    /// slides further out, like a module restart.
    async fn try_restart_provider(&mut self, idx: usize) {
        let name = self.providers[idx].name.clone();
        let failure_count = self.providers[idx].failure_count;
        info!(adapter = %name, failure_count, "adapter restart attempt");
        metrics::counter!(
            "shepherd_adapter_restarts_total",
            "adapter" => name.clone(),
        )
        .increment(1);
        let outcome = self.reinstall_provider(idx).await;
        let provider = &mut self.providers[idx];
        match outcome {
            Ok(Installed::Live) => {
                provider.run_seq += 1;
                provider.liveness.mark_alive();
                provider.alive = true;
                provider.failure_count = 0;
                provider.next_attempt = None;
                info!(adapter = %name, "adapter restart succeeded");
            }
            Ok(Installed::Dead) => {
                defer_provider_restart(provider, "init returned fault on restart");
            }
            Err(e) => defer_provider_restart(provider, &format!("{e:#}")),
        }
    }

    /// Rebuild a provider from its cached component and grants, then hand
    /// it back to its kind to instantiate and install over the dead slot.
    async fn reinstall_provider(&mut self, idx: usize) -> Result<Installed> {
        let provider = &self.providers[idx];
        let (kind, service) = self
            .kinds
            .get(provider.kind)
            .ok_or_else(|| anyhow!("provider kind {} is not registered", provider.kind))?;
        let linker = build_provider_linker::<T>(&self.engine, kind.as_ref())?;
        // A restart is a new run, like a module's.
        let run = RunId::new(provider.name.clone(), provider.run_seq + 1);
        let store = Self::build_store(
            &self.engine,
            &self.components,
            run,
            provider.http_allow.clone(),
            provider.http_limits,
            provider.messaging_topics.clone(),
            provider.memory_limit,
            provider.fuel_per_call,
            provider.chain_response_max_bytes,
            provider.local_store_bytes,
            self.clocks.as_ref(),
            HostServices::default(),
        )?;
        kind.install(
            ProviderInstance {
                component: &provider.component,
                linker: &linker,
                store,
                config: provider.init_config.clone(),
                sections: &provider.sections,
                fuel_per_call: provider.fuel_per_call,
                liveness: provider.liveness.clone(),
            },
            service,
        )
        .await
    }

    /// Count of modules currently alive. A module is not alive when its
    /// `init` returned `Err` (permanent, never retried) or when `on_event`
    /// trapped and its restart backoff has not yet elapsed.
    pub fn alive_count(&self) -> usize {
        self.modules.iter().filter(|m| m.alive).count()
    }

    /// True when at least one init-failed module declared subscriptions.
    /// Lets the launch path distinguish "no manifest declares any
    /// `[[subscription]]`" (benign: exit cleanly) from "every declared
    /// subscription belongs to a dead module" (operator error: abort).
    pub fn dead_modules_hold_subscriptions(&self) -> bool {
        self.modules
            .iter()
            .any(|m| !m.alive && !m.subscriptions.is_empty())
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
    extensions: &[Arc<dyn Extension<T>>],
) -> anyhow::Result<Linker<HostState<T>>> {
    let mut linker = Linker::<HostState<T>>::new(engine);
    EventModule::add_to_linker::<HostState<T>, HasSelf<HostState<T>>>(&mut linker, |state| state)?;
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    // wasi:http only; the p2 call above already covers the shared
    // wasi:io/wasi:clocks interfaces.
    wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)?;
    for ext in extensions {
        ext.link(&mut linker)?;
    }
    Ok(linker)
}

/// Build a `Linker` for one provider kind: the kind's own scoped imports
/// plus the ambient WASI base and the allowlisted `wasi:http`. The core
/// `nexum:host` interfaces a provider must not touch (local-store,
/// remote-store, identity, logging) are deliberately withheld, so a
/// provider that imports one of them fails to instantiate rather than
/// silently gaining reach. Extensions are not linked into providers: a
/// provider speaks its protocol over the standard transport, not a domain
/// extension surface.
pub fn build_provider_linker<T: RuntimeTypes>(
    engine: &Engine,
    kind: &dyn ProviderKind<T>,
) -> anyhow::Result<Linker<HostState<T>>> {
    let mut linker = Linker::<HostState<T>>::new(engine);
    kind.link(&mut linker)?;
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
    extensions: &[Arc<dyn Extension<T>>],
) -> CapabilityRegistry {
    let mut registry = CapabilityRegistry::core();
    for ext in extensions {
        registry.register(ext.capabilities());
    }
    registry
}

/// A guest dispatch, guest execution plus every host call it awaited,
/// outlived its wall-clock deadline and was cancelled. Distinct from a
/// fuel trap: fuel bounds guest instructions, this bounds time spent in
/// host calls (chain RPC, redb, HTTP), which fuel does not meter.
#[derive(Debug, thiserror::Error)]
#[error(
    "dispatch exceeded its {0:?} wall-clock deadline \
     (a host call blocked or ran too long)"
)]
struct DeadlineExceeded(Duration);

/// Run a guest dispatch future under a wall-clock `deadline`.
///
/// Fuel and epoch metering bound only *guest* instructions; time spent
/// inside a host call is unmetered (see [`crate::runtime::limits`]), so
/// without this a module could park the dispatch indefinitely behind a
/// cheap-in-fuel host call. Returns `Err(DeadlineExceeded)` once the
/// future, guest plus every host call it awaited, outlives `deadline`;
/// dropping the future on timeout cancels the in-flight host call at its
/// next await point. Pure guest CPU spinning stays fuel's job: a future
/// that never yields cannot be interrupted here, which is exactly why
/// fuel and this deadline are complementary rather than redundant.
async fn with_dispatch_deadline<F: std::future::Future>(
    deadline: Duration,
    fut: F,
) -> Result<F::Output, DeadlineExceeded> {
    tokio::time::timeout(deadline, fut)
        .await
        .map_err(|_elapsed| DeadlineExceeded(deadline))
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
    /// The per-module dispatch rate limit was exceeded. The event is
    /// dropped before the guest runs; the module stays alive and its
    /// failure / poison state is untouched.
    RateLimited,
}

/// Push the current trap timestamp into a component's failure-window
/// ring, drop entries older than the policy window, and report the
/// recent-failure count once it crosses `policy.max_failures`. Shared by
/// the module and provider poison sweeps.
fn poison_crossed(
    failure_timestamps: &mut std::collections::VecDeque<std::time::Instant>,
    policy: crate::runtime::poison_policy::PoisonPolicy,
) -> Option<u32> {
    let now = std::time::Instant::now();
    while let Some(&front) = failure_timestamps.front() {
        if now.duration_since(front) > policy.window {
            failure_timestamps.pop_front();
        } else {
            break;
        }
    }
    failure_timestamps.push_back(now);
    let recent = failure_timestamps.len() as u32;
    crate::runtime::poison_policy::should_poison(policy, recent).then_some(recent)
}

/// Flip `poisoned = true` once the module's failure window crosses the
/// policy threshold. The first transition emits the
/// `shepherd_module_poisoned` gauge + a structured WARN.
fn record_failure_and_maybe_poison<T: RuntimeTypes>(
    module: &mut LoadedModule<T>,
    policy: crate::runtime::poison_policy::PoisonPolicy,
    last_error: &str,
) {
    if let Some(recent) = poison_crossed(&mut module.failure_timestamps, policy)
        && !module.poisoned
    {
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

/// Slide a failed provider restart's next attempt further out.
fn defer_provider_restart(provider: &mut LoadedProvider, error: &str) {
    provider.failure_count = provider.failure_count.saturating_add(1);
    let backoff = crate::runtime::restart_policy::backoff_for(provider.failure_count);
    provider.next_attempt = Some(std::time::Instant::now() + backoff);
    error!(
        adapter = %provider.name,
        failure_count = provider.failure_count,
        backoff_ms = backoff.as_millis() as u64,
        error,
        "adapter restart failed - will retry after backoff",
    );
}

/// Persisted per-chain progress key; must stay numeric for data compat.
fn progress_key(chain: Chain) -> String {
    format!("last_dispatched_block:{}", chain.id())
}

/// A resolved chain-log subscription for the event loop: the owning
/// module, the chain + alloy `Filter`, and - when the subscription opted
/// into `resume` - the durable cursor key plus the block to resume from
/// (read from the store at boot).
pub struct ChainLogSub {
    /// Module that declared the subscription; also its store namespace.
    pub module: String,
    /// Chain the filter applies to.
    pub chain: Chain,
    /// Alloy filter the poller opens with.
    pub filter: alloy_rpc_types_eth::Filter,
    /// `Some` iff `resume = true`: the store key the resume cursor is read
    /// and written under.
    pub cursor_key: Option<String>,
    /// The persisted resume block, read at boot for a `resume`
    /// subscription; `None` on first run or when `resume` is off.
    pub initial_cursor: Option<u64>,
    /// Opt-in cap on how far back the poller backfills, in blocks. `None`
    /// backfills the whole gap; `Some(cap)` bounds the start to
    /// `head - cap`, dropping the oldest missed blocks.
    pub max_lookback: Option<u64>,
}

/// Durable resume-cursor key for a chain-log subscription. Derived from
/// the normalized manifest inputs - NOT the alloy `Filter`, whose hash
/// uses a process-randomized `HashSet` and is not reproducible across
/// restarts. Stable and independent of `[[subscription]]` ordering. The
/// module name is the store namespace, so it is not part of the digest.
fn chainlog_cursor_key(
    chain: Chain,
    address: Option<&str>,
    event_signature: Option<&str>,
) -> String {
    let normalized = format!(
        "{}|{}|{}",
        chain.id(),
        address.unwrap_or("").to_ascii_lowercase(),
        event_signature.unwrap_or("").to_ascii_lowercase(),
    );
    format!(
        "chainlog_cursor:{:x}",
        alloy_primitives::keccak256(normalized.as_bytes())
    )
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
