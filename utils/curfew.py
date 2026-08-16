"""
Named curfew windows: block queueing and auto-remove from any lobby during a
player-chosen daily window. A player can have any number of these under
different names — the name and what it's for are entirely up to them.

Kept pure here (no database/Discord dependency) so the window math —
including the overnight wraparound and DST-safe timezone conversion — is
unit-testable in isolation. Shared by the service (validation/sweep) and
commands (join check, display formatting).
"""

import re
from datetime import UTC, datetime
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

from utils.timezone import DEFAULT_TIMEZONE

_CLOCK_RE = re.compile(r"^([01]?\d|2[0-3]):([0-5]\d)$")


def parse_clock(text: str) -> tuple[int, int]:
    """Parse a 24-hour "HH:MM" string into (hour, minute). Raises ValueError on bad format."""
    match = _CLOCK_RE.match(text.strip())
    if not match:
        raise ValueError(f"Invalid time '{text}'. Use 24-hour HH:MM, e.g. 22:00.")
    return int(match.group(1)), int(match.group(2))


def effective_timezone(window, general_timezone: str | None) -> str:
    """Resolve the timezone to interpret a window in.

    Prefers the window's own timezone, falls back to the player's general
    /player timezone setting, then to the hardcoded default.
    """
    return window.timezone or general_timezone or DEFAULT_TIMEZONE


def is_within_window(
    window, *, general_timezone: str | None = None, now: datetime | None = None
) -> bool:
    """Return whether ``now`` falls inside ``window``'s daily start-end span.

    A window where start == end is treated as zero-length (never active)
    rather than "all day" — that's almost certainly a misconfiguration.
    """
    try:
        tz = ZoneInfo(effective_timezone(window, general_timezone))
    except ZoneInfoNotFoundError:
        tz = ZoneInfo(DEFAULT_TIMEZONE)

    moment = (now or datetime.now(UTC)).astimezone(tz)
    minutes_now = moment.hour * 60 + moment.minute
    start_minutes = window.start_hour * 60 + (window.start_minute or 0)
    end_minutes = window.end_hour * 60 + (window.end_minute or 0)

    if start_minutes == end_minutes:
        return False
    if start_minutes < end_minutes:
        # Same-day span, e.g. 09:00-17:00.
        return start_minutes <= minutes_now < end_minutes
    # Overnight span, e.g. 22:00-06:00.
    return minutes_now >= start_minutes or minutes_now < end_minutes


def find_active_window(windows, *, general_timezone: str | None = None, now: datetime | None = None):
    """Return the first (by name) of ``windows`` that's currently active, or None."""
    for window in sorted(windows, key=lambda w: w.name):
        if is_within_window(window, general_timezone=general_timezone, now=now):
            return window
    return None


def format_window(window, *, general_timezone: str | None = None) -> str:
    """Render a window for user-facing messages, e.g. '"work": 9:00 AM - 5:00 PM America/New_York'."""
    start = _format_clock(window.start_hour, window.start_minute)
    end = _format_clock(window.end_hour, window.end_minute)
    tz_name = effective_timezone(window, general_timezone)
    return f'"{window.name}": {start} - {end} {tz_name}'


def _format_clock(hour: int, minute: int) -> str:
    minute = minute or 0
    period = "AM" if hour < 12 else "PM"
    display_hour = hour % 12
    if display_hour == 0:
        display_hour = 12
    return f"{display_hour}:{minute:02d} {period}"
