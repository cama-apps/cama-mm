"""Tests for the Dota daily-play streak: tier lookup, advance/reset semantics,
and the per-(player, guild) repository state machine."""
from __future__ import annotations

import time

import pytest

from repositories.player_repository import PlayerRepository
from services.dig_constants import STREAKS
from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY
from utils.game_date import (
    game_date_for,
    get_game_date,
    streak_bonus_for,
    yesterday_of,
)


def _register(repo: PlayerRepository, discord_id: int, guild_id: int = TEST_GUILD_ID):
    repo.add(
        discord_id=discord_id,
        discord_username=f"P{discord_id}",
        guild_id=guild_id,
        initial_mmr=1500,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )


class TestStreakSchedule:
    def test_unified_schedule_values(self):
        # Source of truth: dig and Dota both consume STREAKS, so any drift
        # would silently change Dota payouts too.
        assert STREAKS == {3: 1, 7: 3, 14: 6, 30: 10}

    @pytest.mark.parametrize(
        "streak_days,expected",
        [
            (0, 0),
            (1, 0),
            (2, 0),
            (3, 1),
            (6, 1),
            (7, 3),
            (13, 3),
            (14, 6),
            (29, 6),
            (30, 10),
            (55, 10),
        ],
    )
    def test_streak_bonus_for_each_tier(self, streak_days, expected):
        assert streak_bonus_for(streak_days, STREAKS) == expected


class TestAdvanceDotaStreak:
    def test_first_play_starts_at_one(self, player_repository):
        _register(player_repository, 100)
        today = "2026-05-07"
        new_streak = player_repository.advance_dota_streak(
            100, TEST_GUILD_ID, today, yesterday_of(today)
        )
        assert new_streak == 1
        days, last = player_repository.get_dota_streak(100, TEST_GUILD_ID)
        assert days == 1
        assert last == today

    def test_consecutive_day_increments(self, player_repository):
        _register(player_repository, 101)
        # Day 1
        player_repository.advance_dota_streak(
            101, TEST_GUILD_ID, "2026-05-06", "2026-05-05"
        )
        # Day 2 (consecutive)
        new_streak = player_repository.advance_dota_streak(
            101, TEST_GUILD_ID, "2026-05-07", "2026-05-06"
        )
        assert new_streak == 2

    def test_same_day_replay_keeps_streak_same(self, player_repository):
        _register(player_repository, 102)
        player_repository.advance_dota_streak(
            102, TEST_GUILD_ID, "2026-05-07", "2026-05-06"
        )
        # Second match same day: streak unchanged
        new_streak = player_repository.advance_dota_streak(
            102, TEST_GUILD_ID, "2026-05-07", "2026-05-06"
        )
        assert new_streak == 1

    def test_skipped_day_resets_to_one(self, player_repository):
        _register(player_repository, 103)
        # Day 1 (way back)
        player_repository.advance_dota_streak(
            103, TEST_GUILD_ID, "2026-04-01", "2026-03-31"
        )
        # Many days later — streak resets to 1, not 0
        new_streak = player_repository.advance_dota_streak(
            103, TEST_GUILD_ID, "2026-05-07", "2026-05-06"
        )
        assert new_streak == 1

    def test_unknown_player_returns_zero(self, player_repository):
        # No row for this discord_id
        result = player_repository.advance_dota_streak(
            99999, TEST_GUILD_ID, "2026-05-07", "2026-05-06"
        )
        assert result == 0

    def test_streak_is_per_guild(self, player_repository):
        _register(player_repository, 104, guild_id=TEST_GUILD_ID)
        _register(player_repository, 104, guild_id=TEST_GUILD_ID_SECONDARY)
        player_repository.advance_dota_streak(
            104, TEST_GUILD_ID, "2026-05-07", "2026-05-06"
        )
        # Other guild's streak unaffected
        days_other, _ = player_repository.get_dota_streak(104, TEST_GUILD_ID_SECONDARY)
        assert days_other == 0


class TestGameDateHelpers:
    def test_yesterday_of_basic(self):
        assert yesterday_of("2026-05-07") == "2026-05-06"
        assert yesterday_of("2026-01-01") == "2025-12-31"

    def test_get_game_date_uses_pst_4am_rollover(self, monkeypatch):
        # 2026-05-07 11:30 UTC = 2026-05-07 03:30 PST (UTC-8) = before
        # the 4 AM rollover, so the game-date is still "2026-05-06".
        ts = 1778153400  # 2026-05-07 11:30:00 UTC
        monkeypatch.setattr(time, "time", lambda: ts)
        assert get_game_date() == "2026-05-06"

    def test_game_date_for_handles_naive_dt(self):
        import datetime
        # Naive datetime treated as UTC
        dt = datetime.datetime(2026, 5, 7, 11, 30)
        assert game_date_for(dt) == "2026-05-06"

    def test_game_date_for_handles_aware_dt(self):
        import datetime
        utc = datetime.datetime(2026, 5, 7, 18, 0, tzinfo=datetime.UTC)
        assert game_date_for(utc) == "2026-05-07"
