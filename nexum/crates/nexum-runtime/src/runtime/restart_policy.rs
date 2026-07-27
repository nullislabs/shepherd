//! Supervisor module restart policy.
//!
//! On a trap in `on_event` the supervisor marks the module dead and schedules
//! a restart with exponential backoff; the next eligible dispatch retries, and
//! a successful call resets the failure counter.
//!
//! | failure_count | next_attempt delay |
//! |---|---|
//! | 1 | 1s |
//! | 2 | 2s |
//! | 3 | 4s |
//! | ... | doubles |
//! | 9+ | capped at 5 minutes |
//!
//! State is in-memory per process; it does not persist across restarts.

use std::time::Duration;

/// Hard cap on the restart backoff.
pub const RESTART_MAX_BACKOFF: Duration = Duration::from_secs(300);

/// Backoff before the next restart of a module that trapped `failure_count`
/// times in a row. `0` returns `Duration::ZERO` (steady state, callable
/// unconditionally); `>= 1` is 1 s doubling per trap, capped at 5 min.
pub fn backoff_for(failure_count: u32) -> Duration {
    if failure_count == 0 {
        return Duration::ZERO;
    }
    // 1 << (n - 1) doubles: 1, 2, 4, 8, 16, ..., 256 at n=9.
    // saturating_sub keeps n=1 -> 1s; the .min(9) keeps the shift
    // from overflowing on absurdly large failure counts.
    let shift = failure_count.saturating_sub(1).min(9);
    let secs = 1u64 << shift;
    Duration::from_secs(secs).min(RESTART_MAX_BACKOFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steady_state_is_zero() {
        assert_eq!(backoff_for(0), Duration::ZERO);
    }

    #[test]
    fn first_failure_waits_one_second() {
        assert_eq!(backoff_for(1), Duration::from_secs(1));
    }

    #[test]
    fn doubling_progression() {
        assert_eq!(backoff_for(2), Duration::from_secs(2));
        assert_eq!(backoff_for(3), Duration::from_secs(4));
        assert_eq!(backoff_for(4), Duration::from_secs(8));
        assert_eq!(backoff_for(5), Duration::from_secs(16));
    }

    #[test]
    fn caps_at_five_minutes() {
        assert_eq!(backoff_for(20), RESTART_MAX_BACKOFF);
        assert_eq!(backoff_for(u32::MAX), RESTART_MAX_BACKOFF);
    }
}
