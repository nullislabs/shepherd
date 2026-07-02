//! Clock seam: wall-clock millis plus a monotonic origin, mirroring
//! the direct SystemTime / Instant calls in the clock host impl.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Time source for `clock::now-ms` / `clock::monotonic-ns`.
pub trait Clock {
    /// Milliseconds since the Unix epoch; 0 if the system clock is
    /// before the epoch.
    fn now_ms(&self) -> u64;
    /// Nanoseconds since this clock's construction-time origin.
    fn monotonic_ns(&self) -> u64;
}

/// Default clock: `SystemTime::now` plus an `Instant` origin captured
/// at construction, identical to today's `monotonic_baseline` field.
#[derive(Debug, Clone, Copy)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    /// Capture the monotonic origin now.
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    fn monotonic_ns(&self) -> u64 {
        self.origin.elapsed().as_nanos() as u64
    }
}
