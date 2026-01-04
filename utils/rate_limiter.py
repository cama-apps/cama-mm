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
    """

    def __init__(self) -> None:
        # key -> list of timestamps (monotonic seconds)
        self._hits: dict[tuple[str, int, int], list[float]] = {}

    def check(
        self, *, scope: str, guild_id: int, user_id: int, limit: int, per_seconds: int
    ) -> RateLimitResult:
        now = time.monotonic()
        key = (scope, guild_id, user_id)
        window_start = now - per_seconds

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


GLOBAL_RATE_LIMITER = RateLimiter()
