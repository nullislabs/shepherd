//! The RuntimeTypes lattice: one trait naming the core backend seams plus
//! the pluggable extension slot, so every generic signature takes a single
//! parameter.
//!
//! Randomness is deliberately not a member: it is a WASI concern
//! injected per store via WasiCtxBuilder, not a host backend. Domain
//! backends such as cow-api are not core seams: they live behind the
//! [`RuntimeTypes::Ext`] slot and are wired in as extensions.

use crate::host::component::{
    ChainProvider, Clock, HttpClient, StateStore, SystemClock, UnsupportedHttp,
};
use crate::host::ext_cow::ReferenceExt;
use crate::host::local_store_redb::LocalStore;
use crate::host::provider_pool::ProviderPool;

/// Names the core backend seams a runtime assembly provides, plus the
/// extension slot ([`Ext`](RuntimeTypes::Ext)) that carries any non-core
/// backend an extension needs.
pub trait RuntimeTypes: 'static {
    /// JSON-RPC dispatch and subscriptions.
    type Chain: ChainProvider + Clone + Send + Sync + 'static;
    /// Process-wide store vending per-module handles.
    type Store: StateStore<Handle: Send + Sync + 'static> + Clone + Send + Sync + 'static;
    /// Per-store time source; Default captures the monotonic origin.
    type Clock: Clock + Default + Send + Sync + 'static;
    /// Outbound HTTP backend (post-allowlist).
    type Http: HttpClient + Clone + Send + Sync + 'static;
    /// Extension state slot. Backends that are not core capabilities live
    /// here; an extension reaches its payload through the `ExtState`
    /// accessor without naming the concrete lattice. `()` for an assembly
    /// with no extensions.
    type Ext: Clone + Send + Sync + 'static;
}

/// Per-module store handle of a lattice's Store member.
pub type Handle<T> = <<T as RuntimeTypes>::Store as StateStore>::Handle;

/// Preset binding the backends the reference engine ships, including the
/// cow-api extension in its [`Ext`](RuntimeTypes::Ext) slot.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReferenceTypes;

impl RuntimeTypes for ReferenceTypes {
    type Chain = ProviderPool;
    type Store = LocalStore;
    type Clock = SystemClock;
    type Http = UnsupportedHttp;
    type Ext = ReferenceExt;
}
