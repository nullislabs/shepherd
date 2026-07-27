//! `Host` trait impls for [`crate::host::state::HostState`], one file per WIT
//! interface: dispatch glue to the backends in [`crate::host`].

mod chain;
mod identity;
mod local_store;
mod logging;
mod messaging;
mod remote_store;
mod types;
