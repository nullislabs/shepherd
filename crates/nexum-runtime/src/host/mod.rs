//! Host-side backends for the `nexum:host` / `shepherd:cow` interfaces,
//! plus the per-module `HostState` and the WIT `Host` trait impls.
//!
//! Layout:
//! - [`state`]: `HostState` struct + `WasiView` impl, the receiver
//!   every WIT `Host` trait is implemented for.
//! - `error`: small constructors that build the WIT `HostError`
//!   shape (`unimplemented`, `internal_error`).
//! - [`cow_orderbook`], [`provider_pool`], [`local_store_redb`]:
//!   capability backends. Pure code with no bindgen types, so each
//!   can be unit-tested without spinning up a wasmtime store.
//! - `impls` (private): the bindgen-side trait impls, one file per
//!   WIT interface, that dispatch to the backends above.

pub mod cow_orderbook;
pub(crate) mod error;
mod impls;
pub mod local_store_redb;
pub mod provider_pool;
pub mod state;
