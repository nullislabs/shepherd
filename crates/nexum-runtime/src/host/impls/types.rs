//! `nexum:host/types` is a type-only interface (no functions). The
//! generated trait is empty; we just provide the marker impl.

use crate::bindings::nexum;
use crate::host::state::HostState;

impl nexum::host::types::Host for HostState {}
