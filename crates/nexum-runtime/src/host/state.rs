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
    /// Extension backends (the lattice `Ext` payload). Reached generically
    /// by an extension's `Host` impl through [`ExtState`].
    pub ext: T::Ext,
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

/// Generic access to the extension state slot of a host state.
///
/// An extension crate implements its bindgen-local `Host` trait for the
/// foreign `HostState<T>` (orphan-legal: the trait is local to the
/// extension) and reaches its own payload through this accessor, without
/// naming the concrete lattice `T`. The extension then bounds the payload
/// on its own trait to extract its backend.
pub trait ExtState {
    /// The extension payload type (the lattice `Ext` member).
    type Ext;
    /// Borrow the extension payload.
    fn ext(&self) -> &Self::Ext;
}

impl<T: RuntimeTypes> ExtState for HostState<T> {
    type Ext = T::Ext;
    fn ext(&self) -> &Self::Ext {
        &self.ext
    }
}
