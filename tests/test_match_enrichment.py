"""
Tests for MatchEnrichmentService and related functionality.
"""

import json
from unittest.mock import Mock

import pytest

from services.match_enrichment_service import MatchEnrichmentService

TEST_GUILD_ID = 12345


class TestMatchEnrichmentService:
    """Tests for MatchEnrichmentService."""

    @pytest.fixture
    def mock_repos(self):
        """Create mock repositories."""
        match_repo = Mock()
        player_repo = Mock()
        return match_repo, player_repo

    @pytest.fixture
    def mock_opendota_api(self):
        """Create mock OpenDota API."""
        opendota_api = Mock()
        return opendota_api

    def test_enrich_match_success(self, mock_repos, mock_opendota_api):
        """Test successful match enrichment (with skip_validation for unit test)."""
        match_repo, player_repo = mock_repos

        # Setup mock internal match
        match_repo.get_match.return_value = {"match_id": 1, "winning_team": 1}

        # Setup mock OpenDota API response
        mock_opendota_api.get_match_details.return_value = {
            "match_id": 8181518332,
            "duration": 2400,
            "radiant_win": True,
            "radiant_score": 35,
            "dire_score": 22,
            "game_mode": 2,
            "players": [
                {
                    "account_id": 12345,
                    "player_slot": 0,  # Radiant slot
                    "hero_id": 1,
                    "kills": 10,
                    "deaths": 2,
                    "assists": 5,
                    "gold_per_min": 600,
                    "xp_per_min": 550,
                    "hero_damage": 25000,
                    "tower_damage": 3000,
                    "last_hits": 200,
                    "denies": 10,
                    "net_worth": 20000,
                },
            ],
        }

        # Setup mock participants
        match_repo.get_match_participants.return_value = [
            {"discord_id": 100, "side": "radiant"},
        ]

        # Setup mock steam_id bulk lookup (returns dict of lists)
        player_repo.get_steam_ids_bulk.return_value = {100: [12345]}

        service = MatchEnrichmentService(match_repo, player_repo, mock_opendota_api)
        # Use skip_validation for unit test - validation is tested separately
        result = service.enrich_match(1, 8181518332, skip_validation=True)

        assert result["success"] is True
        assert result["players_enriched"] == 1
        assert result["duration"] == 2400
        assert result["radiant_win"] is True
        assert result["fantasy_points_calculated"] is True

        # Verify update calls
        match_repo.update_match_enrichment.assert_called_once()
        match_repo.update_participant_stats_bulk.assert_called_once()

    def test_enrich_match_api_failure(self, mock_repos, mock_opendota_api):
        """Test enrichment when OpenDota API fails."""
        match_repo, player_repo = mock_repos
        mock_opendota_api.get_match_details.return_value = None

        service = MatchEnrichmentService(match_repo, player_repo, mock_opendota_api)
        result = service.enrich_match(1, 8181518332)

        assert result["success"] is False
        assert "Failed to fetch" in result["error"]

    def test_enrich_match_player_not_found(self, mock_repos, mock_opendota_api):
        """Test enrichment when player steam_id not in API response."""
        match_repo, player_repo = mock_repos

        # Setup mock internal match
        match_repo.get_match.return_value = {"match_id": 1, "winning_team": 1}

        mock_opendota_api.get_match_details.return_value = {
            "match_id": 8181518332,
            "duration": 2400,
            "radiant_win": True,
            "radiant_score": 35,
            "dire_score": 22,
            "game_mode": 2,
            "players": [
                {"account_id": 99999, "hero_id": 1},  # Different account_id
            ],
        }

        match_repo.get_match_participants.return_value = [
            {"discord_id": 100, "side": "radiant"},
        ]
        # Setup mock steam_id bulk lookup - steam_id 12345 not in API response (returns dict of lists)
        player_repo.get_steam_ids_bulk.return_value = {100: [12345]}

        service = MatchEnrichmentService(match_repo, player_repo, mock_opendota_api)
        # Use skip_validation for unit test
        result = service.enrich_match(1, 8181518332, skip_validation=True)

        assert result["success"] is True
        assert result["players_enriched"] == 0
        assert 12345 in result["players_not_found"]

    def test_enrich_match_no_steam_id(self, mock_repos, mock_opendota_api):
        """Test enrichment when player has no steam_id."""
        match_repo, player_repo = mock_repos

        # Setup mock internal match
        match_repo.get_match.return_value = {"match_id": 1, "winning_team": 1}

        mock_opendota_api.get_match_details.return_value = {
            "match_id": 8181518332,
            "duration": 2400,
            "radiant_win": True,
            "radiant_score": 35,
            "dire_score": 22,
            "game_mode": 2,
            "players": [],
        }

        match_repo.get_match_participants.return_value = [
            {"discord_id": 100, "side": "radiant"},
        ]
        # Setup mock steam_id bulk lookup - no steam_id for player (empty list)
        player_repo.get_steam_ids_bulk.return_value = {100: []}

        service = MatchEnrichmentService(match_repo, player_repo, mock_opendota_api)
        # Use skip_validation for unit test
        result = service.enrich_match(1, 8181518332, skip_validation=True)

        assert result["success"] is True
        assert result["players_enriched"] == 0

    def test_backfill_steam_ids(self, mock_repos, mock_opendota_api):
        """Test steam_id backfill from dotabuff URLs."""
        match_repo, player_repo = mock_repos

        player_repo.get_all_with_dotabuff_no_steam_id.return_value = [
            {
                "discord_id": 100,
                "dotabuff_url": "https://www.dotabuff.com/players/76561198012345678",
            },
            {
                "discord_id": 101,
                "dotabuff_url": "https://www.dotabuff.com/players/76561198087654321",
            },
        ]

        # Mock get_steam_ids to return empty lists (no existing steam_ids)
        player_repo.get_steam_ids.return_value = []

        service = MatchEnrichmentService(match_repo, player_repo, mock_opendota_api)

        # Mock the opendota_api extract method
        mock_opendota_api.extract_player_id_from_dotabuff.side_effect = [52079950, 127388593]
        result = service.backfill_steam_ids()

        assert result["players_updated"] == 2
        assert len(result["players_failed"]) == 0
        assert player_repo.add_steam_id.call_count == 2

    def test_backfill_steam_ids_with_failures(self, mock_repos, mock_opendota_api):
        """Test steam_id backfill with some failures."""
        match_repo, player_repo = mock_repos

        player_repo.get_all_with_dotabuff_no_steam_id.return_value = [
            {
                "discord_id": 100,
                "dotabuff_url": "https://www.dotabuff.com/players/76561198012345678",
            },
            {"discord_id": 101, "dotabuff_url": "invalid_url"},
        ]

        # Mock get_steam_ids to return empty lists (no existing steam_ids)
        player_repo.get_steam_ids.return_value = []

        service = MatchEnrichmentService(match_repo, player_repo, mock_opendota_api)
        mock_opendota_api.extract_player_id_from_dotabuff.side_effect = [52079950, None]
        result = service.backfill_steam_ids()

        assert result["players_updated"] == 1
        assert 101 in result["players_failed"]


class TestMatchRepositoryEnrichment:
    """Tests for MatchRepository enrichment methods."""

    @pytest.fixture
    def temp_db_path(self, tmp_path):
        """Create temporary database."""
        return str(tmp_path / "test.db")

    def test_get_most_recent_match(self, temp_db_path):
        """Test getting most recent match."""
        from infrastructure.schema_manager import SchemaManager
        from repositories.match_repository import MatchRepository

        SchemaManager(temp_db_path).initialize()
        repo = MatchRepository(temp_db_path)

        # Record two matches
        repo.record_match([1, 2, 3, 4, 5], [6, 7, 8, 9, 10], 1, guild_id=TEST_GUILD_ID)
        match2_id = repo.record_match([11, 12, 13, 14, 15], [16, 17, 18, 19, 20], 2, guild_id=TEST_GUILD_ID)

        # Most recent should be match2
        recent = repo.get_most_recent_match(guild_id=TEST_GUILD_ID)
        assert recent is not None
        assert recent["match_id"] == match2_id

    def test_update_match_enrichment(self, temp_db_path):
        """Test updating match with enrichment data."""
        from infrastructure.schema_manager import SchemaManager
        from repositories.match_repository import MatchRepository

        SchemaManager(temp_db_path).initialize()
        repo = MatchRepository(temp_db_path)

        match_id = repo.record_match([1, 2, 3, 4, 5], [6, 7, 8, 9, 10], 1, guild_id=TEST_GUILD_ID)

        repo.update_match_enrichment(
            match_id=match_id,
            valve_match_id=8181518332,
            duration_seconds=2400,
            radiant_score=35,
            dire_score=22,
            game_mode=2,
            enrichment_data=json.dumps({"test": "data"}),
        )

        # Verify update
        match = repo.get_most_recent_match(guild_id=TEST_GUILD_ID)
        assert match["valve_match_id"] == 8181518332

    def test_update_participant_stats(self, temp_db_path):
        """Test updating participant stats."""
        from infrastructure.schema_manager import SchemaManager
        from repositories.match_repository import MatchRepository

        SchemaManager(temp_db_path).initialize()
        repo = MatchRepository(temp_db_path)

        match_id = repo.record_match([100], [200], 1, guild_id=TEST_GUILD_ID)

        repo.update_participant_stats(
            match_id=match_id,
            discord_id=100,
            hero_id=1,
            kills=10,
            deaths=2,
            assists=5,
            gpm=600,
            xpm=550,
            hero_damage=25000,
            tower_damage=3000,
            last_hits=200,
            denies=10,
            net_worth=20000,
        )

        participants = repo.get_match_participants(match_id, TEST_GUILD_ID)
        p = next((p for p in participants if p["discord_id"] == 100), None)

        assert p is not None
        assert p["hero_id"] == 1
        assert p["kills"] == 10
        assert p["deaths"] == 2
        assert p["assists"] == 5
        assert p["gpm"] == 600

    def test_get_matches_without_enrichment(self, temp_db_path):
        """Test getting matches without enrichment."""
        from infrastructure.schema_manager import SchemaManager
        from repositories.match_repository import MatchRepository

        SchemaManager(temp_db_path).initialize()
        repo = MatchRepository(temp_db_path)

        # Record match without enrichment
        match_id = repo.record_match([1, 2, 3, 4, 5], [6, 7, 8, 9, 10], 1, guild_id=TEST_GUILD_ID)

        unenriched = repo.get_matches_without_enrichment(guild_id=TEST_GUILD_ID)
        assert len(unenriched) == 1
        assert unenriched[0]["match_id"] == match_id

        # Enrich it
        repo.set_valve_match_id(match_id, 8181518332)

        unenriched = repo.get_matches_without_enrichment(guild_id=TEST_GUILD_ID)
        assert len(unenriched) == 0
