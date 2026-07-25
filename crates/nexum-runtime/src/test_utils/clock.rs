//! A manually-driven WASI clock for deterministic guest time in tests.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wasmtime_wasi::{HostMonotonicClock, HostWallClock};

use crate::supervisor::WasiClockOverride;

/// A shared, manually-advanced clock source. Cloning yields another handle
/// onto the same instant: install one clone as a store's
/// [`WasiClockOverride`], drive the other from the test. [`set`](Self::set)
/// pins wall time; [`advance`](Self::advance) moves wall and monotonic
/// together. Guest-visible only.
#[derive(Clone)]
pub struct ManualClock {
    inner: Arc<Mutex<Instant>>,
}

/// The two time sources a manual clock tracks.
struct Instant {
    /// Wall time as a duration since the Unix epoch.
    wall: Duration,
    /// Monotonic time as nanoseconds since clock creation.
    monotonic: u64,
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::new()
    }
}

impl ManualClock {
    /// Start at the Unix epoch with a zero monotonic reading.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Instant {
                wall: Duration::ZERO,
                monotonic: 0,
            })),
        }
    }

    /// Pin wall time to `time` (clamped to the Unix epoch), leaving monotonic
    /// untouched; a `set` after an `advance` can put wall behind monotonic.
    pub fn set(&self, time: SystemTime) {
        let wall = time.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
        self.locked().wall = wall;
    }

    /// Move both the wall and monotonic sources forward by `by`.
    pub fn advance(&self, by: Duration) {
        let nanos = u64::try_from(by.as_nanos()).unwrap_or(u64::MAX);
        let mut state = self.locked();
        state.wall = state.wall.saturating_add(by);
        state.monotonic = state.monotonic.saturating_add(nanos);
    }

    /// A [`WasiClockOverride`] over this clock for both sources. Both `Arc`s
    /// share this clock's inner state; a fresh `ManualClock::new()` would
    /// split it and break the override.
    pub fn as_override(&self) -> WasiClockOverride {
        WasiClockOverride::new(Arc::new(self.clone()), Arc::new(self.clone()))
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, Instant> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl HostWallClock for ManualClock {
    fn resolution(&self) -> Duration {
        Duration::from_nanos(1)
    }

    fn now(&self) -> Duration {
        self.locked().wall
    }
}

impl HostMonotonicClock for ManualClock {
    fn resolution(&self) -> u64 {
        1
    }

    fn now(&self) -> u64 {
        self.locked().monotonic
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_pins_wall_time_and_leaves_monotonic() {
        let clock = ManualClock::new();
        let target = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        clock.set(target);
        assert_eq!(
            HostWallClock::now(&clock),
            Duration::from_secs(1_700_000_000)
        );
        assert_eq!(HostMonotonicClock::now(&clock), 0);
    }

    #[test]
    fn advance_moves_both_sources() {
        let clock = ManualClock::new();
        clock.set(UNIX_EPOCH + Duration::from_secs(100));
        clock.advance(Duration::from_secs(5));
        assert_eq!(HostWallClock::now(&clock), Duration::from_secs(105));
        assert_eq!(
            HostMonotonicClock::now(&clock),
            Duration::from_secs(5).as_nanos() as u64
        );
    }

    #[test]
    fn clones_share_one_instant() {
        let a = ManualClock::new();
        let b = a.clone();
        a.advance(Duration::from_millis(250));
        assert_eq!(HostMonotonicClock::now(&b), 250_000_000);
    }

    #[test]
    fn pre_epoch_time_clamps_to_zero() {
        let clock = ManualClock::new();
        clock.set(UNIX_EPOCH - Duration::from_secs(10));
        assert_eq!(HostWallClock::now(&clock), Duration::ZERO);
    }
}
