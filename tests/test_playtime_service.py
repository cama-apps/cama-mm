"""Tests for informational dota play-time hours: PlayerRepository + PlayerService."""

import pytest

from repositories.player_repository import PlayerRepository
from services.player_service import PlayerService
from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY
from tests.repository_harness import RepositoryTestDatabase as Database


class TestDotaPlayHoursPersistence:
    def test_default_is_none(self, player_repository):
        player_repository.add(discord_id=1, discord_username="P1", guild_id=TEST_GUILD_ID)
        assert player_repository.get_by_id(1, TEST_GUILD_ID).dota_play_hours is None

    def test_update_and_read(self, player_repository):
        player_repository.add(discord_id=2, discord_username="P2", guild_id=TEST_GUILD_ID)
        player_repository.update_dota_play_hours(2, TEST_GUILD_ID, [18, 19, 20])

        assert player_repository.get_by_id(2, TEST_GUILD_ID).dota_play_hours == [18, 19, 20]

    def test_clearing_with_none(self, player_repository):
        player_repository.add(discord_id=3, discord_username="P3", guild_id=TEST_GUILD_ID)
        player_repository.update_dota_play_hours(3, TEST_GUILD_ID, [18, 19])
        player_repository.update_dota_play_hours(3, TEST_GUILD_ID, None)

        assert player_repository.get_by_id(3, TEST_GUILD_ID).dota_play_hours is None

    def test_guild_scoped(self, player_repository):
        player_repository.add(discord_id=4, discord_username="P4", guild_id=TEST_GUILD_ID)
        player_repository.add(discord_id=4, discord_username="P4", guild_id=TEST_GUILD_ID_SECONDARY)

        player_repository.update_dota_play_hours(4, TEST_GUILD_ID, [18])

        assert player_repository.get_by_id(4, TEST_GUILD_ID).dota_play_hours == [18]
        assert player_repository.get_by_id(4, TEST_GUILD_ID_SECONDARY).dota_play_hours is None


class TestPlayerServiceDotaPlayHours:
    @pytest.fixture
    def test_db(self, repo_db_path):
        return Database(repo_db_path)

    @pytest.fixture
    def player_repo(self, test_db):
        return PlayerRepository(test_db.db_path)

    @pytest.fixture
    def player_service(self, player_repo):
        return PlayerService(player_repo)

    def _register(self, player_repo, discord_id, timezone=None):
        player_repo.add(
            discord_id=discord_id,
            discord_username="TestPlayer",
            guild_id=TEST_GUILD_ID,
            initial_mmr=2000,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
        if timezone:
            player_repo.update_timezone(discord_id, TEST_GUILD_ID, timezone)

    def test_set_dota_play_hours_persists_sorted_and_deduped(self, player_repo, player_service):
        self._register(player_repo, 1)

        player_service.set_dota_play_hours(1, TEST_GUILD_ID, [20, 18, 19, 18])

        assert player_repo.get_by_id(1, TEST_GUILD_ID).dota_play_hours == [18, 19, 20]

    def test_set_dota_play_hours_rejects_out_of_range(self, player_repo, player_service):
        self._register(player_repo, 2)

        with pytest.raises(ValueError, match="between 0 and 23"):
            player_service.set_dota_play_hours(2, TEST_GUILD_ID, [24])

    def test_set_dota_play_hours_rejects_empty(self, player_repo, player_service):
        self._register(player_repo, 3)

        with pytest.raises(ValueError, match="at least one"):
            player_service.set_dota_play_hours(3, TEST_GUILD_ID, [])

    def test_set_dota_play_hours_unregistered_player_raises(self, player_service):
        with pytest.raises(ValueError, match="not registered"):
            player_service.set_dota_play_hours(999, TEST_GUILD_ID, [18])

    def test_clear_dota_play_hours(self, player_repo, player_service):
        self._register(player_repo, 4)
        player_service.set_dota_play_hours(4, TEST_GUILD_ID, [18])

        player_service.clear_dota_play_hours(4, TEST_GUILD_ID)

        assert player_repo.get_by_id(4, TEST_GUILD_ID).dota_play_hours is None

    def test_get_dota_play_hours_info(self, player_repo, player_service):
        self._register(player_repo, 5)

        assert player_service.get_dota_play_hours_info(5, TEST_GUILD_ID) == {"hours": None}

        player_service.set_dota_play_hours(5, TEST_GUILD_ID, [18, 19])

        assert player_service.get_dota_play_hours_info(5, TEST_GUILD_ID) == {"hours": [18, 19]}

    def test_get_popular_play_hours_aggregates_registered_players(self, player_repo, player_service):
        self._register(player_repo, 6, timezone="America/New_York")
        self._register(player_repo, 7, timezone="America/New_York")
        self._register(player_repo, 8, timezone="America/New_York")  # never sets hours
        player_service.set_dota_play_hours(6, TEST_GUILD_ID, [20, 21])
        player_service.set_dota_play_hours(7, TEST_GUILD_ID, [20])

        counts = player_service.get_popular_play_hours(
            TEST_GUILD_ID, reporting_timezone="America/New_York"
        )

        assert counts[20] == 2
        assert counts[21] == 1
        assert sum(counts) == 3

    def test_get_popular_play_hours_defaults_reporting_timezone(self, player_repo, player_service):
        self._register(player_repo, 9, timezone="America/New_York")
        player_service.set_dota_play_hours(9, TEST_GUILD_ID, [12])

        counts = player_service.get_popular_play_hours(TEST_GUILD_ID)

        assert counts[12] == 1
