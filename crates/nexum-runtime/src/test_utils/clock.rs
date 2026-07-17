//! Manually-driven clocks for deterministic time in tests: a WASI clock
//! for guest-visible time, and a supervisor clock for poison-window and
//! restart-backoff scheduling.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wasmtime_wasi::{HostMonotonicClock, HostWallClock};

use crate::supervisor::{SupervisorClock, WasiClockOverride};

/// A shared, manually-advanced clock source.
///
/// Cloning yields another handle onto the same instant: install one clone as
/// the store's [`WasiClockOverride`] and drive the other from the test.
/// [`set`](Self::set) pins wall time; [`advance`](Self::advance) moves both the
/// wall and monotonic sources forward. Guest-visible only; it does not touch
/// the host wall-clock a `RunId` stamps its start with.
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

    /// Pin wall time to `time`, leaving the monotonic reading untouched.
    /// Times before the Unix epoch clamp to it. Because it does not move
    /// monotonic, a `set` after an `advance` can put wall time behind the
    /// monotonic source; the two only stay in step under `advance`.
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

    /// Build a [`WasiClockOverride`] backed by this clock for both the wall and
    /// monotonic sources. The two `Arc`s wrap separate `clone`s of the same
    /// `ManualClock`, which share one inner `Arc<Mutex<_>>`, so both handles
    /// read and drive the same time. Swapping a `clone` for a fresh
    /// `ManualClock::new()` would split that state and silently break the
    /// override.
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

/// A manually-advanced [`SupervisorClock`] for deterministic poison-window
/// and restart-backoff tests. Distinct from [`ManualClock`], which
/// virtualizes guest-visible WASI time: this drives the supervisor's own
/// bookkeeping (`next_attempt`, `failure_timestamps`). Cloning yields
/// another handle onto the same instant.
///
/// Uses `std::sync::Mutex` explicitly (independent of whichever `Mutex`
/// import is in scope elsewhere in this file): a short, never-`.await`-held
/// critical section same as `ManualClock`'s.
#[derive(Clone)]
pub struct ManualInstantClock {
    inner: Arc<std::sync::Mutex<std::time::Instant>>,
}

impl Default for ManualInstantClock {
    fn default() -> Self {
        Self::new()
    }
}

impl ManualInstantClock {
    /// Seeded at the real instant this is called: `std::time::Instant`
    /// has no arbitrary constructor, so a real reading is the only way
    /// to obtain one; [`advance`](Self::advance) moves it forward from
    /// there, never backward.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(std::time::Instant::now())),
        }
    }

    /// Move the clock forward by `by`.
    pub fn advance(&self, by: Duration) {
        let mut guard = self.inner.lock().expect("manual instant clock poisoned");
        *guard += by;
    }
}

impl SupervisorClock for ManualInstantClock {
    fn now(&self) -> std::time::Instant {
        *self.inner.lock().expect("manual instant clock poisoned")
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
