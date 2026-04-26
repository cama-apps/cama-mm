"""
Simple in-memory rate limiter for Discord interactions.

This is not meant to be a perfect security boundary (restarts reset state),
but it prevents accidental spam and protects expensive commands.
"""

from __future__ import annotations

import time
from dataclasses import dataclass


@dataclass(frozen=True)
class RateLimitResult:
    allowed: bool
    retry_after_seconds: int = 0


class RateLimiter:
    """
    Token-bucket-ish limiter: allow N events per window per key.

    Periodically purges keys whose window has fully expired so a long-lived
    bot doesn't accumulate dead entries from users who never come back.
    """

    # Sweep dead keys at most every PURGE_INTERVAL_SECONDS, and only when
    # the dict gets large enough to be worth scanning.
    PURGE_INTERVAL_SECONDS: float = 300.0
    PURGE_SIZE_THRESHOLD: int = 1024

    def __init__(self) -> None:
        # key -> list of timestamps (monotonic seconds)
        self._hits: dict[tuple[str, int, int], list[float]] = {}
        # max(per_seconds) seen; used so the purge scan knows when a key is
        # definitively cold without tracking per-key windows.
        self._max_window_seen: float = 0.0
        self._next_purge_at: float = 0.0

    def check(
        self, *, scope: str, guild_id: int, user_id: int, limit: int, per_seconds: int
    ) -> RateLimitResult:
        now = time.monotonic()
        key = (scope, guild_id, user_id)
        window_start = now - per_seconds

        if per_seconds > self._max_window_seen:
            self._max_window_seen = float(per_seconds)
        self._maybe_purge(now)

        hits = self._hits.get(key, [])
        hits = [t for t in hits if t >= window_start]

        if len(hits) >= limit:
            oldest = min(hits)
            retry_after = int(max(0.0, (oldest + per_seconds) - now) + 0.999)
            self._hits[key] = hits
            return RateLimitResult(allowed=False, retry_after_seconds=retry_after)

        hits.append(now)
        self._hits[key] = hits
        return RateLimitResult(allowed=True, retry_after_seconds=0)

    def _maybe_purge(self, now: float) -> None:
        if now < self._next_purge_at:
            return
        self._next_purge_at = now + self.PURGE_INTERVAL_SECONDS
        if len(self._hits) < self.PURGE_SIZE_THRESHOLD:
            return
        cutoff = now - self._max_window_seen
        # Drop keys whose newest hit is older than any observed window.
        self._hits = {
            key: hits for key, hits in self._hits.items()
            if hits and hits[-1] >= cutoff
        }


GLOBAL_RATE_LIMITER = RateLimiter()
