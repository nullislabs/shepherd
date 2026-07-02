//! Per-instance host state and its WASI view.
//!
//! One [`HostState`] is created per module, lives inside the wasmtime
//! `Store`, and is the receiver every `Host` trait impl in
//! `super::impls` is implemented for.

use std::time::Instant;

use wasmtime::component::ResourceTable;
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

use super::cow_orderbook::OrderBookPool;
use super::local_store_redb::ModuleStore;
use super::provider_pool::ProviderPool;

pub struct HostState {
    pub wasi: WasiCtx,
    pub table: ResourceTable,
    /// Wasmtime memory/table/instance resource limits for this store.
    pub limits: wasmtime::StoreLimits,
    /// Origin for `clock::monotonic-ns`. Differences between successive
    /// readings are the only meaningful values.
    pub monotonic_baseline: Instant,
    /// Per-module `[capabilities.http].allow` allowlist (from module.toml).
    /// Consulted by `http::fetch` before any outbound call.
    pub http_allowlist: Vec<String>,
    /// Namespace for the running module, used only for log tagging.
    /// The namespace identity for storage is baked into `store`'s prefix.
    pub module_namespace: String,
    /// `cow-api` backend - per-chain `OrderBookApi` clients + reqwest.
    pub cow: OrderBookPool,
    /// `chain` backend - per-chain alloy `DynProvider` pool.
    pub chain: ProviderPool,
    /// `local-store` backend — per-module handle with pre-computed
    /// keccak256 namespace prefix.
    pub store: ModuleStore,
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}
