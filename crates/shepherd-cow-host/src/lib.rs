//! The cow-api host extension: `shepherd:cow/cow-api` wired into the nexum
//! runtime through the linker extension seam.
//!
//! The core runtime knows nothing of CoW Protocol; this crate owns the
//! `cowprotocol` dependency, the `shepherd:cow/cow-ext` bindgen, the
//! orderbook backend, and the [`Extension`](nexum_runtime::host::extension::Extension)
//! value the composition root assembles into the linker and capability
//! registry. It depends on the runtime (for `HostState`, the extension
//! seam, and the shared `nexum:host/types` bindgen); the runtime never
//! depends on it, so the CoW cone stays out of the bare engine.

mod cow;
pub mod cow_orderbook;
pub mod ext_cow;

pub use cow::CowApi;
pub use cow_orderbook::{CowApiError, OrderBookPool};
pub use ext_cow::{CowBackend, ReferenceExt, extension};
