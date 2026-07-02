//! `nexum:host/types` is a type-only interface (no functions). The
//! generated trait is empty; we just provide the marker impl.

use crate::bindings::nexum;
use crate::host::component::{ChainProvider, CowApi, HttpClient, StateHandle};
use crate::host::state::HostState;

impl<C, W, S, H> nexum::host::types::Host for HostState<C, W, S, H>
where
    C: ChainProvider + Send + Sync,
    W: CowApi + Send + Sync,
    S: StateHandle + Send + Sync,
    H: HttpClient + Send + Sync,
{
}
