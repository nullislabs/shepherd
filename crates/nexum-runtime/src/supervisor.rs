//! Multi-module supervisor.
//!
//! Loads every `[[modules]]` entry from `engine.toml`, instantiates
//! each as a `Shepherd` bindings against a dedicated wasmtime
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
//! Modules whose `init` returned `Err(HostError)` are dead with
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

use alloy_chains::Chain;
use anyhow::{Context, Error, Result, anyhow};
use tracing::{debug, error, info, warn};
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Engine, Store};
use wasmtime_wasi::WasiCtxBuilder;

use crate::bindings::{Config, Shepherd, nexum};
use crate::engine_config::{EngineConfig, ModuleEntry, ModuleLimits};
#[cfg(test)]
use crate::host::component::UnsupportedHttp;
use crate::host::component::{
    ChainProvider, Components, CowApi, HttpClient, StateHandle, StateStore,
};
#[cfg(test)]
use crate::host::cow_orderbook::OrderBookPool;
#[cfg(test)]
use crate::host::local_store_redb::LocalStore;
#[cfg(test)]
use crate::host::provider_pool::ProviderPool;
use crate::host::state::HostState;
use crate::manifest::{self, LoadedManifest, Subscription};

/// Owns every loaded module and exposes the dispatch surface the
/// event loop needs. Generic over the component seam backends:
/// `C` = chain, `W` = CoW, `S` = state store, `H` = HTTP.
pub struct Supervisor<C, W, S, H>
where
    C: 'static,
    W: 'static,
    S: StateStore,
    S::Handle: 'static,
    H: 'static,
{
    modules: Vec<LoadedModule<C, W, S::Handle, H>>,
    /// Cached for module restart: re-instantiating a trapped module
    /// requires a fresh wasmtime `Store` + `Linker`, which in turn need
    /// the shared backends. The `Components` bundle is cheaply cloned
    /// (Arc-backed members) so the supervisor takes an owned copy at boot.
    engine: Engine,
    components: Components<C, W, S, H>,
    /// Poison-pill thresholds. Defaults to the production
    /// constants (5 failures / 10 min); tests inject tighter values
    /// via `boot_with_poison_policy` / `empty_for_test`.
    poison_policy: crate::runtime::poison_policy::PoisonPolicy,
}

/// The concrete supervisor the reference engine runs. Only named by the
/// test-only constructors today; the launch path infers it.
#[cfg(test)]
pub(crate) type DefaultSupervisor =
    Supervisor<ProviderPool, OrderBookPool, LocalStore, UnsupportedHttp>;

/// A wasmtime `Store` holding the generic `HostState` (`S` is the
/// per-module handle here). Named so the module and helper signatures
/// stay legible.
type HostStore<C, W, S, H> = Store<HostState<C, W, S, H>>;

struct LoadedModule<C, W, S, H>
where
    C: 'static,
    W: 'static,
    S: 'static,
    H: 'static,
{
    name: String,
    bindings: Shepherd,
    store: HostStore<C, W, S, H>,
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

impl<C, W, S, H> Supervisor<C, W, S, H>
where
    C: ChainProvider + Clone + Send + Sync + 'static,
    W: CowApi + Clone + Send + Sync + 'static,
    S: StateStore + Clone + Send + Sync + 'static,
    S::Handle: StateHandle + Send + Sync + 'static,
    H: HttpClient + Clone + Send + Sync + 'static,
{
    /// Compile + instantiate every module declared in
    /// `engine_cfg.modules`. The wasmtime `Engine` + `Linker` are
    /// passed in so `main.rs` can build them once.
    pub async fn boot(
        engine: &Engine,
        linker: &Linker<HostState<C, W, S::Handle, H>>,
        engine_cfg: &EngineConfig,
        components: &Components<C, W, S, H>,
    ) -> Result<Self> {
        let mut modules = Vec::with_capacity(engine_cfg.modules.len());
        for entry in &engine_cfg.modules {
            let loaded = Self::load_one(engine, linker, entry, components, &engine_cfg.limits)
                .await
                .with_context(|| format!("load module {}", entry.path.display()))?;
            modules.push(loaded);
        }
        let alive = modules.iter().filter(|m| m.alive).count();
        info!(loaded = modules.len(), alive, "supervisor up");
        Ok(Self {
            modules,
            engine: engine.clone(),
            components: components.clone(),
            poison_policy: crate::runtime::poison_policy::PoisonPolicy::default(),
        })
    }

    /// One-shot construction from a single ad-hoc `(component, manifest)`
    /// pair. Used by the CLI-positional invocation so `just run`
    /// against the example module keeps working without an
    /// `engine.toml`.
    pub async fn boot_single(
        engine: &Engine,
        linker: &Linker<HostState<C, W, S::Handle, H>>,
        wasm: &Path,
        manifest: Option<&Path>,
        components: &Components<C, W, S, H>,
        limits: &ModuleLimits,
    ) -> Result<Self> {
        let entry = ModuleEntry {
            path: wasm.to_path_buf(),
            manifest: manifest.map(Path::to_path_buf),
        };
        let loaded = Self::load_one(engine, linker, &entry, components, limits).await?;
        Ok(Self {
            modules: vec![loaded],
            engine: engine.clone(),
            components: components.clone(),
            poison_policy: crate::runtime::poison_policy::PoisonPolicy::default(),
        })
    }

    /// Build a fresh wasmtime `Store` wired to the shared backends, with
    /// the per-module namespace, allowlist, memory cap, and fuel applied.
    /// Shared by `load_one` and `reinstantiate_one`.
    fn build_store(
        engine: &Engine,
        components: &Components<C, W, S, H>,
        namespace: &str,
        http_allowlist: Vec<String>,
        memory_limit: usize,
        fuel: u64,
    ) -> Result<HostStore<C, W, S::Handle, H>> {
        let wasi = WasiCtxBuilder::new().inherit_stdio().build();
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
                monotonic_baseline: std::time::Instant::now(),
                http_allowlist,
                module_namespace: namespace.to_owned(),
                cow: components.cow.clone(),
                chain: components.chain.clone(),
                store: module_store,
                http: components.http.clone(),
            },
        );
        store.limiter(|state| &mut state.limits);
        store.set_fuel(fuel)?;
        Ok(store)
    }

    /// Override the poison-pill policy. Tests use this to inject
    /// tighter thresholds (e.g. 3 failures in 60 s) so the
    /// integration suite does not wait out the production 5/10min
    /// schedule. Returns `self` so it can be chained off `boot_single`.
    #[cfg(test)]
    pub(crate) fn with_poison_policy(
        mut self,
        policy: crate::runtime::poison_policy::PoisonPolicy,
    ) -> Self {
        self.poison_policy = policy;
        self
    }

    async fn load_one(
        engine: &Engine,
        linker: &Linker<HostState<C, W, S::Handle, H>>,
        entry: &ModuleEntry,
        components: &Components<C, W, S, H>,
        limits_cfg: &ModuleLimits,
    ) -> Result<LoadedModule<C, W, S::Handle, H>> {
        // Canonical name is module.toml (ADR-0001). nexum.toml is accepted
        // with a deprecation warning during the 0.1→0.2 transition.
        let manifest_path = entry.manifest.clone().or_else(|| {
            let dir = entry.path.parent()?.to_owned();
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
        });
        let loaded_manifest: LoadedManifest = match manifest_path.as_deref() {
            Some(p) if p.exists() => {
                info!(manifest = %p.display(), "loading module manifest");
                manifest::load(p)?
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
        let mut store = Self::build_store(
            engine,
            components,
            &module_namespace,
            loaded_manifest.http_allowlist.clone(),
            limits_cfg.memory(),
            limits_cfg.fuel(),
        )?;
        let bindings = Shepherd::instantiate_async(&mut store, &component, linker)
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
        // `Err(HostError)` the module's strategy state (e.g. an
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
                    domain = %e.domain,
                    kind = ?e.kind,
                    code = e.code,
                    message = %e.message,
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
            subscriptions: loaded_manifest.manifest.subscriptions.clone(),
            fuel_per_event: limits_cfg.fuel(),
            memory_limit: limits_cfg.memory(),
            alive: init_succeeded,
            failure_count: 0,
            next_attempt: None,
            component,
            init_config: config,
            http_allowlist: loaded_manifest.http_allowlist.clone(),
            failure_timestamps: std::collections::VecDeque::new(),
            poisoned: false,
        })
    }

    /// Number of modules currently loaded.
    pub fn module_count(&self) -> usize {
        self.modules.len()
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

    /// Per-module log subscriptions. Each entry is a `(module_name,
    /// chain, filter)` triple the event loop opens against the
    /// matching alloy provider; the resulting stream tags every log
    /// with `module_name` so `dispatch_log` routes correctly.
    pub fn log_subscriptions(&self) -> Vec<(String, Chain, alloy_rpc_types_eth::Filter)> {
        let mut out = Vec::new();
        for module in &self.modules {
            for sub in &module.subscriptions {
                if let Subscription::Log {
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
                            "invalid log subscription - skipping",
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
        // Re-build the wasi linker. Cheap: just two `add_to_linker`
        // calls against the cached `Engine`.
        let linker = build_linker::<C, W, S::Handle, H>(&self.engine)?;

        let module = &mut self.modules[idx];
        let mut store = Self::build_store(
            &self.engine,
            &self.components,
            &module.name,
            module.http_allowlist.clone(),
            module.memory_limit,
            module.fuel_per_event,
        )?;
        let bindings = Shepherd::instantiate_async(&mut store, &module.component, &linker)
            .await
            .map_err(Error::from)
            .with_context(|| format!("reinstantiate {}", module.name))?;
        match bindings.call_init(&mut store, &module.init_config).await? {
            Ok(()) => {}
            Err(e) => {
                return Err(anyhow!(
                    "init returned host-error on restart: {} ({:?})",
                    e.message,
                    e.kind
                ));
            }
        }
        module.bindings = bindings;
        module.store = store;
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
                self.dispatch_to(idx, chain, "block", block_number, &event)
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

    /// Dispatch a log event to the specific module that opened the
    /// subscription. Returns `true` when the module accepted the dispatch;
    /// `false` when the module is dead, not found, or its callback failed.
    /// A trapping module is marked dead and excluded from future dispatch.
    pub async fn dispatch_log(
        &mut self,
        module_name: &str,
        chain: Chain,
        log: alloy_rpc_types_eth::Log,
    ) -> bool {
        let now = std::time::Instant::now();
        let Some(idx) = self.modules.iter().position(|m| m.name == module_name) else {
            warn!(module = %module_name, "no such module - dropping log");
            return false;
        };

        // Poison-pill: quarantined modules get no log
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
        let event = nexum::host::types::Event::Logs(vec![project_log(chain.id(), &log)]);
        matches!(
            self.dispatch_to(idx, chain, "log", block_number, &event)
                .await,
            DispatchOutcome::Ok,
        )
    }

    /// Shared per-module dispatch path: refuel, call `on_event`, and
    /// process the three outcomes (ok / host-error / trap) with the
    /// same telemetry + lifecycle bookkeeping. Returns whether the
    /// guest call succeeded; the caller layers any path-specific
    /// follow-up (e.g. the progress marker on `dispatch_block`).
    async fn dispatch_to(
        &mut self,
        idx: usize,
        chain: Chain,
        event_kind: &'static str,
        block_number: u64,
        event: &nexum::host::types::Event,
    ) -> DispatchOutcome {
        let chain_id = chain.id();
        let poison_policy = self.poison_policy;
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
            Ok(Err(host_err)) => {
                let elapsed = start.elapsed();
                let latency_ms = elapsed.as_millis() as u64;
                warn!(
                    module = %module.name,
                    chain_id,
                    event_kind,
                    block_number,
                    latency_ms,
                    domain = %host_err.domain,
                    kind = ?host_err.kind,
                    message = %host_err.message,
                    "on-event returned host-error",
                );
                metrics::counter!(
                    "shepherd_module_errors_total",
                    "module" => module.name.clone(),
                    "error_kind" => format!("{:?}", host_err.kind),
                )
                .increment(1);
                DispatchOutcome::HostError
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

#[cfg(test)]
impl DefaultSupervisor {
    /// Build a zero-module supervisor with synthetic shared
    /// backends. Used by the unit tests that need a `Supervisor` to
    /// poke its public surface without going through the full
    /// `boot` pipeline.
    pub(crate) fn empty_for_test(engine: &Engine, local_store: LocalStore) -> Self {
        Self {
            modules: Vec::new(),
            engine: engine.clone(),
            components: Components {
                chain: ProviderPool::empty(),
                cow: OrderBookPool::default(),
                store: local_store,
                http: UnsupportedHttp,
            },
            poison_policy: crate::runtime::poison_policy::PoisonPolicy::default(),
        }
    }
}

/// Build a `Linker` binding every WIT `Host` impl for
/// `HostState<C, W, S, H>` (here `S` is the per-module handle). Shared by
/// the supervisor restart path and the bootstrap launch path.
pub(crate) fn build_linker<C, W, S, H>(
    engine: &Engine,
) -> anyhow::Result<Linker<HostState<C, W, S, H>>>
where
    C: ChainProvider + Send + Sync + 'static,
    W: CowApi + Send + Sync + 'static,
    S: StateHandle + Send + Sync + 'static,
    H: HttpClient + Send + Sync + 'static,
{
    let mut linker = Linker::<HostState<C, W, S, H>>::new(engine);
    Shepherd::add_to_linker::<
        HostState<C, W, S, H>,
        wasmtime::component::HasSelf<HostState<C, W, S, H>>,
    >(&mut linker, |state| state)?;
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    Ok(linker)
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
    /// Guest returned a typed `host-error` via WIT.
    HostError,
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
fn record_failure_and_maybe_poison<C, W, S, H>(
    module: &mut LoadedModule<C, W, S, H>,
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

/// Project an alloy `Log` onto the WIT `log` record. The chain id
/// is not on the alloy log (the subscription context carries it),
/// so we receive it alongside.
fn project_log(chain_id: u64, log: &alloy_rpc_types_eth::Log) -> nexum::host::types::Log {
    nexum::host::types::Log {
        chain_id,
        address: log.address().as_slice().to_vec(),
        topics: log.topics().iter().map(|t| t.as_slice().to_vec()).collect(),
        data: log.inner.data.data.to_vec(),
        block_number: log.block_number.unwrap_or(0),
        transaction_hash: log
            .transaction_hash
            .map(|h| h.as_slice().to_vec())
            .unwrap_or_default(),
        log_index: log.log_index.unwrap_or(0) as u32,
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
    #[error("invalid log address {address:?}: {source}")]
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

/// Translate a `[[subscription]]` log entry into an alloy `Filter`.
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
