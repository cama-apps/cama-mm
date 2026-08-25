//! In-memory fixed-window interaction rate limiting.

use std::collections::HashMap;

pub const PURGE_INTERVAL_SECONDS: f64 = 300.0;
pub const PURGE_SIZE_THRESHOLD: usize = 1_024;

/// The decision returned for one rate-limit check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RateLimitResult {
    pub allowed: bool,
    pub retry_after_seconds: u64,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RateLimitKey {
    scope: String,
    guild_id: i64,
    user_id: i64,
}

/// Process-local rate limiter. Restarts deliberately reset all state.
#[derive(Debug, Default)]
pub struct RateLimiter {
    hits: HashMap<RateLimitKey, Vec<f64>>,
    max_window_seen: f64,
    next_purge_at: f64,
}

impl RateLimiter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Check an event using an injected monotonic timestamp.
    ///
    /// The runtime adapter owns the clock; keeping it out of this type makes the
    /// Python/Rust behavioral comparison deterministic.
    pub fn check_at(
        &mut self,
        now: f64,
        scope: impl Into<String>,
        guild_id: i64,
        user_id: i64,
        limit: usize,
        per_seconds: u64,
    ) -> RateLimitResult {
        let per_seconds = per_seconds as f64;
        let window_start = now - per_seconds;

        self.max_window_seen = self.max_window_seen.max(per_seconds);
        self.maybe_purge(now);

        let key = RateLimitKey {
            scope: scope.into(),
            guild_id,
            user_id,
        };
        let hits = self.hits.entry(key).or_default();
        hits.retain(|timestamp| *timestamp >= window_start);

        if hits.len() >= limit {
            let oldest = hits.iter().copied().fold(f64::INFINITY, f64::min);
            let retry_after_seconds = ((oldest + per_seconds) - now).max(0.0).ceil() as u64;
            return RateLimitResult {
                allowed: false,
                retry_after_seconds,
            };
        }

        hits.push(now);
        RateLimitResult {
            allowed: true,
            retry_after_seconds: 0,
        }
    }

    /// Remove a previously-recorded hit at the exact injected timestamp.
    ///
    /// Callers can use this to claim capacity before a fallible operation and
    /// refund the claim when the operation does not ultimately consume the
    /// rate-limited action. Hits at the same timestamp are interchangeable,
    /// so removing the newest match also handles deterministic/concurrent
    /// callers that share an injected clock value.
    pub fn refund_at(
        &mut self,
        now: f64,
        scope: impl Into<String>,
        guild_id: i64,
        user_id: i64,
    ) -> bool {
        let key = RateLimitKey {
            scope: scope.into(),
            guild_id,
            user_id,
        };
        let Some(hits) = self.hits.get_mut(&key) else {
            return false;
        };
        let Some(index) = hits.iter().rposition(|timestamp| *timestamp == now) else {
            return false;
        };
        hits.remove(index);
        if hits.is_empty() {
            self.hits.remove(&key);
        }
        true
    }

    fn maybe_purge(&mut self, now: f64) {
        if now < self.next_purge_at {
            return;
        }
        self.next_purge_at = now + PURGE_INTERVAL_SECONDS;
        if self.hits.len() < PURGE_SIZE_THRESHOLD {
            return;
        }

        let cutoff = now - self.max_window_seen;
        self.hits.retain(|_, hits| {
            hits.last()
                .is_some_and(|newest_timestamp| *newest_timestamp >= cutoff)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::RateLimiter;

    #[test]
    fn test_rate_limiter_allows_within_limit() {
        let mut limiter = RateLimiter::new();
        let first = limiter.check_at(0.0, "test", 1, 2, 2, 10);
        let second = limiter.check_at(1.0, "test", 1, 2, 2, 10);
        assert!(first.allowed);
        assert!(second.allowed);
    }

    #[test]
    fn test_rate_limiter_blocks_and_sets_retry() {
        let mut limiter = RateLimiter::new();
        limiter.check_at(0.0, "test", 1, 2, 2, 10);
        limiter.check_at(1.0, "test", 1, 2, 2, 10);
        let blocked = limiter.check_at(2.0, "test", 1, 2, 2, 10);
        assert!(!blocked.allowed);
        assert_eq!(blocked.retry_after_seconds, 8);
    }

    #[test]
    fn test_rate_limiter_allows_after_window() {
        let mut limiter = RateLimiter::new();
        limiter.check_at(0.0, "test", 1, 2, 2, 10);
        limiter.check_at(1.0, "test", 1, 2, 2, 10);
        let allowed = limiter.check_at(11.0, "test", 1, 2, 2, 10);
        assert!(allowed.allowed);
    }

    #[test]
    fn test_rate_limiter_refund_restores_capacity_for_only_the_claimed_key() {
        let mut limiter = RateLimiter::new();
        assert!(limiter.check_at(1.0, "test", 1, 2, 1, 10).allowed);
        assert!(!limiter.check_at(2.0, "test", 1, 2, 1, 10).allowed);

        assert!(!limiter.refund_at(9.0, "test", 1, 2));
        assert!(!limiter.refund_at(1.0, "other", 1, 2));
        assert!(limiter.refund_at(1.0, "test", 1, 2));
        assert!(limiter.check_at(2.0, "test", 1, 2, 1, 10).allowed);
    }

    #[test]
    fn test_three_event_window_is_isolated_by_user_and_guild() {
        let mut limiter = RateLimiter::new();
        for now in [0.0, 1.0, 2.0] {
            assert!(limiter.check_at(now, "lobby", 10, 20, 3, 30).allowed);
        }
        assert!(!limiter.check_at(3.0, "lobby", 10, 20, 3, 30).allowed);

        assert!(limiter.check_at(3.0, "lobby", 10, 21, 3, 30).allowed);
        assert!(limiter.check_at(3.0, "lobby", 11, 20, 3, 30).allowed);
        assert!(limiter.check_at(31.0, "lobby", 10, 20, 3, 30).allowed);
    }
}
