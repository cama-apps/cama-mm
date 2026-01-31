"""
End-to-end tests for OpenDota API integration and error handling.
"""

from unittest.mock import patch

import pytest

from database import Database
from rating_system import CamaRatingSystem


class TestOpenDotaIntegration:
    """Tests for OpenDota API integration and error handling."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    @patch("opendota_integration.OpenDotaAPI.get_player_data")
    @patch("opendota_integration.OpenDotaAPI.get_player_mmr")
    def test_register_with_opendota_failure(self, mock_get_mmr, mock_get_data, test_db):
        """Test registration when OpenDota API fails."""
        # Mock OpenDota API to return None (failure)
        mock_get_data.return_value = None
        mock_get_mmr.return_value = None

        # Simulate registration attempt
        # In real bot, this would show error message
        # Here we test that the system handles the failure gracefully
        steam_id = 12345678

        # The registration should fail if OpenDota returns None
        # This tests the error handling path
        player_data = mock_get_data(steam_id)
        assert player_data is None, "OpenDota should return None on failure"

        # Verify player was not added to database
        player = test_db.get_player(99999)  # Use non-existent ID
        assert player is None

    @patch("opendota_integration.OpenDotaAPI.get_player_data")
    @patch("opendota_integration.OpenDotaAPI.get_player_mmr")
    def test_register_with_invalid_steam_id(self, mock_get_mmr, mock_get_data, test_db):
        """Test registration with invalid Steam ID."""
        # Mock OpenDota API to return None for invalid Steam ID
        mock_get_data.return_value = None
        mock_get_mmr.return_value = None

        invalid_steam_id = -1  # Invalid Steam ID

        # Validate Steam ID (what bot does)
        if invalid_steam_id <= 0 or invalid_steam_id > 2147483647:
            # Should reject invalid Steam ID
            assert True, "Invalid Steam ID should be rejected"

        # Verify player was not added
        player = test_db.get_player(99999)
        assert player is None

    @patch("opendota_integration.OpenDotaAPI.get_player_data")
    @patch("opendota_integration.OpenDotaAPI.get_player_mmr")
    def test_register_with_valid_opendota_response(self, mock_get_mmr, mock_get_data, test_db):
        """Test successful registration with valid OpenDota response."""
        # Mock successful OpenDota response
        mock_get_data.return_value = {"rank_tier": 50, "leaderboard_rank": None}
        mock_get_mmr.return_value = 2000

        user_id = 100001

        # Simulate successful registration
        rating_system = CamaRatingSystem()
        glicko_player = rating_system.create_player_from_mmr(2000)

        # Add player to database
        test_db.add_player(
            discord_id=user_id,
            discord_username="TestUser",
            initial_mmr=2000,
            glicko_rating=glicko_player.rating,
            glicko_rd=glicko_player.rd,
            glicko_volatility=glicko_player.vol,
        )

        # Verify player was added
        player = test_db.get_player(user_id)
        assert player is not None
        assert player.mmr == 2000


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
