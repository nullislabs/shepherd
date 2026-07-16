//! Per-module dispatch rate limiter: one token bucket per module, checked
//! before `on_event`, drops over-rate events. Caps how often a dispatch
//! starts (fuel/memory/poison cap what one costs); per-module, so a flood
//! cannot starve other modules. Pure with injected time.

use std::time::Instant;

/// Token-bucket thresholds from `[limits.dispatch]`, else
/// [`DispatchRatePolicy::default`].
#[derive(Debug, Clone, Copy)]
pub struct DispatchRatePolicy {
    /// Bucket capacity: the burst allowance. One dispatch consumes one token.
    pub capacity: u32,
    /// Tokens replenished per second: the sustained ceiling.
    pub refill_per_sec: u32,
}

impl DispatchRatePolicy {
    pub const fn new(capacity: u32, refill_per_sec: u32) -> Self {
        Self {
            capacity,
            refill_per_sec,
        }
    }
}

impl Default for DispatchRatePolicy {
    fn default() -> Self {
        Self::new(DEFAULT_DISPATCH_BURST, DEFAULT_DISPATCH_REFILL_PER_SEC)
    }
}

/// Production default burst allowance.
pub const DEFAULT_DISPATCH_BURST: u32 = 256;

/// Production default sustained ceiling, in dispatches per second.
pub const DEFAULT_DISPATCH_REFILL_PER_SEC: u32 = 128;

/// Per-module token-bucket state; fractional tokens, starts full.
#[derive(Debug)]
pub struct TokenBucket {
    policy: DispatchRatePolicy,
    /// Current tokens in `[0, capacity]`; fractional so slow refill is not lost.
    tokens: f64,
    last_refill: Instant,
}

impl TokenBucket {
    /// A bucket that starts full at `policy.capacity`, as of `now`.
    pub fn new(policy: DispatchRatePolicy, now: Instant) -> Self {
        Self {
            policy,
            tokens: f64::from(policy.capacity),
            last_refill: now,
        }
    }

    /// Refill for elapsed time, then consume one token. `true` = allowed,
    /// `false` = over-rate. `now` is injected to stay pure and testable.
    pub fn try_acquire(&mut self, now: Instant) -> bool {
        let capacity = f64::from(self.policy.capacity);
        let elapsed = now
            .saturating_duration_since(self.last_refill)
            .as_secs_f64();
        self.tokens = (self.tokens + elapsed * f64::from(self.policy.refill_per_sec)).min(capacity);
        self.last_refill = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn default_is_production_constants() {
        let p = DispatchRatePolicy::default();
        assert_eq!(p.capacity, DEFAULT_DISPATCH_BURST);
        assert_eq!(p.refill_per_sec, DEFAULT_DISPATCH_REFILL_PER_SEC);
    }

    #[test]
    fn bucket_starts_full_and_allows_a_burst_up_to_capacity() {
        let now = Instant::now();
        let mut bucket = TokenBucket::new(DispatchRatePolicy::new(3, 1), now);
        // Three dispatches in the same instant clear the burst allowance.
        assert!(bucket.try_acquire(now));
        assert!(bucket.try_acquire(now));
        assert!(bucket.try_acquire(now));
        // The fourth over-rate event in the same instant is dropped.
        assert!(!bucket.try_acquire(now));
    }

    #[test]
    fn empty_bucket_refills_over_time() {
        let start = Instant::now();
        let mut bucket = TokenBucket::new(DispatchRatePolicy::new(2, 4), start);
        // Drain the burst.
        assert!(bucket.try_acquire(start));
        assert!(bucket.try_acquire(start));
        assert!(!bucket.try_acquire(start), "burst exhausted");
        // 4 tokens/s means one token is back after 250 ms.
        let later = start + Duration::from_millis(250);
        assert!(bucket.try_acquire(later), "one token refilled after 250ms");
        assert!(!bucket.try_acquire(later), "only one token had refilled");
    }

    #[test]
    fn refill_never_exceeds_capacity() {
        let start = Instant::now();
        let mut bucket = TokenBucket::new(DispatchRatePolicy::new(2, 100), start);
        assert!(bucket.try_acquire(start));
        assert!(bucket.try_acquire(start));
        // A long idle would refill 100 tokens/s, but the bucket caps at
        // capacity: only `capacity` dispatches are allowed back-to-back.
        let much_later = start + Duration::from_secs(10);
        assert!(bucket.try_acquire(much_later));
        assert!(bucket.try_acquire(much_later));
        assert!(
            !bucket.try_acquire(much_later),
            "burst is capped at capacity, not the whole idle refill",
        );
    }

    /// The acceptance criterion at the policy layer: a flooding source is
    /// throttled while a second, independent source keeps being served.
    #[test]
    fn one_flooding_bucket_does_not_starve_another() {
        let now = Instant::now();
        let policy = DispatchRatePolicy::new(2, 1);
        let mut flooder = TokenBucket::new(policy, now);
        let mut neighbour = TokenBucket::new(policy, now);

        // Hammer the flooder in a single instant: the first `capacity`
        // dispatches pass, the rest are dropped.
        let mut allowed = 0;
        for _ in 0..100 {
            if flooder.try_acquire(now) {
                allowed += 1;
            }
        }
        assert_eq!(allowed, 2, "flooder is throttled to its burst allowance");
        assert!(!flooder.try_acquire(now), "flooder stays throttled");

        // The neighbour's bucket is untouched by the flood: it still
        // serves its own full burst.
        assert!(neighbour.try_acquire(now));
        assert!(neighbour.try_acquire(now));
    }
}
