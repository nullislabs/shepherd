//! Host-side backends for the `nexum:host` interfaces, plus the per-module
//! [`state::HostState`] and the WIT `Host` trait impls.
//!
//! [`provider_pool`] and [`local_store_redb`] are the capability backends;
//! [`component`] is the backend-trait seam; [`extension`] wires in domain
//! extensions; [`actor`] supervises provider instances; [`http`] gates
//! outgoing wasi:http; [`logs`] is the module-log pipeline; [`error`] projects
//! backend errors into the WIT `chain-error` / `Fault` shapes.

pub mod actor;
pub mod component;
pub mod error;
pub mod extension;
pub mod http;
mod impls;
pub mod local_store_redb;
pub mod logs;
pub mod provider_pool;
pub mod state;
