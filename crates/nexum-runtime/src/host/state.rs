//! Per-instance host state and its WASI view.
//!
//! One [`HostState`] is created per module, lives inside the wasmtime
//! `Store`, and is the receiver every `Host` trait impl in
//! `super::impls` is implemented for.

use std::sync::Arc;

use wasmtime::component::ResourceTable;
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_http::WasiHttpCtx;

use super::component::{Handle, RuntimeTypes};
use super::extension::HostServices;
use super::http::HttpGate;
use super::logs::{LogRouter, RunId};

/// Per-module host state, generic over the [`RuntimeTypes`] lattice
/// binding the backend seams. The composition root supplies the
/// concrete assembly.
pub struct HostState<T: RuntimeTypes> {
    pub wasi: WasiCtx,
    pub table: ResourceTable,
    /// Wasmtime memory/table/instance resource limits for this store.
    pub limits: wasmtime::StoreLimits,
    /// Per-store wasi:http context.
    pub http_ctx: WasiHttpCtx,
    /// Per-module allowlist gate every wasi:http outgoing request
    /// passes through.
    pub http_gate: HttpGate,
    /// Messaging content topics this store may publish to. Empty means
    /// unscoped (the module default and current messaging posture); a
    /// venue adapter carries its `[[adapters]].messaging_topics` grant
    /// here, so an out-of-scope publish is refused before it reaches the
    /// backend.
    pub messaging_topics: Vec<String>,
    /// Identity of this store's run: module namespace plus the restart
    /// sequence. Tags every captured log record. The namespace identity
    /// for storage is baked into `store`'s prefix.
    pub run: RunId,
    /// Shared log pipeline the `nexum:host/logging` glue routes through.
    pub log_router: Arc<LogRouter>,
    /// Extension backends (the lattice `Ext` payload). Reached generically
    /// by an extension's `Host` impl through [`ExtState`].
    pub ext: T::Ext,
    /// `chain` backend - per-chain alloy `DynProvider` pool.
    pub chain: T::Chain,
    /// Host-enforced cap on a single chain JSON-RPC response body.
    /// Responses larger than this are rejected before they reach the guest.
    pub chain_response_max_bytes: usize,
    /// `local-store` backend - per-module handle with pre-computed
    /// keccak256 namespace prefix.
    pub store: Handle<T>,
    /// Extension-owned host services, keyed by extension namespace and
    /// downcast at the call site. One shared map across every module store;
    /// a provider store carries an empty map.
    pub services: HostServices,
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
