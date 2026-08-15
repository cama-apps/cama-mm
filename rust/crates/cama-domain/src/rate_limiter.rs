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
}
