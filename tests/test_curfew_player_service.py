"""Tests for PlayerService curfew methods: set_curfew, disable_curfew, get_curfew_info."""

import pytest

from repositories.player_repository import PlayerRepository
from services.player_service import PlayerService
from tests.conftest import TEST_GUILD_ID
from tests.repository_harness import RepositoryTestDatabase as Database


class TestPlayerServiceCurfew:
    @pytest.fixture
    def test_db(self, repo_db_path):
        return Database(repo_db_path)

    @pytest.fixture
    def player_repo(self, test_db):
        return PlayerRepository(test_db.db_path)

    @pytest.fixture
    def player_service(self, player_repo):
        return PlayerService(player_repo)

    def _register(self, player_repo, discord_id):
        player_repo.add(
            discord_id=discord_id,
            discord_username="TestPlayer",
            guild_id=TEST_GUILD_ID,
            initial_mmr=2000,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )

    def test_set_curfew_persists_and_enables(self, player_repo, player_service):
        self._register(player_repo, 1)

        player_service.set_curfew(
            1,
            TEST_GUILD_ID,
            curfew_hour=22,
            curfew_minute=0,
            wake_hour=6,
            wake_minute=0,
            timezone="America/New_York",
        )

        player = player_repo.get_by_id(1, TEST_GUILD_ID)
        assert player.curfew_enabled is True
        assert player.curfew_hour == 22
        assert player.curfew_wake_hour == 6
        assert player.curfew_timezone == "America/New_York"

    def test_set_curfew_rejects_unknown_timezone(self, player_repo, player_service):
        self._register(player_repo, 2)

        with pytest.raises(ValueError, match="timezone"):
            player_service.set_curfew(
                2,
                TEST_GUILD_ID,
                curfew_hour=22,
                curfew_minute=0,
                wake_hour=6,
                wake_minute=0,
                timezone="Not/AZone",
            )

    def test_set_curfew_rejects_equal_bedtime_and_wake(self, player_repo, player_service):
        self._register(player_repo, 3)

        with pytest.raises(ValueError, match="same"):
            player_service.set_curfew(
                3,
                TEST_GUILD_ID,
                curfew_hour=22,
                curfew_minute=0,
                wake_hour=22,
                wake_minute=0,
                timezone="America/New_York",
            )

    def test_set_curfew_rejects_out_of_range_hour(self, player_repo, player_service):
        self._register(player_repo, 4)

        with pytest.raises(ValueError, match="Hour"):
            player_service.set_curfew(
                4,
                TEST_GUILD_ID,
                curfew_hour=24,
                curfew_minute=0,
                wake_hour=6,
                wake_minute=0,
                timezone="America/New_York",
            )

    def test_set_curfew_unregistered_player_raises(self, player_service):
        with pytest.raises(ValueError, match="not registered"):
            player_service.set_curfew(
                999,
                TEST_GUILD_ID,
                curfew_hour=22,
                curfew_minute=0,
                wake_hour=6,
                wake_minute=0,
                timezone="America/New_York",
            )

    def test_disable_curfew_keeps_configured_hours(self, player_repo, player_service):
        self._register(player_repo, 5)
        player_service.set_curfew(
            5,
            TEST_GUILD_ID,
            curfew_hour=22,
            curfew_minute=0,
            wake_hour=6,
            wake_minute=0,
            timezone="America/New_York",
        )

        player_service.disable_curfew(5, TEST_GUILD_ID)

        player = player_repo.get_by_id(5, TEST_GUILD_ID)
        assert player.curfew_enabled is False
        assert player.curfew_hour == 22

    def test_get_curfew_info_before_any_set(self, player_repo, player_service):
        self._register(player_repo, 6)

        info = player_service.get_curfew_info(6, TEST_GUILD_ID)

        assert info == {"enabled": False, "window": None}

    def test_get_curfew_info_after_set(self, player_repo, player_service):
        self._register(player_repo, 7)
        player_service.set_curfew(
            7,
            TEST_GUILD_ID,
            curfew_hour=22,
            curfew_minute=0,
            wake_hour=6,
            wake_minute=0,
            timezone="America/New_York",
        )

        info = player_service.get_curfew_info(7, TEST_GUILD_ID)

        assert info["enabled"] is True
        assert info["window"] == "10:00 PM - 6:00 AM America/New_York"
