"""
General per-player timezone preference.

Kept separate from utils/curfew.py so it's not curfew-specific: curfew falls
back to this when no curfew-specific override is set, and any other
time-based feature (embeds, reminders, wrapped, ...) can read the same
preference without depending on the curfew module.
"""

from zoneinfo import available_timezones

DEFAULT_TIMEZONE = "America/New_York"


def is_valid_timezone(timezone: str) -> bool:
    """Return whether ``timezone`` is a real IANA zone name."""
    return timezone in available_timezones()
