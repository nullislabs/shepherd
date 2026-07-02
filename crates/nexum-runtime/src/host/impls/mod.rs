//! `Host` trait impls for [`crate::host::state::HostState`], one
//! file per WIT interface.
//!
//! The interfaces themselves (and their generated trait shapes) live
//! in [`crate::bindings`]; this module only contains the dispatch
//! glue between the WIT signature and the corresponding backend in
//! [`crate::host`].

mod chain;
mod clock;
mod cow_api;
mod http;
mod identity;
mod local_store;
mod logging;
mod messaging;
mod random;
mod remote_store;
mod types;
