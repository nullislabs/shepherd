//! Per-instance host state and its WASI view.
//!
//! One [`HostState`] is created per module, lives inside the wasmtime
//! `Store`, and is the receiver every `Host` trait impl in
//! `super::impls` is implemented for.

use wasmtime::component::ResourceTable;
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

use super::component::{Handle, RuntimeTypes};

/// Per-module host state, generic over the [`RuntimeTypes`] lattice
/// binding the five backend seams. [`ReferenceTypes`] is the shipped
/// assembly.
///
/// [`ReferenceTypes`]: super::component::ReferenceTypes
pub struct HostState<T: RuntimeTypes> {
    pub wasi: WasiCtx,
    pub table: ResourceTable,
    /// Wasmtime memory/table/instance resource limits for this store.
    pub limits: wasmtime::StoreLimits,
    /// Per-module `[capabilities.http].allow` allowlist (from module.toml).
    /// Consulted by `http::fetch` before any outbound call.
    pub http_allowlist: Vec<String>,
    /// Namespace for the running module, used only for log tagging.
    /// The namespace identity for storage is baked into `store`'s prefix.
    pub module_namespace: String,
    /// `cow-api` backend - per-chain `OrderBookApi` clients + reqwest.
    pub cow: T::Cow,
    /// `chain` backend - per-chain alloy `DynProvider` pool.
    pub chain: T::Chain,
    /// `local-store` backend — per-module handle with pre-computed
    /// keccak256 namespace prefix.
    pub store: Handle<T>,
    /// Time source for `clock::now-ms` / `clock::monotonic-ns`; the
    /// Default origin is captured per store.
    pub clock: T::Clock,
    /// `http` backend - the 0.2 reference build wires the stub.
    pub http: T::Http,
}

// `WasiView: Send`, so the backends must be `Send` too; the lattice
// supertraits already guarantee it.
impl<T: RuntimeTypes> WasiView for HostState<T> {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}
