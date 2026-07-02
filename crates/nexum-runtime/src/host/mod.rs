//! Host-side backends for the `nexum:host` / `shepherd:cow` interfaces,
//! plus the per-module `HostState` and the WIT `Host` trait impls.
//!
//! Layout:
//! - [`state`]: the `HostState` struct + `WasiView` impl, the receiver
//!   every WIT `Host` trait is implemented for. `HostState` is generic
//!   over the component seam; `DefaultHostState` is the shipped assembly.
//! - `error`: From conversions and small constructors that build the WIT
//!   `HostError` shape.
//! - [`cow_orderbook`], [`provider_pool`], [`local_store_redb`]:
//!   capability backends. Pure code with no bindgen types, so each
//!   can be unit-tested without spinning up a wasmtime store.
//! - `impls` (private): the bindgen-side trait impls, one file per
//!   WIT interface, that dispatch to the backends above.
//! - [`component`]: backend traits over the capability backends, the seam a generic runtime consumes.

pub mod component;
pub mod cow_orderbook;
pub(crate) mod error;
mod impls;
pub mod local_store_redb;
pub mod provider_pool;
pub mod state;
