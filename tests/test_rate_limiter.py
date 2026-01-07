"""
Tests for RateLimiter.
"""

from utils.rate_limiter import RateLimiter


def test_rate_limiter_allows_within_limit(monkeypatch):
    limiter = RateLimiter()
    times = iter([0.0, 1.0])

    monkeypatch.setattr("utils.rate_limiter.time.monotonic", lambda: next(times))

    result1 = limiter.check(
        scope="test",
        guild_id=1,
        user_id=2,
        limit=2,
        per_seconds=10,
    )
    result2 = limiter.check(
        scope="test",
        guild_id=1,
        user_id=2,
        limit=2,
        per_seconds=10,
    )

    assert result1.allowed is True
    assert result2.allowed is True


def test_rate_limiter_blocks_and_sets_retry(monkeypatch):
    limiter = RateLimiter()
    times = iter([0.0, 1.0, 2.0])

    monkeypatch.setattr("utils.rate_limiter.time.monotonic", lambda: next(times))

    limiter.check(scope="test", guild_id=1, user_id=2, limit=2, per_seconds=10)
    limiter.check(scope="test", guild_id=1, user_id=2, limit=2, per_seconds=10)
    blocked = limiter.check(scope="test", guild_id=1, user_id=2, limit=2, per_seconds=10)

    assert blocked.allowed is False
    assert blocked.retry_after_seconds == 8


def test_rate_limiter_allows_after_window(monkeypatch):
    limiter = RateLimiter()
    times = iter([0.0, 1.0, 11.0])

    monkeypatch.setattr("utils.rate_limiter.time.monotonic", lambda: next(times))

    limiter.check(scope="test", guild_id=1, user_id=2, limit=2, per_seconds=10)
    limiter.check(scope="test", guild_id=1, user_id=2, limit=2, per_seconds=10)
    allowed = limiter.check(scope="test", guild_id=1, user_id=2, limit=2, per_seconds=10)

    assert allowed.allowed is True
