//! `nexum:host/clock`: wall-clock + monotonic time over the clock seam.

use crate::bindings::nexum;
use crate::host::component::{Clock, RuntimeTypes};
use crate::host::state::HostState;

impl<T: RuntimeTypes> nexum::host::clock::Host for HostState<T> {
    async fn now_ms(&mut self) -> u64 {
        self.clock.now_ms()
    }

    async fn monotonic_ns(&mut self) -> u64 {
        self.clock.monotonic_ns()
    }
}
