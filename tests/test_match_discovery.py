"""
Tests for MatchDiscoveryService and related functionality.
"""

from datetime import datetime
from unittest.mock import Mock

import pytest

from services.match_discovery_service import (
    MatchDiscoveryService,
)

TEST_GUILD_ID = 12345


class TestMatchDiscoveryService:
    """Tests for MatchDiscoveryService."""

    @pytest.fixture
    def mock_repos(self):
        """Create mock repositories."""
        match_repo = Mock()
        player_repo = Mock()
        return match_repo, player_repo

    @pytest.fixture
    def mock_opendota_api(self):
        """Create mock OpenDota API."""
        return Mock()

    def test_discover_match_high_confidence(self, mock_repos, mock_opendota_api):
        """Test discovery with high confidence (8+ players)."""
        match_repo, player_repo = mock_repos

        # Setup match data
        match_repo.get_match.return_value = {
            "match_id": 1,
            "match_date": "2024-01-15 12:00:00",
        }
        match_repo.get_match_participants.return_value = [
            {"discord_id": i}
            for i in range(1, 11)  # 10 players
        ]

        # All players have steam_ids (bulk method returns dict of lists)
        player_repo.get_steam_ids_bulk.return_value = {
            i: [i + 1000] for i in range(1, 11)
        }

        # OpenDota returns matches for all players within time window
        match_time = int(datetime(2024, 1, 15, 12, 0, 0).timestamp())
        mock_opendota_api.get_player_matches.return_value = [
            {"match_id": 99999, "start_time": match_time + 60},  # 1 min after
        ]

        service = MatchDiscoveryService(match_repo, player_repo, mock_opendota_api)
        result = service._discover_single_match(1, dry_run=True)

        assert result["status"] == "discovered"
        assert result["valve_match_id"] == 99999
        assert result["confidence"] == 1.0  # All 10 players found
        assert result["player_count"] == 10

    def test_discover_match_low_confidence_skipped(self, mock_repos, mock_opendota_api):
        """Test discovery with low confidence (<80%) is skipped."""
        match_repo, player_repo = mock_repos

        match_repo.get_match.return_value = {
            "match_id": 1,
            "match_date": "2024-01-15 12:00:00",
        }
        match_repo.get_match_participants.return_value = [{"discord_id": i} for i in range(1, 11)]

        # All players have steam_ids (bulk method returns dict of lists)
        player_repo.get_steam_ids_bulk.return_value = {
            i: [i + 1000] for i in range(1, 11)
        }

        match_time = int(datetime(2024, 1, 15, 12, 0, 0).timestamp())

        # Only first 5 players have matches (50% confidence, below 80% threshold)
        def mock_get_matches(steam_id, limit=20):
            if steam_id < 1006:  # First 5 players
                return [{"match_id": 99999, "start_time": match_time}]
            return []

        mock_opendota_api.get_player_matches.side_effect = mock_get_matches

        service = MatchDiscoveryService(match_repo, player_repo, mock_opendota_api)
        result = service._discover_single_match(1, dry_run=True)

        assert result["status"] == "low_confidence"
        assert result["confidence"] == 0.5
        assert result["player_count"] == 5

    def test_discover_match_not_enough_steam_ids(self, mock_repos, mock_opendota_api):
        """Test discovery skipped when not enough players have steam_ids."""
        match_repo, player_repo = mock_repos

        match_repo.get_match.return_value = {
            "match_id": 1,
            "match_date": "2024-01-15 12:00:00",
        }
        match_repo.get_match_participants.return_value = [{"discord_id": i} for i in range(1, 11)]

        # Only 3 players have steam_ids (below MIN_PLAYERS_FOR_DISCOVERY)
        # Bulk method returns dict of lists (empty list for players without steam_ids)
        player_repo.get_steam_ids_bulk.return_value = {
            1: [1001], 2: [1002], 3: [1003],
            4: [], 5: [], 6: [], 7: [], 8: [], 9: [], 10: [],
        }

        service = MatchDiscoveryService(match_repo, player_repo, mock_opendota_api)
        result = service._discover_single_match(1, dry_run=True)

        assert result["status"] == "no_steam_ids"
        assert result["players_with_steam_id"] == 3

    def test_discover_match_outside_time_window(self, mock_repos, mock_opendota_api):
        """Test matches outside time window are not considered."""
        match_repo, player_repo = mock_repos

        match_repo.get_match.return_value = {
            "match_id": 1,
            "match_date": "2024-01-15 12:00:00",
        }
        match_repo.get_match_participants.return_value = [{"discord_id": i} for i in range(1, 11)]

        # Bulk method returns dict of lists
        player_repo.get_steam_ids_bulk.return_value = {
            i: [i + 1000] for i in range(1, 11)
        }

        match_time = int(datetime(2024, 1, 15, 12, 0, 0).timestamp())

        # Matches are 5 hours away (outside 3 hour window)
        mock_opendota_api.get_player_matches.return_value = [
            {"match_id": 99999, "start_time": match_time + 5 * 3600},
        ]

        service = MatchDiscoveryService(match_repo, player_repo, mock_opendota_api)
        result = service._discover_single_match(1, dry_run=True)

        assert result["status"] == "no_candidates"

    def test_discover_match_dry_run_no_enrichment(self, mock_repos, mock_opendota_api):
        """Test dry_run=True doesn't call enrichment."""
        match_repo, player_repo = mock_repos

        match_repo.get_match.return_value = {
            "match_id": 1,
            "match_date": "2024-01-15 12:00:00",
        }
        match_repo.get_match_participants.return_value = [{"discord_id": i} for i in range(1, 11)]

        # Bulk method returns dict of lists
        player_repo.get_steam_ids_bulk.return_value = {
            i: [i + 1000] for i in range(1, 11)
        }

        match_time = int(datetime(2024, 1, 15, 12, 0, 0).timestamp())
        mock_opendota_api.get_player_matches.return_value = [
            {"match_id": 99999, "start_time": match_time},
        ]

        service = MatchDiscoveryService(match_repo, player_repo, mock_opendota_api)
        result = service._discover_single_match(1, dry_run=True)

        # In dry run, match_repo.update_match_enrichment should NOT be called
        match_repo.update_match_enrichment.assert_not_called()

        assert result["status"] == "discovered"

    def test_discover_all_matches(self, mock_repos, mock_opendota_api):
        """Test discover_all_matches processes multiple matches."""
        match_repo, player_repo = mock_repos

        # Two unenriched matches
        match_repo.get_matches_without_enrichment.return_value = [
            {"match_id": 1},
            {"match_id": 2},
        ]

        # Both matches have same structure
        match_repo.get_match.side_effect = [
            {"match_id": 1, "match_date": "2024-01-15 12:00:00"},
            {"match_id": 2, "match_date": "2024-01-16 12:00:00"},
        ]
        match_repo.get_match_participants.return_value = [{"discord_id": i} for i in range(1, 11)]

        # Only 3 steam_ids (will skip both)
        # Bulk method returns dict of lists
        player_repo.get_steam_ids_bulk.return_value = {
            1: [1001], 2: [1002], 3: [1003],
            4: [], 5: [], 6: [], 7: [], 8: [], 9: [], 10: [],
        }

        service = MatchDiscoveryService(match_repo, player_repo, mock_opendota_api)
        results = service.discover_all_matches(dry_run=True)

        assert results["total_unenriched"] == 2
        assert results["skipped_no_steam_ids"] == 2
        assert results["discovered"] == 0

    def test_parse_match_time_iso_format(self, mock_repos, mock_opendota_api):
        """Test parsing ISO format timestamps."""
        match_repo, player_repo = mock_repos
        service = MatchDiscoveryService(match_repo, player_repo, mock_opendota_api)

        # ISO format with Z
        result = service._parse_match_time("2024-01-15T12:00:00Z")
        assert result is not None

        # ISO format with offset
        result = service._parse_match_time("2024-01-15T12:00:00+00:00")
        assert result is not None

    def test_parse_match_time_sqlite_format(self, mock_repos, mock_opendota_api):
        """Test parsing SQLite timestamp format."""
        match_repo, player_repo = mock_repos
        service = MatchDiscoveryService(match_repo, player_repo, mock_opendota_api)

        result = service._parse_match_time("2024-01-15 12:00:00")
        assert result is not None

    def test_parse_match_time_unix_timestamp(self, mock_repos, mock_opendota_api):
        """Test parsing Unix timestamps."""
        match_repo, player_repo = mock_repos
        service = MatchDiscoveryService(match_repo, player_repo, mock_opendota_api)

        result = service._parse_match_time(1705320000)
        assert result == 1705320000

    def test_parse_match_time_invalid(self, mock_repos, mock_opendota_api):
        """Test parsing invalid timestamps returns None."""
        match_repo, player_repo = mock_repos
        service = MatchDiscoveryService(match_repo, player_repo, mock_opendota_api)

        assert service._parse_match_time(None) is None
        assert service._parse_match_time("invalid") is None


class TestMatchRepositoryWipeMethods:
    """Tests for MatchRepository wipe and discovery-related methods."""

    @pytest.fixture
    def temp_db_path(self, tmp_path):
        """Create temporary database."""
        return str(tmp_path / "test.db")

    def test_wipe_match_enrichment(self, temp_db_path):
        """Test wiping enrichment for a single match."""
        from infrastructure.schema_manager import SchemaManager
        from repositories.match_repository import MatchRepository

        SchemaManager(temp_db_path).initialize()
        repo = MatchRepository(temp_db_path)

        # Create and enrich a match
        match_id = repo.record_match([1, 2, 3, 4, 5], [6, 7, 8, 9, 10], 1, TEST_GUILD_ID)
        repo.update_match_enrichment(
            match_id=match_id,
            valve_match_id=99999,
            duration_seconds=2400,
            radiant_score=35,
            dire_score=22,
            game_mode=2,
            enrichment_source="manual",
        )

        # Verify enriched
        match = repo.get_most_recent_match(TEST_GUILD_ID)
        assert match["valve_match_id"] == 99999

        # Wipe it
        success = repo.wipe_match_enrichment(match_id)
        assert success is True

        # Verify wiped
        match = repo.get_most_recent_match(TEST_GUILD_ID)
        assert match["valve_match_id"] is None

    def test_wipe_match_enrichment_not_found(self, temp_db_path):
        """Test wiping non-existent match returns False."""
        from infrastructure.schema_manager import SchemaManager
        from repositories.match_repository import MatchRepository

        SchemaManager(temp_db_path).initialize()
        repo = MatchRepository(temp_db_path)

        success = repo.wipe_match_enrichment(99999)
        assert success is False

    def test_wipe_auto_discovered_enrichments(self, temp_db_path):
        """Test wiping only auto-discovered enrichments."""
        from infrastructure.schema_manager import SchemaManager
        from repositories.match_repository import MatchRepository

        SchemaManager(temp_db_path).initialize()
        repo = MatchRepository(temp_db_path)

        # Create matches with different enrichment sources
        match1 = repo.record_match([1, 2, 3, 4, 5], [6, 7, 8, 9, 10], 1, TEST_GUILD_ID)
        repo.update_match_enrichment(
            match_id=match1,
            valve_match_id=11111,
            duration_seconds=2400,
            radiant_score=35,
            dire_score=22,
            game_mode=2,
            enrichment_source="manual",
        )

        match2 = repo.record_match([11, 12, 13, 14, 15], [16, 17, 18, 19, 20], 2, TEST_GUILD_ID)
        repo.update_match_enrichment(
            match_id=match2,
            valve_match_id=22222,
            duration_seconds=1800,
            radiant_score=20,
            dire_score=30,
            game_mode=2,
            enrichment_source="auto",
            enrichment_confidence=0.9,
        )

        match3 = repo.record_match([21, 22, 23, 24, 25], [26, 27, 28, 29, 30], 1, TEST_GUILD_ID)
        repo.update_match_enrichment(
            match_id=match3,
            valve_match_id=33333,
            duration_seconds=2000,
            radiant_score=25,
            dire_score=25,
            game_mode=2,
            enrichment_source="auto",
            enrichment_confidence=0.85,
        )

        # Verify counts
        assert repo.get_auto_discovered_count(TEST_GUILD_ID) == 2

        # Wipe auto-discovered
        wiped = repo.wipe_auto_discovered_enrichments(TEST_GUILD_ID)
        assert wiped == 2

        # Verify manual one still enriched
        unenriched = repo.get_matches_without_enrichment(TEST_GUILD_ID)
        assert len(unenriched) == 2  # The two auto ones are now unenriched

        # Get the manual match and verify it's still enriched
        import sqlite3

        conn = sqlite3.connect(temp_db_path)
        conn.row_factory = sqlite3.Row
        cursor = conn.cursor()
        cursor.execute("SELECT valve_match_id FROM matches WHERE match_id = ?", (match1,))
        row = cursor.fetchone()
        conn.close()
        assert row["valve_match_id"] == 11111

    def test_get_auto_discovered_count(self, temp_db_path):
        """Test counting auto-discovered enrichments."""
        from infrastructure.schema_manager import SchemaManager
        from repositories.match_repository import MatchRepository

        SchemaManager(temp_db_path).initialize()
        repo = MatchRepository(temp_db_path)

        # Initially zero
        assert repo.get_auto_discovered_count(TEST_GUILD_ID) == 0

        # Add an auto-discovered match
        match_id = repo.record_match([1, 2, 3, 4, 5], [6, 7, 8, 9, 10], 1, TEST_GUILD_ID)
        repo.update_match_enrichment(
            match_id=match_id,
            valve_match_id=99999,
            duration_seconds=2400,
            radiant_score=35,
            dire_score=22,
            game_mode=2,
            enrichment_source="auto",
            enrichment_confidence=0.9,
        )

        assert repo.get_auto_discovered_count(TEST_GUILD_ID) == 1

    def test_enrichment_source_and_confidence_stored(self, temp_db_path):
        """Test enrichment source and confidence are properly stored."""
        import sqlite3

        from infrastructure.schema_manager import SchemaManager
        from repositories.match_repository import MatchRepository

        SchemaManager(temp_db_path).initialize()
        repo = MatchRepository(temp_db_path)

        match_id = repo.record_match([1, 2, 3, 4, 5], [6, 7, 8, 9, 10], 1, TEST_GUILD_ID)
        repo.update_match_enrichment(
            match_id=match_id,
            valve_match_id=99999,
            duration_seconds=2400,
            radiant_score=35,
            dire_score=22,
            game_mode=2,
            enrichment_source="auto",
            enrichment_confidence=0.85,
        )

        # Verify stored correctly
        conn = sqlite3.connect(temp_db_path)
        conn.row_factory = sqlite3.Row
        cursor = conn.cursor()
        cursor.execute(
            "SELECT enrichment_source, enrichment_confidence FROM matches WHERE match_id = ?",
            (match_id,),
        )
        row = cursor.fetchone()
        conn.close()

        assert row["enrichment_source"] == "auto"
        assert row["enrichment_confidence"] == 0.85


class TestEnrichmentServiceSource:
    """Tests for enrichment source tracking in MatchEnrichmentService."""

    @pytest.fixture
    def mock_repos(self):
        match_repo = Mock()
        player_repo = Mock()
        return match_repo, player_repo

    @pytest.fixture
    def mock_opendota_api(self):
        return Mock()

    def test_enrich_match_default_source_manual(self, mock_repos, mock_opendota_api):
        """Test that default enrichment source is 'manual'."""
        from services.match_enrichment_service import MatchEnrichmentService

        match_repo, player_repo = mock_repos

        mock_opendota_api.get_match_details.return_value = {
            "match_id": 99999,
            "duration": 2400,
            "radiant_win": True,
            "radiant_score": 35,
            "dire_score": 22,
            "game_mode": 2,
            "players": [],
        }

        match_repo.get_match.return_value = {"match_id": 1, "winning_team": 1}
        match_repo.get_match_participants.return_value = []
        # Bulk method returns dict of lists (empty for no participants)
        player_repo.get_steam_ids_bulk.return_value = {}

        service = MatchEnrichmentService(match_repo, player_repo, mock_opendota_api)
        # Use skip_validation since this test is checking source/confidence, not validation
        service.enrich_match(1, 99999, skip_validation=True)

        # Check that update_match_enrichment was called with source='manual'
        match_repo.update_match_enrichment.assert_called_once()
        call_kwargs = match_repo.update_match_enrichment.call_args[1]
        assert call_kwargs["enrichment_source"] == "manual"
        assert call_kwargs["enrichment_confidence"] is None

    def test_enrich_match_auto_source(self, mock_repos, mock_opendota_api):
        """Test enrichment with auto source and confidence."""
        from services.match_enrichment_service import MatchEnrichmentService

        match_repo, player_repo = mock_repos

        mock_opendota_api.get_match_details.return_value = {
            "match_id": 99999,
            "duration": 2400,
            "radiant_win": True,
            "radiant_score": 35,
            "dire_score": 22,
            "game_mode": 2,
            "players": [],
        }

        match_repo.get_match.return_value = {"match_id": 1, "winning_team": 1}
        match_repo.get_match_participants.return_value = []
        # Bulk method returns dict of lists (empty for no participants)
        player_repo.get_steam_ids_bulk.return_value = {}

        service = MatchEnrichmentService(match_repo, player_repo, mock_opendota_api)
        # Use skip_validation since this test is checking source/confidence, not validation
        service.enrich_match(1, 99999, source="auto", confidence=0.9, skip_validation=True)

        call_kwargs = match_repo.update_match_enrichment.call_args[1]
        assert call_kwargs["enrichment_source"] == "auto"
        assert call_kwargs["enrichment_confidence"] == 0.9


class TestFantasyPointCalculation:
    """Tests for fantasy point calculation."""

    def test_calculate_fantasy_points_basic(self):
        """Test basic fantasy point calculation."""
        from services.match_enrichment_service import calculate_fantasy_points

        player_data = {
            "kills": 10,
            "deaths": 5,
            "assists": 15,
            "last_hits": 200,
            "denies": 10,
            "gold_per_min": 500,
            "xp_per_min": 450,
        }

        points = calculate_fantasy_points(player_data)

        # kills: 10 * 0.3 = 3.0
        # deaths: 3 - (5 * 0.3) = 1.5
        # assists: 15 * 0.15 = 2.25
        # last_hits: 200 * 0.003 = 0.6
        # denies: 10 * 0.003 = 0.03
        # gpm: 500 * 0.002 = 1.0
        # xpm: 450 * 0.002 = 0.9
        # Total: 9.28
        assert points == 9.28

    def test_calculate_fantasy_points_with_objectives(self):
        """Test fantasy points with tower/roshan kills."""
        from services.match_enrichment_service import calculate_fantasy_points

        player_data = {
            "kills": 5,
            "deaths": 2,
            "assists": 10,
            "last_hits": 100,
            "denies": 5,
            "gold_per_min": 400,
            "xp_per_min": 350,
            "tower_damage": 3000,
            "towers_killed": 3,
            "roshans_killed": 1,
        }

        points = calculate_fantasy_points(player_data)

        # kills: 5 * 0.3 = 1.5
        # deaths: 3 - (2 * 0.3) = 2.4
        # assists: 10 * 0.15 = 1.5
        # last_hits: 100 * 0.003 = 0.3
        # denies: 5 * 0.003 = 0.015
        # gpm: 400 * 0.002 = 0.8
        # xpm: 350 * 0.002 = 0.7
        # tower_damage: 0 (no points)
        # towers: 3 * 1.0 = 3.0
        # roshans: 1 * 1.0 = 1.0
        # Total: 11.215 -> 11.21 (rounded)
        assert points == 11.21

    def test_calculate_fantasy_points_support_style(self):
        """Test fantasy points for support-style performance."""
        from services.match_enrichment_service import calculate_fantasy_points

        player_data = {
            "kills": 2,
            "deaths": 8,
            "assists": 25,
            "last_hits": 50,
            "gold_per_min": 250,
            "xp_per_min": 300,
            "tower_damage": 500,
            "obs_placed": 15,
            "sen_placed": 10,
            "camps_stacked": 8,
            "teamfight_participation": 0.85,
            "hero_healing": 5000,
        }

        points = calculate_fantasy_points(player_data)

        # kills: 2 * 0.3 = 0.6
        # deaths: 3 - (8 * 0.3) = 0.6
        # assists: 25 * 0.15 = 3.75
        # last_hits: 50 * 0.003 = 0.15
        # gpm: 250 * 0.002 = 0.5
        # xpm: 300 * 0.002 = 0.6
        # tower_damage: 0 (no points)
        # wards: (15 + 10) * 0.5 = 12.5
        # camps: 8 * 0.5 = 4.0
        # teamfight: 0.85 * 3.0 = 2.55
        # hero_healing: 5000 * 0.0004 = 2.0
        # Total: 27.25
        assert points == 27.25

    def test_calculate_fantasy_points_first_blood(self):
        """Test fantasy points with first blood bonus."""
        from services.match_enrichment_service import calculate_fantasy_points

        player_data = {
            "kills": 1,
            "deaths": 0,
            "assists": 0,
            "last_hits": 0,
            "gold_per_min": 0,
            "firstblood_claimed": True,
        }

        points = calculate_fantasy_points(player_data)

        # kills: 1 * 0.3 = 0.3
        # deaths: 3 - (0 * 0.3) = 3.0
        # firstblood: 4.0
        # Total: 7.3
        assert points == 7.3

    def test_calculate_fantasy_points_stuns(self):
        """Test fantasy points with stun duration."""
        from services.match_enrichment_service import calculate_fantasy_points

        player_data = {
            "kills": 0,
            "deaths": 0,
            "assists": 0,
            "last_hits": 0,
            "gold_per_min": 0,
            "stuns": 100.0,  # 100 seconds of stun
        }

        points = calculate_fantasy_points(player_data)

        # deaths: 3 - (0 * 0.3) = 3.0
        # stuns: 100 * 0.07 = 7.0
        # Total: 10.0
        assert points == 10.0

    def test_calculate_fantasy_points_offlane_style(self):
        """Test fantasy points for offlane-style performance (initiator/splitpush)."""
        from services.match_enrichment_service import calculate_fantasy_points

        player_data = {
            "kills": 4,
            "deaths": 6,
            "assists": 18,
            "last_hits": 150,
            "denies": 8,
            "gold_per_min": 380,
            "xp_per_min": 550,
            "tower_damage": 5000,
            "towers_killed": 2,
            "stuns": 85.0,  # Initiator hero like Slardar/Tide
            "teamfight_participation": 0.75,
        }

        points = calculate_fantasy_points(player_data)

        # kills: 4 * 0.3 = 1.2
        # deaths: 3 - (6 * 0.3) = 1.2
        # assists: 18 * 0.15 = 2.7
        # last_hits: 150 * 0.003 = 0.45
        # denies: 8 * 0.003 = 0.024
        # gpm: 380 * 0.002 = 0.76
        # xpm: 550 * 0.002 = 1.1
        # tower_damage: 0 (no points)
        # towers: 2 * 1.0 = 2.0
        # stuns: 85 * 0.07 = 5.95
        # teamfight: 0.75 * 3.0 = 2.25
        # Total: 17.63
        assert points == 17.63

    def test_calculate_fantasy_points_empty(self):
        """Test fantasy points with empty data."""
        from services.match_enrichment_service import calculate_fantasy_points

        player_data = {}
        points = calculate_fantasy_points(player_data)
        assert points == 0.0


class TestEnrichmentValidation:
    """Tests for enrichment validation logic."""

    @pytest.fixture
    def mock_repos(self):
        match_repo = Mock()
        player_repo = Mock()
        return match_repo, player_repo

    @pytest.fixture
    def mock_opendota_api(self):
        return Mock()

    def test_validation_winning_team_mismatch(self, mock_repos, mock_opendota_api, monkeypatch):
        """Test validation fails when winning team doesn't match."""
        from services.match_enrichment_service import MatchEnrichmentService

        # Temporarily lower the min player match requirement for this test
        import services.match_enrichment_service as mes
        original_min = mes.ENRICHMENT_MIN_PLAYER_MATCH
        monkeypatch.setattr(mes, "ENRICHMENT_MIN_PLAYER_MATCH", 0)

        match_repo, player_repo = mock_repos

        # Internal match: Radiant won
        match_repo.get_match.return_value = {"match_id": 1, "winning_team": 1}
        match_repo.get_match_participants.return_value = []
        # Bulk method returns dict of lists (empty for no participants)
        player_repo.get_steam_ids_bulk.return_value = {}

        # OpenDota: Dire won
        mock_opendota_api.get_match_details.return_value = {
            "match_id": 99999,
            "duration": 2400,
            "radiant_win": False,  # Dire won
            "radiant_score": 22,
            "dire_score": 35,
            "game_mode": 2,
            "players": [],
        }

        service = MatchEnrichmentService(match_repo, player_repo, mock_opendota_api)
        result = service.enrich_match(1, 99999)

        assert result["success"] is False
        assert "Winning team mismatch" in result["error"]

    def test_validation_player_side_mismatch(self, mock_repos, mock_opendota_api, monkeypatch):
        """Test validation fails when player is on wrong team."""
        from services.match_enrichment_service import MatchEnrichmentService

        # Temporarily lower the min player match requirement for this test
        import services.match_enrichment_service as mes
        monkeypatch.setattr(mes, "ENRICHMENT_MIN_PLAYER_MATCH", 1)

        match_repo, player_repo = mock_repos

        # Internal match: Player 123 on Radiant
        match_repo.get_match.return_value = {"match_id": 1, "winning_team": 1}
        match_repo.get_match_participants.return_value = [
            {"discord_id": 123, "side": "radiant"},
        ]
        # Use bulk method - returns dict of lists
        player_repo.get_steam_ids_bulk.return_value = {123: [12345678]}

        # OpenDota: Player 12345678 on Dire (slot 128+)
        mock_opendota_api.get_match_details.return_value = {
            "match_id": 99999,
            "duration": 2400,
            "radiant_win": True,
            "radiant_score": 35,
            "dire_score": 22,
            "game_mode": 2,
            "players": [
                {"account_id": 12345678, "player_slot": 128},  # Dire slot
            ],
        }

        service = MatchEnrichmentService(match_repo, player_repo, mock_opendota_api)
        result = service.enrich_match(1, 99999)

        assert result["success"] is False
        assert "wrong team" in result["error"]

    def test_validation_success_with_full_match(self, mock_repos, mock_opendota_api):
        """Test validation succeeds with proper 10-player match."""
        from services.match_enrichment_service import MatchEnrichmentService

        match_repo, player_repo = mock_repos

        # Create 10 participants (5 Radiant, 5 Dire)
        participants = []
        for i in range(5):
            participants.append({"discord_id": i, "side": "radiant"})
        for i in range(5, 10):
            participants.append({"discord_id": i, "side": "dire"})

        match_repo.get_match.return_value = {"match_id": 1, "winning_team": 1}
        match_repo.get_match_participants.return_value = participants

        # Map discord_id to steam_id using bulk method (discord_id + 1000)
        # Returns dict of lists
        player_repo.get_steam_ids_bulk.return_value = {i: [i + 1000] for i in range(10)}

        # Create OpenDota players
        od_players = []
        for i in range(5):
            od_players.append({
                "account_id": i + 1000,
                "player_slot": i,  # Radiant slots 0-4
                "kills": 5,
                "deaths": 2,
                "assists": 10,
                "last_hits": 100,
                "gold_per_min": 400,
                "hero_id": i + 1,
            })
        for i in range(5, 10):
            od_players.append({
                "account_id": i + 1000,
                "player_slot": 128 + (i - 5),  # Dire slots 128-132
                "kills": 3,
                "deaths": 4,
                "assists": 8,
                "last_hits": 80,
                "gold_per_min": 350,
                "hero_id": i + 1,
            })

        mock_opendota_api.get_match_details.return_value = {
            "match_id": 99999,
            "duration": 2400,
            "radiant_win": True,
            "radiant_score": 35,
            "dire_score": 22,
            "game_mode": 2,
            "players": od_players,
        }

        service = MatchEnrichmentService(match_repo, player_repo, mock_opendota_api)
        result = service.enrich_match(1, 99999)

        assert result["success"] is True
        assert result["players_enriched"] == 10
        assert result["fantasy_points_calculated"] is True
        assert "total_fantasy_points" in result
