"""Shared game-date helper.

Both /dig and Dota match streaks roll over at 4 AM PST so they can't drift.
"""
import datetime
import time

# Intentionally fixed UTC-8 (no DST). The "PST" game-date convention skips
# the PDT half of the year on purpose so streak math stays stable year-round
# and a single Sunday in March doesn't compress into 23 hours.
_PST = datetime.timezone(datetime.timedelta(hours=-8))


def game_date_for(dt: datetime.datetime) -> str:
    """Convert any datetime into its game-date string (4 AM PST rollover)."""
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=datetime.UTC)
    pst_dt = dt.astimezone(_PST)
    return (pst_dt - datetime.timedelta(hours=4)).strftime("%Y-%m-%d")


def get_game_date() -> str:
    """Current game date string YYYY-MM-DD. Day rolls over at 4 AM PST."""
    return game_date_for(datetime.datetime.fromtimestamp(time.time(), tz=datetime.UTC))


def yesterday_of(today: str) -> str:
    """Given a YYYY-MM-DD game-date string, return the prior day's string."""
    return (
        datetime.datetime.strptime(today, "%Y-%m-%d") - datetime.timedelta(days=1)
    ).strftime("%Y-%m-%d")


def game_date_for_timestamp(ts: float) -> str:
    """Game-date string for a unix timestamp (4 AM PST rollover)."""
    return game_date_for(datetime.datetime.fromtimestamp(ts, tz=datetime.UTC))


def weekday_of_game_date(date_str: str) -> int:
    """Weekday of a game-date string. Monday=0 … Sunday=6."""
    return datetime.datetime.strptime(date_str, "%Y-%m-%d").weekday()


def week_start_of_game_date(date_str: str) -> str:
    """The Monday game-date of the week containing ``date_str``."""
    d = datetime.datetime.strptime(date_str, "%Y-%m-%d")
    return (d - datetime.timedelta(days=d.weekday())).strftime("%Y-%m-%d")


def game_date_start_timestamp(date_str: str) -> int:
    """Unix ts when a game-date begins — 04:00 PST on that calendar date."""
    d = datetime.datetime.strptime(date_str, "%Y-%m-%d").replace(
        tzinfo=_PST
    ) + datetime.timedelta(hours=4)
    return int(d.timestamp())


def game_week_deadline_timestamp(week_start: str) -> int:
    """Unix ts of the Sunday-night deadline — start of the next week's Monday."""
    next_monday = (
        datetime.datetime.strptime(week_start, "%Y-%m-%d") + datetime.timedelta(days=7)
    ).strftime("%Y-%m-%d")
    return game_date_start_timestamp(next_monday)


def streak_bonus_for(streak_days: int, schedule: dict[int, int]) -> int:
    """Look up the JC bonus for a given streak length. Picks the largest tier ≤ streak."""
    bonus = 0
    for threshold in sorted(schedule.keys(), reverse=True):
        if streak_days >= threshold:
            bonus = schedule[threshold]
            break
    return bonus
