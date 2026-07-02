//! The RuntimeTypes lattice: one trait naming the five backend seams
//! so every generic signature takes a single parameter.
//!
//! Randomness is deliberately not a member: it is a WASI concern
//! injected per store via WasiCtxBuilder, not a host backend.

use crate::host::component::{
    ChainProvider, Clock, CowApi, HttpClient, StateStore, SystemClock, UnsupportedHttp,
};
use crate::host::cow_orderbook::OrderBookPool;
use crate::host::local_store_redb::LocalStore;
use crate::host::provider_pool::ProviderPool;

/// Names the five backend seams a runtime assembly provides.
pub trait RuntimeTypes: 'static {
    /// JSON-RPC dispatch and subscriptions.
    type Chain: ChainProvider + Clone + Send + Sync + 'static;
    /// CoW orderbook passthrough and typed submission.
    type Cow: CowApi + Clone + Send + Sync + 'static;
    /// Process-wide store vending per-module handles.
    type Store: StateStore<Handle: Send + Sync + 'static> + Clone + Send + Sync + 'static;
    /// Per-store time source; Default captures the monotonic origin.
    type Clock: Clock + Default + Send + Sync + 'static;
    /// Outbound HTTP backend (post-allowlist).
    type Http: HttpClient + Clone + Send + Sync + 'static;
}

/// Per-module store handle of a lattice's Store member.
pub type Handle<T> = <<T as RuntimeTypes>::Store as StateStore>::Handle;

/// Preset binding the backends the reference engine ships.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReferenceTypes;

impl RuntimeTypes for ReferenceTypes {
    type Chain = ProviderPool;
    type Cow = OrderBookPool;
    type Store = LocalStore;
    type Clock = SystemClock;
    type Http = UnsupportedHttp;
}
