"""
Shared HTTP safety helpers for external API clients.

This module centralizes defensive logic that is reused across the OpenDota and
Steam/Valve HTTP clients. Keeping the helper in one place means both clients
apply the same body-size ceiling and the same malformed-JSON handling, so a
misbehaving upstream can't slip past one client while being caught by the
other.
"""

from __future__ import annotations

import email.utils
import json
import logging
from datetime import UTC, datetime

import requests

logger = logging.getLogger("cama_bot.http_safety")

# Default cap on response body size (bytes). 5 MB is generous for JSON payloads
# from OpenDota/Valve but small enough to guard against pathological replies.
DEFAULT_MAX_BYTES = 5 * 1024 * 1024


def parse_json_bounded(
    response: requests.Response,
    context: str,
    *,
    max_bytes: int = DEFAULT_MAX_BYTES,
):
    """Parse a JSON response body, returning ``None`` on malformed JSON or
    oversized payloads instead of raising.

    Enforces a post-hoc ceiling on response size:
    - If ``Content-Length`` is present and exceeds ``max_bytes``, reject up
      front.
    - Otherwise, measure ``response.content`` (which is the fully-buffered
      body) and reject if it exceeds ``max_bytes``. Because ``.content`` is
      already in memory by the time we see it, this is a late check rather
      than a true streaming bound — see ``opendota_integration.py`` callers
      for context.

    Args:
        response: A ``requests.Response`` from a completed GET.
        context: A short human-readable label for logs (e.g., ``"match 123"``).
        max_bytes: Maximum body size to accept. Defaults to
            :data:`DEFAULT_MAX_BYTES`.

    Returns:
        The decoded JSON body, or ``None`` if the body was malformed or
        exceeded the size cap.
    """
    content_length = response.headers.get("Content-Length")
    if content_length is not None:
        try:
            if int(content_length) > max_bytes:
                logger.warning(
                    f"HTTP response for {context} too large "
                    f"(Content-Length={content_length}); rejecting"
                )
                return None
        except ValueError:
            # Malformed Content-Length header — fall through to body check.
            pass

    body = response.content
    if len(body) > max_bytes:
        logger.warning(
            f"HTTP response for {context} too large "
            f"({len(body)} bytes); rejecting"
        )
        return None

    try:
        return response.json()
    except (requests.exceptions.JSONDecodeError, json.JSONDecodeError, ValueError) as e:
        logger.error(f"HTTP response for {context} was malformed JSON: {e}")
        return None


def retry_after_seconds(response: requests.Response) -> int | None:
    """Parse a ``Retry-After`` header into a non-negative integer delay.

    RFC 7231 permits either a delta-seconds integer or an HTTP-date. We honor
    both; any unparseable value returns ``None`` so callers can fall back to
    their own backoff schedule.

    Args:
        response: A ``requests.Response`` to inspect for the header.

    Returns:
        Integer seconds to wait, or ``None`` if the header is missing or
        cannot be parsed.
    """
    raw = response.headers.get("Retry-After")
    if raw is None:
        return None

    # Integer form: delta-seconds.
    try:
        seconds = int(raw)
    except (TypeError, ValueError):
        pass
    else:
        return max(seconds, 0)

    # HTTP-date form: parse into a datetime and compute a delta from now.
    try:
        parsed = email.utils.parsedate_to_datetime(raw)
    except (TypeError, ValueError):
        return None
    if parsed is None:
        return None

    # Naive datetimes come back in local time per RFC 7231; assume UTC to be
    # safe (the spec requires GMT).
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=UTC)

    delta = (parsed - datetime.now(tz=UTC)).total_seconds()
    if delta <= 0:
        return 0
    return int(delta)
