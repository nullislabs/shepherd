//! Supervisor poison-pill policy.
//!
//! A module reaching `max_failures` traps within a sliding `window` is
//! poisoned: the supervisor stops dispatching to it (no further restarts),
//! sets the `shepherd_module_poisoned{module}` gauge to 1, and logs the
//! quarantine. Recovery needs an operator-driven full engine restart.

use std::time::Duration;

/// Production defaults: 5 traps within 10 minutes quarantines a module.
pub const POISON_MAX_FAILURES: u32 = 5;
pub const POISON_WINDOW: Duration = Duration::from_secs(600);

/// Configurable poison-pill thresholds from `[limits.poison]`, else
/// [`PoisonPolicy::default`].
#[derive(Debug, Clone, Copy)]
pub struct PoisonPolicy {
    /// Maximum traps within `window` before the module is poisoned.
    pub max_failures: u32,
    /// Sliding window the failures are counted across.
    pub window: Duration,
}

impl PoisonPolicy {
    pub const fn new(max_failures: u32, window: Duration) -> Self {
        Self {
            max_failures,
            window,
        }
    }
}

impl Default for PoisonPolicy {
    fn default() -> Self {
        Self::new(POISON_MAX_FAILURES, POISON_WINDOW)
    }
}

/// `true` when the recent-failure count crosses the configured threshold.
pub fn should_poison(policy: PoisonPolicy, recent_failures: u32) -> bool {
    recent_failures >= policy.max_failures
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_production_constants() {
        let p = PoisonPolicy::default();
        assert_eq!(p.max_failures, POISON_MAX_FAILURES);
        assert_eq!(p.window, POISON_WINDOW);
    }

    #[test]
    fn poisons_at_threshold() {
        let p = PoisonPolicy::new(3, Duration::from_secs(60));
        assert!(!should_poison(p, 0));
        assert!(!should_poison(p, 2));
        assert!(should_poison(p, 3));
        assert!(should_poison(p, 100));
    }
}
