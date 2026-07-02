//! `nexum:host/random`: fills `len` bytes from the OS CSPRNG.
//! Getrandom 0.4 failures are exceptionally rare on supported
//! platforms; on failure we log an error and return zero-filled bytes.
//! The WIT contract (`fill: func(len: u32) -> list<u8>`) has no error
//! channel, so the best we can do is make the failure observable.

use crate::bindings::nexum;
use crate::host::state::HostState;

impl nexum::host::random::Host for HostState {
    async fn fill(&mut self, len: u32) -> Vec<u8> {
        let mut buf = vec![0u8; len as usize];
        if let Err(e) = getrandom::fill(&mut buf) {
            tracing::error!(
                len,
                error = %e,
                "CSPRNG failure: getrandom::fill failed, returning zero-filled buffer — \
                 modules using this for nonces or key material may be vulnerable"
            );
        }
        buf
    }
}
