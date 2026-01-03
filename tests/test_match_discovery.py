"""
Tests for MatchDiscoveryService and related functionality.
"""

import json
import pytest
from unittest.mock import Mock, patch
from datetime import datetime

from services.match_discovery_service import (
    MatchDiscoveryService,
    MIN_PLAYERS_FOR_DISCOVERY,
    MIN_CONFIDENCE_FOR_AUTO,
    TIME_WINDOW_SECONDS,
)


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
            {"discord_id": i} for i in range(1, 11)  # 10 players
        ]

        # All players have steam_ids
        player_repo.get_steam_id.side_effect = list(range(1001, 1011))

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
        match_repo.get_match_participants.return_value = [
            {"discord_id": i} for i in range(1, 11)
        ]

        # All players have steam_ids
        player_repo.get_steam_id.side_effect = list(range(1001, 1011))

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
        match_repo.get_match_participants.return_value = [
            {"discord_id": i} for i in range(1, 11)
        ]

        # Only 3 players have steam_ids (below MIN_PLAYERS_FOR_DISCOVERY)
        def mock_get_steam_id(discord_id):
            if discord_id <= 3:
                return discord_id + 1000
            return None

        player_repo.get_steam_id.side_effect = mock_get_steam_id

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
        match_repo.get_match_participants.return_value = [
            {"discord_id": i} for i in range(1, 11)
        ]

        player_repo.get_steam_id.side_effect = list(range(1001, 1011))

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
        match_repo.get_match_participants.return_value = [
            {"discord_id": i} for i in range(1, 11)
        ]

        player_repo.get_steam_id.side_effect = list(range(1001, 1011))

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
        match_repo.get_match_participants.return_value = [
            {"discord_id": i} for i in range(1, 11)
        ]

        # Only 3 steam_ids (will skip both)
        player_repo.get_steam_id.side_effect = lambda x: x + 1000 if x <= 3 else None

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
        match_id = repo.record_match([1, 2, 3, 4, 5], [6, 7, 8, 9, 10], 1)
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
        match = repo.get_most_recent_match()
        assert match["valve_match_id"] == 99999

        # Wipe it
        success = repo.wipe_match_enrichment(match_id)
        assert success is True

        # Verify wiped
        match = repo.get_most_recent_match()
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
        match1 = repo.record_match([1, 2, 3, 4, 5], [6, 7, 8, 9, 10], 1)
        repo.update_match_enrichment(
            match_id=match1,
            valve_match_id=11111,
            duration_seconds=2400,
            radiant_score=35,
            dire_score=22,
            game_mode=2,
            enrichment_source="manual",
        )

        match2 = repo.record_match([11, 12, 13, 14, 15], [16, 17, 18, 19, 20], 2)
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

        match3 = repo.record_match([21, 22, 23, 24, 25], [26, 27, 28, 29, 30], 1)
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
        assert repo.get_auto_discovered_count() == 2

        # Wipe auto-discovered
        wiped = repo.wipe_auto_discovered_enrichments()
        assert wiped == 2

        # Verify manual one still enriched
        unenriched = repo.get_matches_without_enrichment()
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
        assert repo.get_auto_discovered_count() == 0

        # Add an auto-discovered match
        match_id = repo.record_match([1, 2, 3, 4, 5], [6, 7, 8, 9, 10], 1)
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

        assert repo.get_auto_discovered_count() == 1

    def test_enrichment_source_and_confidence_stored(self, temp_db_path):
        """Test enrichment source and confidence are properly stored."""
        from infrastructure.schema_manager import SchemaManager
        from repositories.match_repository import MatchRepository
        import sqlite3

        SchemaManager(temp_db_path).initialize()
        repo = MatchRepository(temp_db_path)

        match_id = repo.record_match([1, 2, 3, 4, 5], [6, 7, 8, 9, 10], 1)
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
            (match_id,)
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

        match_repo.get_match_participants.return_value = []

        service = MatchEnrichmentService(match_repo, player_repo, mock_opendota_api)
        service.enrich_match(1, 99999)

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

        match_repo.get_match_participants.return_value = []

        service = MatchEnrichmentService(match_repo, player_repo, mock_opendota_api)
        service.enrich_match(1, 99999, source="auto", confidence=0.9)

        call_kwargs = match_repo.update_match_enrichment.call_args[1]
        assert call_kwargs["enrichment_source"] == "auto"
        assert call_kwargs["enrichment_confidence"] == 0.9
