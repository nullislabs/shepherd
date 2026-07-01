//! Supervisor poison-pill policy.
//!
//! Modules that trap more than `max_failures` times within a sliding
//! `window` are marked **poisoned**: the supervisor stops dispatching
//! events to them entirely (no further restart attempts), bumps a
//! `shepherd_module_poisoned{module}` gauge to 1, and logs the
//! quarantine event so an operator can investigate. Recovery
//! requires an operator-driven full engine restart (today): remove
//! the entry from `engine.toml::[[modules]]`, kill the process, fix
//! the module, restart.
//!
//! ## Difference from the restart policy
//!
//! `restart_policy::backoff_for` schedules retries for transient
//! traps; the failure counter resets on a successful dispatch. The
//! poison policy is the *sustained-failure* escalation: if a module
//! is still trapping after `max_failures` retries inside `window`,
//! it stops being a transient and becomes a permanent failure that
//! exhausts an operator's restart budget without ever recovering.
//! Stop retrying.
//!
//! The two policies share `LoadedModule.failure_count` for the
//! consecutive-failure semantic; poison adds a `failure_timestamps`
//! ring so the window check is independent of how the failures are
//! spaced (one second apart vs nine minutes apart both count toward
//! the same window).

use std::time::Duration;

/// Production defaults: 5 traps within 10 minutes -> quarantine.
/// Aggressive enough to catch a deterministically broken module
/// without waiting out the full exponential backoff (the 5th trap
/// happens at ~31 s into the schedule: 1+2+4+8+16 s); lenient
/// enough that a one-off RPC blip during a real cow-api submit does
/// not get a module quarantined.
pub const POISON_MAX_FAILURES: u32 = 5;
pub const POISON_WINDOW: Duration = Duration::from_secs(600);

/// Configurable poison-pill thresholds. Constructed via
/// [`PoisonPolicy::default`] for production; tests can shorten both
/// values via [`PoisonPolicy::new`] so the integration test does
/// not have to wait out the full real-world schedule.
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

/// Return `true` when `failure_count` failures inside `window`
/// crosses the configured threshold.
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
