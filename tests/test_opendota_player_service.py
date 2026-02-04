"""
Tests for OpenDotaPlayerService.
"""

from datetime import datetime, timedelta
from unittest.mock import Mock, patch

import pytest

from services.opendota_player_service import CACHE_TTL_SECONDS, OpenDotaPlayerService


class TestOpenDotaPlayerService:
    """Tests for OpenDotaPlayerService."""

    @pytest.fixture
    def mock_player_repo(self):
        """Create mock player repository."""
        repo = Mock()
        return repo

    def test_get_player_profile_no_steam_id(self, mock_player_repo):
        """Test profile fetch when player has no steam_id."""
        mock_player_repo.get_steam_id.return_value = None

        service = OpenDotaPlayerService(mock_player_repo)
        result = service.get_player_profile(discord_id=100)

        assert result is None

    def test_get_player_profile_cached(self, mock_player_repo):
        """Test profile fetch uses cache."""
        mock_player_repo.get_steam_id.return_value = 12345

        service = OpenDotaPlayerService(mock_player_repo)

        # Prime the cache
        service._memory_cache[100] = {
            "data": {"steam_id": 12345, "wins": 100, "losses": 50},
            "cached_at": datetime.now(),
        }

        result = service.get_player_profile(discord_id=100)

        assert result is not None
        assert result["steam_id"] == 12345
        assert result["wins"] == 100

    def test_get_player_profile_cache_expired(self, mock_player_repo):
        """Test profile fetch refreshes expired cache."""
        mock_player_repo.get_steam_id.return_value = 12345

        service = OpenDotaPlayerService(mock_player_repo)

        # Prime cache with expired entry
        service._memory_cache[100] = {
            "data": {"steam_id": 12345, "wins": 100},
            "cached_at": datetime.now() - timedelta(seconds=CACHE_TTL_SECONDS + 100),
        }

        # Mock the API fetch to return new data
        with patch.object(service, "_fetch_profile", return_value={"steam_id": 12345, "wins": 150}):
            result = service.get_player_profile(discord_id=100)

        assert result is not None
        assert result["wins"] == 150  # Should have new data

    def test_get_player_profile_force_refresh(self, mock_player_repo):
        """Test force_refresh bypasses cache."""
        mock_player_repo.get_steam_id.return_value = 12345

        service = OpenDotaPlayerService(mock_player_repo)

        # Prime cache with recent entry
        service._memory_cache[100] = {
            "data": {"steam_id": 12345, "wins": 100},
            "cached_at": datetime.now(),
        }

        with patch.object(service, "_fetch_profile", return_value={"steam_id": 12345, "wins": 200}):
            result = service.get_player_profile(discord_id=100, force_refresh=True)

        assert result["wins"] == 200  # Should have refreshed data

    def test_calc_win_rate(self, mock_player_repo):
        """Test win rate calculation."""
        service = OpenDotaPlayerService(mock_player_repo)

        assert service._calc_win_rate(50, 50) == 50.0
        assert service._calc_win_rate(75, 25) == 75.0
        assert service._calc_win_rate(0, 0) == 0.0
        assert service._calc_win_rate(100, 0) == 100.0

    def test_did_win_radiant(self, mock_player_repo):
        """Test win detection for radiant player."""
        service = OpenDotaPlayerService(mock_player_repo)

        # Radiant player (slot < 128) in radiant win
        assert service._did_win({"player_slot": 0, "radiant_win": True}) is True
        assert service._did_win({"player_slot": 0, "radiant_win": False}) is False

    def test_did_win_dire(self, mock_player_repo):
        """Test win detection for dire player."""
        service = OpenDotaPlayerService(mock_player_repo)

        # Dire player (slot >= 128) in dire win
        assert service._did_win({"player_slot": 128, "radiant_win": False}) is True
        assert service._did_win({"player_slot": 128, "radiant_win": True}) is False

    def test_format_profile_embed_no_profile(self, mock_player_repo):
        """Test format_profile_embed when profile unavailable."""
        mock_player_repo.get_steam_id.return_value = None

        service = OpenDotaPlayerService(mock_player_repo)
        result = service.format_profile_embed(discord_id=100, target_name="TestUser")

        assert result is None

    def test_format_profile_embed_success(self, mock_player_repo):
        """Test format_profile_embed returns proper structure."""
        mock_player_repo.get_steam_id.return_value = 12345

        service = OpenDotaPlayerService(mock_player_repo)

        # Mock the full profile
        with patch.object(
            service,
            "get_player_profile",
            return_value={
                "steam_id": 12345,
                "wins": 100,
                "losses": 50,
                "win_rate": 66.7,
                "avg_kills": 8.5,
                "avg_deaths": 5.2,
                "avg_assists": 12.0,
                "avg_gpm": 500,
                "avg_xpm": 550,
                "top_heroes": [
                    {"hero_name": "Pudge", "games": 50, "win_rate": 60.0},
                ],
                "recent_matches": [
                    {"hero_name": "Pudge", "kills": 10, "deaths": 3, "assists": 8, "won": True},
                ],
                "last_match_id": 8181518332,
            },
        ):
            result = service.format_profile_embed(discord_id=100, target_name="TestUser")

        assert result is not None
        assert result["title"] == "Profile: TestUser"
        assert len(result["fields"]) >= 4
        assert result["last_match_id"] == 8181518332


TEST_GUILD_ID = 12345


class TestPlayerRepositorySteamId:
    """Tests for PlayerRepository steam_id methods."""

    @pytest.fixture
    def temp_db_path(self, tmp_path):
        """Create temporary database."""
        return str(tmp_path / "test.db")

    def test_set_and_get_steam_id(self, temp_db_path):
        """Test setting and getting steam_id."""
        from infrastructure.schema_manager import SchemaManager
        from repositories.player_repository import PlayerRepository

        SchemaManager(temp_db_path).initialize()
        repo = PlayerRepository(temp_db_path)

        # Add a player
        repo.add(discord_id=100, discord_username="TestUser", guild_id=TEST_GUILD_ID)

        # Initially no steam_id
        assert repo.get_steam_id(100) is None

        # Set steam_id
        repo.set_steam_id(100, 12345678)

        # Now should have steam_id
        assert repo.get_steam_id(100) == 12345678

    def test_get_by_steam_id(self, temp_db_path):
        """Test finding player by steam_id."""
        from infrastructure.schema_manager import SchemaManager
        from repositories.player_repository import PlayerRepository

        SchemaManager(temp_db_path).initialize()
        repo = PlayerRepository(temp_db_path)

        repo.add(discord_id=100, discord_username="TestUser", guild_id=TEST_GUILD_ID)
        repo.set_steam_id(100, 12345678)

        player = repo.get_by_steam_id(12345678, guild_id=TEST_GUILD_ID)
        assert player is not None
        assert player.discord_id == 100
        assert player.name == "TestUser"

    def test_get_by_steam_id_not_found(self, temp_db_path):
        """Test finding player by steam_id that doesn't exist."""
        from infrastructure.schema_manager import SchemaManager
        from repositories.player_repository import PlayerRepository

        SchemaManager(temp_db_path).initialize()
        repo = PlayerRepository(temp_db_path)

        player = repo.get_by_steam_id(99999999, guild_id=TEST_GUILD_ID)
        assert player is None

    def test_get_all_with_dotabuff_no_steam_id(self, temp_db_path):
        """Test getting players needing steam_id backfill."""
        from infrastructure.schema_manager import SchemaManager
        from repositories.player_repository import PlayerRepository

        SchemaManager(temp_db_path).initialize()
        repo = PlayerRepository(temp_db_path)

        # Add players with various states
        repo.add(
            discord_id=100,
            discord_username="HasBoth",
            guild_id=TEST_GUILD_ID,
            dotabuff_url="https://dotabuff.com/players/123",
        )
        repo.set_steam_id(100, 12345)

        repo.add(
            discord_id=101,
            discord_username="NeedsSteamId",
            guild_id=TEST_GUILD_ID,
            dotabuff_url="https://dotabuff.com/players/456",
        )
        # No steam_id set

        repo.add(discord_id=102, discord_username="NoDotabuff", guild_id=TEST_GUILD_ID)
        # No dotabuff_url

        needs_backfill = repo.get_all_with_dotabuff_no_steam_id()
        assert len(needs_backfill) == 1
        assert needs_backfill[0]["discord_id"] == 101


class TestDistributionCalculations:
    """Tests for hero attribute and lane distribution calculations."""

    @pytest.fixture
    def mock_player_repo(self):
        """Create mock player repository."""
        repo = Mock()
        return repo

    def test_calc_attribute_distribution_empty(self, mock_player_repo):
        """Test attribute distribution with no matches."""
        service = OpenDotaPlayerService(mock_player_repo)
        result = service._calc_attribute_distribution([])

        assert result == {"str": 0, "agi": 0, "int": 0, "all": 0}

    def test_calc_attribute_distribution_with_data(self, mock_player_repo):
        """Test attribute distribution calculation."""
        service = OpenDotaPlayerService(mock_player_repo)

        # Mock hero attributes (normally fetched from API)
        with patch.object(
            service,
            "_get_hero_attributes",
            return_value={
                1: "agi",  # Anti-Mage
                2: "str",  # Axe
                3: "int",  # Bane
                138: "all",  # Primal Beast (Universal)
            },
        ):
            matches = [
                {"hero_id": 1},  # agi
                {"hero_id": 1},  # agi
                {"hero_id": 2},  # str
                {"hero_id": 3},  # int
                {"hero_id": 138},  # all
            ]
            result = service._calc_attribute_distribution(matches)

        assert result["agi"] == 40.0  # 2/5 = 40%
        assert result["str"] == 20.0  # 1/5 = 20%
        assert result["int"] == 20.0  # 1/5 = 20%
        assert result["all"] == 20.0  # 1/5 = 20%

    def test_calc_lane_distribution_empty(self, mock_player_repo):
        """Test lane distribution with no matches."""
        service = OpenDotaPlayerService(mock_player_repo)
        result, count = service._calc_lane_distribution([])

        assert result["Safe Lane"] == 0
        assert result["Mid"] == 0
        assert result["Off Lane"] == 0
        assert result["Jungle"] == 0
        assert result["Roaming"] == 0
        assert count == 0

    def test_calc_lane_distribution_with_data(self, mock_player_repo):
        """Test lane distribution calculation."""
        service = OpenDotaPlayerService(mock_player_repo)

        matches = [
            {"lane_role": 1},  # Safe Lane
            {"lane_role": 1},  # Safe Lane
            {"lane_role": 2},  # Mid
            {"lane_role": 3},  # Off Lane
            {"lane_role": None},  # Unknown - should be skipped
        ]
        result, count = service._calc_lane_distribution(matches)

        assert result["Safe Lane"] == 50.0  # 2/4 = 50%
        assert result["Mid"] == 25.0  # 1/4 = 25%
        assert result["Off Lane"] == 25.0  # 1/4 = 25%
        assert result["Jungle"] == 0  # 0/4 = 0%
        assert result["Roaming"] == 0  # 0/4 = 0%
        assert count == 4

    def test_calc_lane_distribution_with_roaming(self, mock_player_repo):
        """Test lane distribution includes roaming (lane_role=0)."""
        service = OpenDotaPlayerService(mock_player_repo)

        matches = [
            {"lane_role": 0},  # Roaming
            {"lane_role": 0},  # Roaming
            {"lane_role": 1},  # Safe Lane
            {"lane_role": 2},  # Mid
        ]
        result, count = service._calc_lane_distribution(matches)

        assert result["Roaming"] == 50.0  # 2/4 = 50%
        assert result["Safe Lane"] == 25.0  # 1/4 = 25%
        assert result["Mid"] == 25.0  # 1/4 = 25%
        assert count == 4

    def test_get_full_stats_no_steam_id(self, mock_player_repo):
        """Test get_full_stats when player has no steam_id."""
        mock_player_repo.get_steam_id.return_value = None

        service = OpenDotaPlayerService(mock_player_repo)
        result = service.get_full_stats(discord_id=100)

        assert result is None

    def test_get_full_stats_success(self, mock_player_repo):
        """Test get_full_stats returns complete data structure."""
        mock_player_repo.get_steam_id.return_value = 12345

        service = OpenDotaPlayerService(mock_player_repo)

        # Mock all the dependencies
        with patch.object(
            service,
            "get_player_profile",
            return_value={
                "persona_name": "TestPlayer",
                "rank_tier": 55,
                "mmr_estimate": 4500,
                "wins": 500,
                "losses": 400,
                "win_rate": 55.6,
                "avg_kills": 8.0,
                "avg_deaths": 5.0,
                "avg_assists": 10.0,
                "avg_gpm": 450,
                "avg_xpm": 500,
                "avg_last_hits": 150,
                "top_heroes": [{"hero_name": "Pudge", "games": 100, "win_rate": 55.0}],
            },
        ):
            with patch.object(
                service,
                "_fetch_matches_for_stats",
                return_value=[
                    {"hero_id": 1, "lane_role": 1, "player_slot": 0, "radiant_win": True},
                    {"hero_id": 2, "lane_role": 2, "player_slot": 0, "radiant_win": False},
                ],
            ):
                with patch.object(
                    service,
                    "_calc_attribute_distribution",
                    return_value={"str": 50.0, "agi": 50.0, "int": 0, "all": 0},
                ):
                    with patch.object(
                        service,
                        "_calc_lane_distribution",
                        return_value=(
                            {"Safe Lane": 50.0, "Mid": 50.0, "Off Lane": 0, "Jungle": 0, "Roaming": 0},
                            2,  # parsed count
                        ),
                    ):
                        result = service.get_full_stats(discord_id=100)

        assert result is not None
        assert result["steam_id"] == 12345
        assert result["total_wins"] == 500
        assert result["total_losses"] == 400
        assert result["attribute_distribution"]["str"] == 50.0
        assert result["lane_distribution"]["Safe Lane"] == 50.0
        assert result["lane_parsed_count"] == 2
        assert len(result["top_heroes"]) == 1
