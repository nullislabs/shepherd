//! The RuntimeTypes lattice: one trait naming the core backend seams plus the
//! pluggable [`RuntimeTypes::Ext`] slot, so every generic signature takes one
//! parameter.

use crate::host::component::{ChainProvider, StateStore};

/// Core backend seams a runtime assembly provides, plus the extension slot
/// ([`Ext`](RuntimeTypes::Ext)). Sealed.
pub trait RuntimeTypes: crate::sealed::SealedRuntimeTypes + 'static {
    /// JSON-RPC dispatch and subscriptions.
    type Chain: ChainProvider + Clone + Send + Sync + 'static;
    /// Process-wide store vending per-module handles.
    type Store: StateStore<Handle: Send + Sync + 'static> + Clone + Send + Sync + 'static;
    /// Extension state slot; `()` for an assembly with no extensions.
    type Ext: Clone + Send + Sync + 'static;
}

/// Per-module store handle of a lattice's Store member.
pub type Handle<T> = <<T as RuntimeTypes>::Store as StateStore>::Handle;
