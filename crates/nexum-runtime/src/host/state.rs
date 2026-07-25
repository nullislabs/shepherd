//! Per-module host state, held in the wasmtime `Store` and the receiver for
//! every `Host` impl in `super::impls`.

use std::sync::Arc;

use wasmtime::component::ResourceTable;
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_http::WasiHttpCtx;

use super::component::{Handle, RuntimeTypes};
use super::extension::HostServices;
use super::http::HttpGate;
use super::logs::{LogRouter, RunId};

/// Per-module host state, generic over the [`RuntimeTypes`] lattice.
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
    /// Content topics this store may publish to; empty is unscoped. An
    /// out-of-scope publish is refused before the backend.
    pub messaging_topics: Vec<String>,
    /// Identity of this store's run; tags every captured log record.
    pub run: RunId,
    /// Shared log pipeline the `nexum:host/logging` glue routes through.
    pub log_router: Arc<LogRouter>,
    /// Extension backends (the lattice `Ext` payload), reached via
    /// [`ExtState`].
    pub ext: T::Ext,
    /// `chain` backend: per-chain provider pool.
    pub chain: T::Chain,
    /// Cap on a chain JSON-RPC response body; larger responses are rejected.
    pub chain_response_max_bytes: usize,
    /// `local-store` backend: per-module handle with keccak256 prefix.
    pub store: Handle<T>,
    /// Extension-owned host services, keyed by namespace; a provider store
    /// carries an empty map.
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

/// Generic access to the extension payload of a host state, without naming
/// the concrete lattice `T`.
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
