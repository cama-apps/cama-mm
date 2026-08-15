"""
Personal lobby-queue curfew: block queueing and auto-remove from any lobby
during a player-chosen daily window (e.g. 10pm-6am in their own timezone).

Kept pure here (no database/Discord dependency) so the window math — including
the overnight wraparound and DST-safe timezone conversion — is unit-testable
in isolation. Shared by the service (validation/sweep), commands (join check),
and embeds/messages (display formatting).
"""

import re
from datetime import UTC, datetime
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

from utils.timezone import DEFAULT_TIMEZONE, is_valid_timezone  # noqa: F401  (re-exported)

# Backward-compatible alias — curfew's own default, same value as the general one.
DEFAULT_CURFEW_TIMEZONE = DEFAULT_TIMEZONE

_CLOCK_RE = re.compile(r"^([01]?\d|2[0-3]):([0-5]\d)$")


def parse_clock(text: str) -> tuple[int, int]:
    """Parse a 24-hour "HH:MM" string into (hour, minute). Raises ValueError on bad format."""
    match = _CLOCK_RE.match(text.strip())
    if not match:
        raise ValueError(f"Invalid time '{text}'. Use 24-hour HH:MM, e.g. 22:00.")
    return int(match.group(1)), int(match.group(2))


def _effective_timezone(player) -> str:
    """Resolve the timezone to interpret a player's curfew in.

    Prefers a curfew-specific override, falls back to the player's general
    /player timezone setting, then to the hardcoded default.
    """
    return player.curfew_timezone or getattr(player, "timezone", None) or DEFAULT_TIMEZONE


def is_within_curfew(player, *, now: datetime | None = None) -> bool:
    """Return whether ``player`` is currently inside their curfew window.

    Disabled, or missing bedtime/wake hours, always returns False. A window
    where bedtime == wake is treated as zero-length (never blocking) rather
    than "all day" — that's almost certainly a misconfiguration, not intent.
    """
    if (
        not player.curfew_enabled
        or player.curfew_hour is None
        or player.curfew_wake_hour is None
    ):
        return False

    try:
        tz = ZoneInfo(_effective_timezone(player))
    except ZoneInfoNotFoundError:
        tz = ZoneInfo(DEFAULT_CURFEW_TIMEZONE)

    moment = (now or datetime.now(UTC)).astimezone(tz)
    minutes_now = moment.hour * 60 + moment.minute
    bedtime_minutes = player.curfew_hour * 60 + (player.curfew_minute or 0)
    wake_minutes = player.curfew_wake_hour * 60 + (player.curfew_wake_minute or 0)

    if bedtime_minutes == wake_minutes:
        return False
    if bedtime_minutes < wake_minutes:
        # Same-day window, e.g. 01:00-05:00.
        return bedtime_minutes <= minutes_now < wake_minutes
    # Overnight window, e.g. 22:00-06:00.
    return minutes_now >= bedtime_minutes or minutes_now < wake_minutes


def format_curfew_window(player) -> str:
    """Render a player's curfew window for user-facing messages, e.g. '10:00 PM - 6:00 AM America/New_York'."""
    bedtime = _format_clock(player.curfew_hour, player.curfew_minute)
    wake = _format_clock(player.curfew_wake_hour, player.curfew_wake_minute)
    return f"{bedtime} - {wake} {_effective_timezone(player)}"


def _format_clock(hour: int | None, minute: int | None) -> str:
    if hour is None:
        return "?"
    minute = minute or 0
    period = "AM" if hour < 12 else "PM"
    display_hour = hour % 12
    if display_hour == 0:
        display_hour = 12
    return f"{display_hour}:{minute:02d} {period}"
