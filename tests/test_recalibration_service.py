"""
Tests for the recalibration service.
"""

import time
import pytest

from database import Database
from repositories.player_repository import PlayerRepository
from repositories.recalibration_repository import RecalibrationRepository
from services.recalibration_service import RecalibrationService

TEST_GUILD_ID = 12345


@pytest.fixture
def services(repo_db_path):
    """Create player repo, recalibration repo, and service for testing."""
    player_repo = PlayerRepository(repo_db_path)
    recalibration_repo = RecalibrationRepository(repo_db_path)
    recalibration_service = RecalibrationService(
        recalibration_repo,
        player_repo,
        cooldown_seconds=3600,  # 1 hour for testing
        initial_rd=350.0,
        initial_volatility=0.06,
        min_games=5,
    )
    return {
        "player_repo": player_repo,
        "recalibration_repo": recalibration_repo,
        "service": recalibration_service,
        "db_path": repo_db_path,
    }


class TestRecalibrationService:
    """Tests for RecalibrationService."""

    def test_get_state_no_history(self, services):
        """Test get_state returns default state for player with no recalibration history."""
        service = services["service"]

        state = service.get_state(discord_id=12345, guild_id=TEST_GUILD_ID)

        assert state.discord_id == 12345
        assert state.last_recalibration_at is None
        assert state.total_recalibrations == 0
        assert state.is_on_cooldown is False
        assert state.cooldown_ends_at is None

    def test_can_recalibrate_not_registered(self, services):
        """Test can_recalibrate returns error for unregistered player."""
        service = services["service"]

        result = service.can_recalibrate(discord_id=99999, guild_id=TEST_GUILD_ID)

        assert result["allowed"] is False
        assert result["reason"] == "not_registered"

    def test_can_recalibrate_insufficient_games(self, services):
        """Test can_recalibrate returns error for player with insufficient games."""
        player_repo = services["player_repo"]
        service = services["service"]

        # Register a player with 0 games
        player_repo.add(
            discord_id=12345,
            discord_username="TestPlayer",
            glicko_rating=1500.0,
            glicko_rd=200.0,
            glicko_volatility=0.06,
            guild_id=TEST_GUILD_ID,
        )

        result = service.can_recalibrate(discord_id=12345, guild_id=TEST_GUILD_ID)

        assert result["allowed"] is False
        assert result["reason"] == "insufficient_games"
        assert result["games_played"] == 0
        assert result["min_games"] == 5

    def test_can_recalibrate_success(self, services):
        """Test can_recalibrate returns success for player with enough games."""
        player_repo = services["player_repo"]
        service = services["service"]

        # Register a player with enough games
        player_repo.add(
            discord_id=12345,
            discord_username="TestPlayer",
            glicko_rating=1500.0,
            glicko_rd=200.0,  # RD doesn't matter anymore
            glicko_volatility=0.06,
            guild_id=TEST_GUILD_ID,
        )
        # Add 5 wins to meet minimum games requirement
        for _ in range(5):
            player_repo.increment_wins(12345, TEST_GUILD_ID)

        result = service.can_recalibrate(discord_id=12345, guild_id=TEST_GUILD_ID)

        assert result["allowed"] is True
        assert result["current_rating"] == 1500.0
        assert result["current_rd"] == 200.0
        assert result["games_played"] == 5

    def test_can_recalibrate_on_cooldown(self, services):
        """Test can_recalibrate blocks during cooldown."""
        player_repo = services["player_repo"]
        recalibration_repo = services["recalibration_repo"]
        service = services["service"]

        # Register a player with enough games
        player_repo.add(
            discord_id=12345,
            discord_username="TestPlayer",
            glicko_rating=1500.0,
            glicko_rd=80.0,
            glicko_volatility=0.06,
            guild_id=TEST_GUILD_ID,
        )
        for _ in range(5):
            player_repo.increment_wins(12345, TEST_GUILD_ID)

        # Set a recent recalibration (within cooldown)
        now = int(time.time())
        recalibration_repo.upsert_state(discord_id=12345, guild_id=TEST_GUILD_ID,
            last_recalibration_at=now,
            total_recalibrations=1,
        )

        result = service.can_recalibrate(discord_id=12345, guild_id=TEST_GUILD_ID)

        assert result["allowed"] is False
        assert result["reason"] == "on_cooldown"
        assert result["cooldown_ends_at"] is not None

    def test_recalibrate_success(self, services):
        """Test successful recalibration."""
        player_repo = services["player_repo"]
        service = services["service"]

        # Register a player with enough games
        player_repo.add(
            discord_id=12345,
            discord_username="TestPlayer",
            glicko_rating=1500.0,
            glicko_rd=80.0,
            glicko_volatility=0.05,
            guild_id=TEST_GUILD_ID,
        )
        for _ in range(5):
            player_repo.increment_wins(12345, TEST_GUILD_ID)

        result = service.recalibrate(discord_id=12345, guild_id=TEST_GUILD_ID)

        assert result["success"] is True
        assert result["old_rating"] == 1500.0
        assert result["old_rd"] == 80.0
        assert result["new_rd"] == 350.0
        assert result["new_volatility"] == 0.06
        assert result["total_recalibrations"] == 1

        # Verify player's rating is preserved but RD is reset
        rating_data = player_repo.get_glicko_rating(12345, TEST_GUILD_ID)
        assert rating_data is not None
        rating, rd, vol = rating_data
        assert rating == 1500.0  # Rating unchanged
        assert rd == 350.0  # RD reset
        assert vol == 0.06  # Volatility reset

    def test_recalibrate_preserves_rating(self, services):
        """Test recalibration preserves current rating."""
        player_repo = services["player_repo"]
        service = services["service"]

        # Register a player with enough games and specific rating
        player_repo.add(
            discord_id=12345,
            discord_username="TestPlayer",
            glicko_rating=2000.0,
            glicko_rd=50.0,
            glicko_volatility=0.04,
            guild_id=TEST_GUILD_ID,
        )
        for _ in range(5):
            player_repo.increment_wins(12345, TEST_GUILD_ID)

        result = service.recalibrate(discord_id=12345, guild_id=TEST_GUILD_ID)

        assert result["success"] is True
        assert result["old_rating"] == 2000.0

        # Verify rating is preserved
        rating_data = player_repo.get_glicko_rating(12345, TEST_GUILD_ID)
        rating, rd, vol = rating_data
        assert rating == 2000.0

    def test_recalibrate_increments_count(self, services):
        """Test recalibration increments total count."""
        player_repo = services["player_repo"]
        recalibration_repo = services["recalibration_repo"]
        service = services["service"]

        # Register a player with enough games
        player_repo.add(
            discord_id=12345,
            discord_username="TestPlayer",
            glicko_rating=1500.0,
            glicko_rd=80.0,
            glicko_volatility=0.06,
            guild_id=TEST_GUILD_ID,
        )
        for _ in range(5):
            player_repo.increment_wins(12345, TEST_GUILD_ID)

        # Set a past recalibration (outside cooldown)
        past = int(time.time()) - 7200  # 2 hours ago
        recalibration_repo.upsert_state(discord_id=12345, guild_id=TEST_GUILD_ID,
            last_recalibration_at=past,
            total_recalibrations=2,
        )

        result = service.recalibrate(discord_id=12345, guild_id=TEST_GUILD_ID)

        assert result["success"] is True
        assert result["total_recalibrations"] == 3

        # Verify state was updated
        state = service.get_state(12345, TEST_GUILD_ID)
        assert state.total_recalibrations == 3

    def test_reset_cooldown(self, services):
        """Test admin cooldown reset."""
        player_repo = services["player_repo"]
        recalibration_repo = services["recalibration_repo"]
        service = services["service"]

        # Register a player
        player_repo.add(
            discord_id=12345,
            discord_username="TestPlayer",
            glicko_rating=1500.0,
            glicko_rd=80.0,
            glicko_volatility=0.06,
            guild_id=TEST_GUILD_ID,
        )

        # Set a recent recalibration (on cooldown)
        now = int(time.time())
        recalibration_repo.upsert_state(discord_id=12345, guild_id=TEST_GUILD_ID,
            last_recalibration_at=now,
            total_recalibrations=1,
        )

        # Verify on cooldown
        state_before = service.get_state(12345, TEST_GUILD_ID)
        assert state_before.is_on_cooldown is True

        # Reset cooldown
        result = service.reset_cooldown(12345, TEST_GUILD_ID)

        assert result["success"] is True

        # Verify no longer on cooldown
        state_after = service.get_state(12345, TEST_GUILD_ID)
        assert state_after.is_on_cooldown is False
        # Count should be preserved
        assert state_after.total_recalibrations == 1

    def test_reset_cooldown_no_history(self, services):
        """Test reset_cooldown fails for player with no history."""
        player_repo = services["player_repo"]
        service = services["service"]

        # Register a player with no recalibration history
        player_repo.add(
            discord_id=12345,
            discord_username="TestPlayer",
            glicko_rating=1500.0,
            glicko_rd=80.0,
            glicko_volatility=0.06,
            guild_id=TEST_GUILD_ID,
        )

        result = service.reset_cooldown(12345, TEST_GUILD_ID)

        assert result["success"] is False
        assert result["reason"] == "no_recalibration_history"


class TestRecalibrationRepository:
    """Tests for RecalibrationRepository."""

    def test_get_state_not_exists(self, repo_db_path):
        """Test get_state returns None for non-existent player."""
        repo = RecalibrationRepository(repo_db_path)

        state = repo.get_state(99999, TEST_GUILD_ID)

        assert state is None

    def test_upsert_state_create(self, repo_db_path):
        """Test upsert_state creates new record."""
        repo = RecalibrationRepository(repo_db_path)

        now = int(time.time())
        repo.upsert_state(discord_id=12345, guild_id=TEST_GUILD_ID,
            last_recalibration_at=now,
            total_recalibrations=1,
            rating_at_recalibration=1500.0,
        )

        state = repo.get_state(12345, TEST_GUILD_ID)
        assert state is not None
        assert state["discord_id"] == 12345
        assert state["last_recalibration_at"] == now
        assert state["total_recalibrations"] == 1
        assert state["rating_at_recalibration"] == 1500.0

    def test_upsert_state_update(self, repo_db_path):
        """Test upsert_state updates existing record."""
        repo = RecalibrationRepository(repo_db_path)

        # Create initial state
        old_time = int(time.time()) - 3600
        repo.upsert_state(discord_id=12345, guild_id=TEST_GUILD_ID,
            last_recalibration_at=old_time,
            total_recalibrations=1,
        )

        # Update with new values
        new_time = int(time.time())
        repo.upsert_state(discord_id=12345, guild_id=TEST_GUILD_ID,
            last_recalibration_at=new_time,
            total_recalibrations=2,
        )

        state = repo.get_state(12345, TEST_GUILD_ID)
        assert state["last_recalibration_at"] == new_time
        assert state["total_recalibrations"] == 2

    def test_reset_cooldown(self, repo_db_path):
        """Test reset_cooldown sets last_recalibration_at to 0."""
        repo = RecalibrationRepository(repo_db_path)

        # Create initial state
        now = int(time.time())
        repo.upsert_state(discord_id=12345, guild_id=TEST_GUILD_ID,
            last_recalibration_at=now,
            total_recalibrations=1,
        )

        # Reset cooldown
        repo.reset_cooldown(12345, TEST_GUILD_ID)

        state = repo.get_state(12345, TEST_GUILD_ID)
        assert state["last_recalibration_at"] == 0
        # Count should be preserved
        assert state["total_recalibrations"] == 1
