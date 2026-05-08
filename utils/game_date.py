"""Shared game-date helper.

Both /dig and Dota match streaks roll over at 4 AM PST so they can't drift.
"""
import datetime
import time

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


def streak_bonus_for(streak_days: int, schedule: dict[int, int]) -> int:
    """Look up the JC bonus for a given streak length. Picks the largest tier ≤ streak."""
    bonus = 0
    for threshold in sorted(schedule.keys(), reverse=True):
        if streak_days >= threshold:
            bonus = schedule[threshold]
            break
    return bonus
