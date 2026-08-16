"""
Personal dota play-time preference: purely informational, never enforced.

Unlike curfew, this never blocks or removes anyone from a lobby — it just
lets players record which hours they'd like to play so /player playtime
popular can report the group's most desired hours. Each player's hours are
interpreted in their own timezone (their /player timezone setting, or the
default) and converted into one reporting timezone before being counted, so
the aggregate is an honest cross-timezone comparison.
"""

import re
from datetime import date, datetime
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

from utils.timezone import DEFAULT_TIMEZONE

_HOUR_TOKEN_RE = re.compile(r"^(\d{1,2})(?:-(\d{1,2}))?$")


def parse_hour_set(text: str) -> set[int]:
    """Parse a comma-separated list of hours and/or hour ranges into a set of hours (0-23).

    Accepts single hours ("14"), ranges ("18-23"; wrapping past midnight is
    fine, e.g. "22-2"), or any comma-separated mix ("14,18-23,2").
    """
    text = text.strip()
    if not text:
        raise ValueError("Give at least one hour, e.g. '18-23' or '14,20,21'.")
    hours: set[int] = set()
    for raw_token in text.split(","):
        token = raw_token.strip()
        match = _HOUR_TOKEN_RE.match(token)
        if not match:
            raise ValueError(
                f"Invalid hour or range '{token}'. Use hours 0-23, e.g. '18-23' or '14,20,21'."
            )
        start = int(match.group(1))
        end = int(match.group(2)) if match.group(2) else start
        if not (0 <= start <= 23) or not (0 <= end <= 23):
            raise ValueError(f"Hours must be between 0 and 23 (got '{token}').")
        hour = start
        hours.add(hour)
        while hour != end:
            hour = (hour + 1) % 24
            hours.add(hour)
    return hours


def format_hour_ranges(hours: set[int] | list[int]) -> str:
    """Render a set of hours as compact ascending ranges, e.g. {18,19,20,23} -> '18:00-20:00, 23:00'."""
    if not hours:
        return "none"
    ordered = sorted(set(hours))
    ranges: list[tuple[int, int]] = []
    start = prev = ordered[0]
    for hour in ordered[1:]:
        if hour == prev + 1:
            prev = hour
            continue
        ranges.append((start, prev))
        start = prev = hour
    ranges.append((start, prev))
    return ", ".join(
        f"{s:02d}:00" if s == e else f"{s:02d}:00-{e:02d}:00" for s, e in ranges
    )


def effective_timezone(player) -> str:
    """The timezone to interpret a player's play-time hours in: their general setting, else the default."""
    return getattr(player, "timezone", None) or DEFAULT_TIMEZONE


def aggregate_popular_hours(
    players, *, reporting_timezone: str = DEFAULT_TIMEZONE, today: date | None = None
) -> list[int]:
    """Count, per hour (0-23) in ``reporting_timezone``, how many players want to play then."""
    try:
        report_tz = ZoneInfo(reporting_timezone)
    except ZoneInfoNotFoundError:
        report_tz = ZoneInfo(DEFAULT_TIMEZONE)
    reference_date = today or datetime.now(report_tz).date()

    counts = [0] * 24
    for player in players:
        hours = getattr(player, "dota_play_hours", None)
        if not hours:
            continue
        try:
            player_tz = ZoneInfo(effective_timezone(player))
        except ZoneInfoNotFoundError:
            player_tz = ZoneInfo(DEFAULT_TIMEZONE)
        for hour in hours:
            local_dt = datetime(
                reference_date.year, reference_date.month, reference_date.day, hour, tzinfo=player_tz
            )
            counts[local_dt.astimezone(report_tz).hour] += 1
    return counts
