"""Tests for the general per-player timezone preference (repository + PlayerService)."""

import pytest

from repositories.player_repository import PlayerRepository
from services.player_service import PlayerService
from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY
from tests.repository_harness import RepositoryTestDatabase as Database


class TestTimezonePersistence:
    def test_default_timezone_is_none(self, player_repository):
        player_repository.add(discord_id=1, discord_username="P1", guild_id=TEST_GUILD_ID)
        assert player_repository.get_by_id(1, TEST_GUILD_ID).timezone is None

    def test_update_and_read_timezone(self, player_repository):
        player_repository.add(discord_id=2, discord_username="P2", guild_id=TEST_GUILD_ID)
        player_repository.update_timezone(2, TEST_GUILD_ID, "Asia/Tokyo")

        assert player_repository.get_by_id(2, TEST_GUILD_ID).timezone == "Asia/Tokyo"

    def test_timezone_is_guild_scoped(self, player_repository):
        player_repository.add(discord_id=3, discord_username="P3", guild_id=TEST_GUILD_ID)
        player_repository.add(discord_id=3, discord_username="P3", guild_id=TEST_GUILD_ID_SECONDARY)

        player_repository.update_timezone(3, TEST_GUILD_ID, "Asia/Tokyo")

        assert player_repository.get_by_id(3, TEST_GUILD_ID).timezone == "Asia/Tokyo"
        assert player_repository.get_by_id(3, TEST_GUILD_ID_SECONDARY).timezone is None


class TestPlayerServiceTimezone:
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

    def test_set_timezone_persists(self, player_repo, player_service):
        self._register(player_repo, 1)

        player_service.set_timezone(1, TEST_GUILD_ID, "Asia/Tokyo")

        assert player_repo.get_by_id(1, TEST_GUILD_ID).timezone == "Asia/Tokyo"

    def test_set_timezone_rejects_unknown_timezone(self, player_repo, player_service):
        self._register(player_repo, 2)

        with pytest.raises(ValueError, match="timezone"):
            player_service.set_timezone(2, TEST_GUILD_ID, "Not/AZone")

    def test_set_timezone_unregistered_player_raises(self, player_service):
        with pytest.raises(ValueError, match="not registered"):
            player_service.set_timezone(999, TEST_GUILD_ID, "Asia/Tokyo")

    def test_get_timezone_info_before_any_set(self, player_repo, player_service):
        self._register(player_repo, 3)

        assert player_service.get_timezone_info(3, TEST_GUILD_ID) == {"timezone": None}

    def test_get_timezone_info_after_set(self, player_repo, player_service):
        self._register(player_repo, 4)
        player_service.set_timezone(4, TEST_GUILD_ID, "Asia/Tokyo")

        assert player_service.get_timezone_info(4, TEST_GUILD_ID) == {"timezone": "Asia/Tokyo"}
