//! The RuntimeTypes lattice: one trait naming the core backend seams plus
//! the pluggable extension slot, so every generic signature takes a single
//! parameter.
//!
//! Time, randomness, and outbound HTTP are deliberately not members: all
//! are WASI concerns serviced per store (WasiCtxBuilder for clocks and
//! randomness, wasi:http behind the allowlist gate), not host backends.
//! Domain backends such as cow-api are not core seams: they live behind
//! the [`RuntimeTypes::Ext`] slot and are wired in as extensions.

use crate::host::component::{ChainProvider, StateStore};

/// Names the core backend seams a runtime assembly provides, plus the
/// extension slot ([`Ext`](RuntimeTypes::Ext)) that carries any non-core
/// backend an extension needs.
pub trait RuntimeTypes: 'static {
    /// JSON-RPC dispatch and subscriptions.
    type Chain: ChainProvider + Clone + Send + Sync + 'static;
    /// Process-wide store vending per-module handles.
    type Store: StateStore<Handle: Send + Sync + 'static> + Clone + Send + Sync + 'static;
    /// Extension state slot. Backends that are not core capabilities live
    /// here; an extension reaches its payload through the `ExtState`
    /// accessor without naming the concrete lattice. `()` for an assembly
    /// with no extensions.
    type Ext: Clone + Send + Sync + 'static;
}

/// Per-module store handle of a lattice's Store member.
pub type Handle<T> = <<T as RuntimeTypes>::Store as StateStore>::Handle;
