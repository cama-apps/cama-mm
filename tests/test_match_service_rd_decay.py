"""Tests for RD decay application when loading Glicko players in MatchService."""

import math
from datetime import datetime, timedelta, timezone

from services.match_service import MatchService

TEST_GUILD_ID = 12345


class FakePlayerRepo:
    def __init__(self, rating_tuple, last_match_date=None, created_at=None):
        self.rating_tuple = rating_tuple
        self.last_match_date = last_match_date
        self.created_at = created_at

    def get_glicko_rating(self, _pid, guild_id=None):
        return self.rating_tuple

    def get_last_match_date(self, _pid, guild_id=None):
        return (self.last_match_date, self.created_at)

    def get_by_id(self, _pid, guild_id=None):
        return None  # Not used when rating data exists

    # Unused repo methods in this test
    def apply_match_outcome(self, *_args, **_kwargs):
        raise NotImplementedError


class FakeMatchRepo:
    pass


def _iso_days_ago(days: int) -> str:
    dt = datetime.now(timezone.utc) - timedelta(days=days)
    return dt.isoformat()


def test_load_glicko_player_applies_decay_after_grace_period():
    start_rd = 100.0
    rating_tuple = (1500.0, start_rd, 0.06)
    last_match_iso = _iso_days_ago(21)  # 3 weeks ago
    repo = FakePlayerRepo(rating_tuple, last_match_date=last_match_iso, created_at=None)
    ms = MatchService(repo, FakeMatchRepo())

    player, _pid = ms._load_glicko_player(1)

    expected_weeks = 21 // 7
    expected_rd = math.sqrt(start_rd * start_rd + (50.0 * 50.0) * expected_weeks)
    assert math.isclose(player.rd, expected_rd, rel_tol=1e-6), "RD should decay using Glicko formula"
    assert player.rd > start_rd


def test_load_glicko_player_uses_created_at_when_last_match_missing():
    start_rd = 120.0
    rating_tuple = (1500.0, start_rd, 0.06)
    created_at_iso = _iso_days_ago(21)
    repo = FakePlayerRepo(rating_tuple, last_match_date=None, created_at=created_at_iso)
    ms = MatchService(repo, FakeMatchRepo())

    player, _pid = ms._load_glicko_player(2)
    assert player.rd > start_rd, "RD should decay when using created_at fallback"


def test_load_glicko_player_caps_rd_at_350():
    start_rd = 340.0
    rating_tuple = (1500.0, start_rd, 0.06)
    last_match_iso = _iso_days_ago(70)  # Long inactivity
    repo = FakePlayerRepo(rating_tuple, last_match_date=last_match_iso, created_at=None)
    ms = MatchService(repo, FakeMatchRepo())

    player, _pid = ms._load_glicko_player(3)
    assert player.rd == 350.0, "RD should cap at 350 after decay"

