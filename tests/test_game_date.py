"""Tests for the shared game-date helper (4 AM PST rollover, DST-stable)."""

import datetime as dt

from utils.game_date import _PST, game_date_for


def test_game_date_boundary_is_dst_stable_across_march_transition():
    """The game-date boundary uses a fixed UTC-8 offset, so two consecutive
    days are exactly 86400s apart — even across the March DST transition, where
    a DST-aware zone would compress to 23h and drift streak math.

    (Previously guarded via commands.shop._today_4am_pst_unix, removed when the
    Regrowth shop item moved to a rolling 24h window.)
    """
    spring_fwd = dt.datetime(2026, 3, 8, 4, tzinfo=_PST)
    next_day = dt.datetime(2026, 3, 9, 4, tzinfo=_PST)
    assert int(next_day.timestamp()) - int(spring_fwd.timestamp()) == 86400


def test_game_date_for_rolls_over_at_4am_pst():
    """A timestamp just before 4 AM PST belongs to the previous game date;
    just after belongs to the current one."""
    before = dt.datetime(2026, 5, 20, 3, 59, tzinfo=_PST)
    after = dt.datetime(2026, 5, 20, 4, 1, tzinfo=_PST)
    assert game_date_for(before) == "2026-05-19"
    assert game_date_for(after) == "2026-05-20"
