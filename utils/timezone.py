"""
General per-player timezone preference.

Kept separate from utils/curfew.py so it's not curfew-specific: curfew falls
back to this when no curfew-specific override is set, and any other
time-based feature (embeds, reminders, wrapped, ...) can read the same
preference without depending on the curfew module.
"""

from zoneinfo import available_timezones

DEFAULT_TIMEZONE = "America/New_York"

# A curated subset of the ~500 valid IANA names, for /player timezone list.
# /player timezone set itself accepts any name in available_timezones(), not
# just these — this is just a friendlier menu than making players guess exact
# IANA spelling.
COMMON_TIMEZONES: dict[str, list[tuple[str, str]]] = {
    "US": [
        ("America/New_York", "Eastern"),
        ("America/Chicago", "Central"),
        ("America/Denver", "Mountain"),
        ("America/Phoenix", "Mountain, no DST (Arizona)"),
        ("America/Los_Angeles", "Pacific"),
        ("America/Anchorage", "Alaska"),
        ("Pacific/Honolulu", "Hawaii"),
    ],
    "Americas (other)": [
        ("America/Toronto", "Canada East"),
        ("America/Vancouver", "Canada West"),
        ("America/Mexico_City", "Mexico"),
        ("America/Sao_Paulo", "Brazil"),
    ],
    "Europe": [
        ("Europe/London", "UK"),
        ("Europe/Paris", "Central Europe"),
        ("Europe/Berlin", "Central Europe"),
        ("Europe/Moscow", "Russia"),
    ],
    "Asia / Pacific": [
        ("Asia/Tokyo", "Japan"),
        ("Asia/Shanghai", "China"),
        ("Asia/Kolkata", "India"),
        ("Asia/Dubai", "UAE"),
        ("Australia/Sydney", "Australia East"),
    ],
}


def is_valid_timezone(timezone: str) -> bool:
    """Return whether ``timezone`` is a real IANA zone name."""
    return timezone in available_timezones()


def format_common_timezones() -> str:
    """Render COMMON_TIMEZONES as a Discord message for /player timezone list."""
    sections = []
    for region, zones in COMMON_TIMEZONES.items():
        lines = [f"`{name}` — {label}" for name, label in zones]
        sections.append(f"**{region}**\n" + "\n".join(lines))
    body = "\n\n".join(sections)
    return (
        f"{body}\n\n"
        "Any IANA timezone name works, not just these — use `/player timezone set <name>`."
    )
