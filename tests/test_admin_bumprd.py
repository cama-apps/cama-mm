"""Tests for /admin bumprd command and PlayerService.bump_glicko_rds."""

import pytest

from repositories.player_repository import PlayerRepository
from services.player_service import PlayerService
from tests.conftest import TEST_GUILD_ID


def _make_player(repo, discord_id, rating, rd, volatility=0.06, guild_id=TEST_GUILD_ID):
    """Register a player with specific Glicko values."""
    repo.add(
        discord_id=discord_id,
        discord_username=f"player_{discord_id}",
        guild_id=guild_id,
        glicko_rating=rating,
        glicko_rd=rd,
        glicko_volatility=volatility,
    )


class TestBumpGlickoRds:
    def test_bumps_all_rated_players(self, repo_db_path):
        repo = PlayerRepository(repo_db_path)
        service = PlayerService(repo)
        _make_player(repo, 1, 1500.0, 50.0)
        _make_player(repo, 2, 1200.0, 120.0)
        _make_player(repo, 3, 1800.0, 80.0)

        result = service.bump_glicko_rds(TEST_GUILD_ID, 100)

        assert result is not None
        assert result["count"] == 3
        assert result["avg_before"] == pytest.approx((50 + 120 + 80) / 3)
        assert result["avg_after"] == pytest.approx((150 + 220 + 180) / 3)

        # Verify actual DB values
        for pid, expected_rd in [(1, 150.0), (2, 220.0), (3, 180.0)]:
            rating_data = repo.get_glicko_rating(pid, TEST_GUILD_ID)
            assert rating_data[1] == pytest.approx(expected_rd)

    def test_caps_rd_at_350(self, repo_db_path):
        repo = PlayerRepository(repo_db_path)
        service = PlayerService(repo)
        _make_player(repo, 1, 1500.0, 300.0)
        _make_player(repo, 2, 1200.0, 340.0)

        result = service.bump_glicko_rds(TEST_GUILD_ID, 100)

        assert result is not None
        assert result["avg_after"] == pytest.approx((350 + 350) / 2)

        rating_data = repo.get_glicko_rating(1, TEST_GUILD_ID)
        assert rating_data[1] == pytest.approx(350.0)
        rating_data = repo.get_glicko_rating(2, TEST_GUILD_ID)
        assert rating_data[1] == pytest.approx(350.0)

    def test_preserves_rating_and_volatility(self, repo_db_path):
        repo = PlayerRepository(repo_db_path)
        service = PlayerService(repo)
        _make_player(repo, 1, 1500.0, 50.0, volatility=0.07)

        service.bump_glicko_rds(TEST_GUILD_ID, 100)

        rating_data = repo.get_glicko_rating(1, TEST_GUILD_ID)
        assert rating_data[0] == pytest.approx(1500.0)  # rating unchanged
        assert rating_data[2] == pytest.approx(0.07)  # volatility unchanged

    def test_skips_unrated_players(self, repo_db_path):
        repo = PlayerRepository(repo_db_path)
        service = PlayerService(repo)
        _make_player(repo, 1, 1500.0, 50.0)
        # Player with no rating
        repo.add(discord_id=2, discord_username="unrated", guild_id=TEST_GUILD_ID)

        result = service.bump_glicko_rds(TEST_GUILD_ID, 100)

        assert result is not None
        assert result["count"] == 1

    def test_returns_none_for_no_rated_players(self, repo_db_path):
        repo = PlayerRepository(repo_db_path)
        service = PlayerService(repo)

        result = service.bump_glicko_rds(TEST_GUILD_ID, 100)

        assert result is None

    def test_guild_isolation(self, repo_db_path):
        repo = PlayerRepository(repo_db_path)
        service = PlayerService(repo)
        _make_player(repo, 1, 1500.0, 50.0, guild_id=TEST_GUILD_ID)
        _make_player(repo, 2, 1200.0, 60.0, guild_id=99999)

        result = service.bump_glicko_rds(TEST_GUILD_ID, 100)

        assert result["count"] == 1
        # Other guild's player should be untouched
        rating_data = repo.get_glicko_rating(2, 99999)
        assert rating_data[1] == pytest.approx(60.0)
