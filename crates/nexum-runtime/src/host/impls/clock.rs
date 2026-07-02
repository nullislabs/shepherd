//! `nexum:host/clock`: wall-clock + monotonic time.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::bindings::nexum;
use crate::host::component::{ChainProvider, CowApi, HttpClient, StateHandle};
use crate::host::state::HostState;

impl<C, W, S, H> nexum::host::clock::Host for HostState<C, W, S, H>
where
    C: ChainProvider + Send + Sync,
    W: CowApi + Send + Sync,
    S: StateHandle + Send + Sync,
    H: HttpClient + Send + Sync,
{
    async fn now_ms(&mut self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    async fn monotonic_ns(&mut self) -> u64 {
        self.monotonic_baseline.elapsed().as_nanos() as u64
    }
}
