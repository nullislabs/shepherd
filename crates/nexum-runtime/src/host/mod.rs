//! Host-side backends for the `nexum:host` interfaces, plus the
//! per-module `HostState` and the WIT `Host` trait impls.
//!
//! Layout:
//! - [`state`]: the `HostState` struct + `WasiView` impl, the receiver
//!   every WIT `Host` trait is implemented for. `HostState` is generic
//!   over the `RuntimeTypes` lattice; the composition root supplies the
//!   concrete assembly.
//! - [`error`]: From conversions and small constructors that build the WIT
//!   `HostError` shape. The constructors are `pub` so extension crates
//!   projecting their own backend errors reuse the same shapes.
//! - [`provider_pool`], [`local_store_redb`]: capability backends. Pure
//!   code with no bindgen types, so each can be unit-tested without
//!   spinning up a wasmtime store.
//! - `impls` (private): the bindgen-side trait impls, one file per core
//!   WIT interface, that dispatch to the backends above.
//! - [`component`]: backend traits over the capability backends, the seam a generic runtime consumes.
//! - [`extension`]: the extension seam (linker hook + capability
//!   namespace) an extension is wired in through at the composition root.
//!   Domain extensions such as cow-api live in their own crates and plug
//!   in through this seam rather than being hard-linked into the core host.

pub mod component;
pub mod error;
pub mod extension;
mod impls;
pub mod local_store_redb;
pub mod provider_pool;
pub mod state;
