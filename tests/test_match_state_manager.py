"""
Tests for MatchStateManager.

This manager handles in-memory state for pending matches after shuffle.
"""

import pytest

from domain.models.player import Player
from domain.models.team import Team
from services.match_state_manager import MatchState, MatchStateManager
from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY


# =============================================================================
# FIXTURES
# =============================================================================


@pytest.fixture
def state_manager():
    """Create a fresh MatchStateManager instance."""
    return MatchStateManager()


@pytest.fixture
def sample_team():
    """Create a sample Team with 5 players."""
    players = [
        Player(
            name=f"Player{i}",
            mmr=1500 + i * 100,
            glicko_rating=1500.0 + i * 20,
            discord_id=1000 + i,
        )
        for i in range(5)
    ]
    return Team(players)


@pytest.fixture
def sample_match_state(sample_team):
    """Create a sample MatchState for testing."""
    return MatchState(
        radiant_team_ids=[1000, 1001, 1002, 1003, 1004],
        dire_team_ids=[1005, 1006, 1007, 1008, 1009],
        excluded_player_ids=[1010, 1011],
        radiant_team=sample_team,
        dire_team=sample_team,
        radiant_roles=["1", "2", "3", "4", "5"],
        dire_roles=["1", "2", "3", "4", "5"],
        radiant_value=7500.0,
        dire_value=7450.0,
        first_pick_team="radiant",
    )


# =============================================================================
# MATCH STATE TESTS
# =============================================================================


class TestMatchState:
    """Test MatchState data class functionality."""

    def test_to_dict(self, sample_match_state):
        """Test converting MatchState to dictionary."""
        data = sample_match_state.to_dict()

        assert data["radiant_team_ids"] == [1000, 1001, 1002, 1003, 1004]
        assert data["dire_team_ids"] == [1005, 1006, 1007, 1008, 1009]
        assert data["excluded_player_ids"] == [1010, 1011]
        assert data["first_pick_team"] == "radiant"
        assert data["radiant_value"] == 7500.0
        assert data["dire_value"] == 7450.0

    def test_from_dict(self, sample_match_state):
        """Test creating MatchState from dictionary."""
        data = sample_match_state.to_dict()
        restored = MatchState.from_dict(data)

        assert restored.radiant_team_ids == sample_match_state.radiant_team_ids
        assert restored.dire_team_ids == sample_match_state.dire_team_ids
        assert restored.excluded_player_ids == sample_match_state.excluded_player_ids
        assert restored.first_pick_team == sample_match_state.first_pick_team

    def test_get_winning_ids_radiant(self, sample_match_state):
        """Test getting winning IDs when Radiant wins."""
        winners = sample_match_state.get_winning_ids("radiant")
        assert winners == [1000, 1001, 1002, 1003, 1004]

    def test_get_winning_ids_dire(self, sample_match_state):
        """Test getting winning IDs when Dire wins."""
        winners = sample_match_state.get_winning_ids("dire")
        assert winners == [1005, 1006, 1007, 1008, 1009]

    def test_get_losing_ids_radiant_wins(self, sample_match_state):
        """Test getting losing IDs when Radiant wins."""
        losers = sample_match_state.get_losing_ids("radiant")
        assert losers == [1005, 1006, 1007, 1008, 1009]

    def test_get_losing_ids_dire_wins(self, sample_match_state):
        """Test getting losing IDs when Dire wins."""
        losers = sample_match_state.get_losing_ids("dire")
        assert losers == [1000, 1001, 1002, 1003, 1004]

    def test_get_winning_ids_invalid_team(self, sample_match_state):
        """Test that invalid team name raises ValueError."""
        with pytest.raises(ValueError, match="Invalid winning_team"):
            sample_match_state.get_winning_ids("invalid")

    def test_get_losing_ids_invalid_team(self, sample_match_state):
        """Test that invalid team name raises ValueError."""
        with pytest.raises(ValueError, match="Invalid winning_team"):
            sample_match_state.get_losing_ids("invalid")

    def test_excluded_conditional_player_ids_defaults_empty(self, sample_team):
        """Test that excluded_conditional_player_ids defaults to empty list."""
        state = MatchState(
            radiant_team_ids=[1, 2, 3, 4, 5],
            dire_team_ids=[6, 7, 8, 9, 10],
            excluded_player_ids=[],
            radiant_team=sample_team,
            dire_team=sample_team,
            radiant_roles=["1", "2", "3", "4", "5"],
            dire_roles=["1", "2", "3", "4", "5"],
            radiant_value=0,
            dire_value=0,
            first_pick_team="radiant",
            # excluded_conditional_player_ids not provided
        )
        assert state.excluded_conditional_player_ids == []


# =============================================================================
# MATCH STATE MANAGER TESTS
# =============================================================================


class TestMatchStateManager:
    """Test MatchStateManager functionality."""

    def test_get_state_returns_none_when_empty(self, state_manager):
        """Test that get_state returns None when no state is set."""
        assert state_manager.get_state(TEST_GUILD_ID) is None

    def test_set_and_get_state(self, state_manager, sample_match_state):
        """Test setting and retrieving state."""
        state_manager.set_state(TEST_GUILD_ID, sample_match_state)

        retrieved = state_manager.get_state(TEST_GUILD_ID)

        assert retrieved is not None
        assert retrieved.radiant_team_ids == sample_match_state.radiant_team_ids
        assert retrieved.dire_team_ids == sample_match_state.dire_team_ids

    def test_clear_state(self, state_manager, sample_match_state):
        """Test clearing state."""
        state_manager.set_state(TEST_GUILD_ID, sample_match_state)
        assert state_manager.get_state(TEST_GUILD_ID) is not None

        state_manager.clear_state(TEST_GUILD_ID)

        assert state_manager.get_state(TEST_GUILD_ID) is None

    def test_clear_state_when_empty(self, state_manager):
        """Test that clearing non-existent state doesn't raise."""
        # Should not raise
        state_manager.clear_state(TEST_GUILD_ID)

    def test_has_pending_match(self, state_manager, sample_match_state):
        """Test checking if there's a pending match."""
        assert state_manager.has_pending_match(TEST_GUILD_ID) is False

        state_manager.set_state(TEST_GUILD_ID, sample_match_state)

        assert state_manager.has_pending_match(TEST_GUILD_ID) is True

    def test_has_pending_match_after_clear(self, state_manager, sample_match_state):
        """Test has_pending_match returns False after clearing."""
        state_manager.set_state(TEST_GUILD_ID, sample_match_state)
        state_manager.clear_state(TEST_GUILD_ID)

        assert state_manager.has_pending_match(TEST_GUILD_ID) is False


class TestMatchStateManagerGuildIsolation:
    """Test guild isolation in MatchStateManager."""

    def test_states_isolated_between_guilds(self, state_manager, sample_match_state, sample_team):
        """Test that states are isolated between different guilds."""
        state1 = sample_match_state
        state2 = MatchState(
            radiant_team_ids=[2000, 2001, 2002, 2003, 2004],
            dire_team_ids=[2005, 2006, 2007, 2008, 2009],
            excluded_player_ids=[],
            radiant_team=sample_team,
            dire_team=sample_team,
            radiant_roles=["1", "2", "3", "4", "5"],
            dire_roles=["1", "2", "3", "4", "5"],
            radiant_value=8000.0,
            dire_value=8000.0,
            first_pick_team="dire",
        )

        state_manager.set_state(TEST_GUILD_ID, state1)
        state_manager.set_state(TEST_GUILD_ID_SECONDARY, state2)

        retrieved1 = state_manager.get_state(TEST_GUILD_ID)
        retrieved2 = state_manager.get_state(TEST_GUILD_ID_SECONDARY)

        assert retrieved1.radiant_team_ids == [1000, 1001, 1002, 1003, 1004]
        assert retrieved2.radiant_team_ids == [2000, 2001, 2002, 2003, 2004]

    def test_clearing_one_guild_preserves_other(self, state_manager, sample_match_state):
        """Test that clearing one guild's state doesn't affect other guilds."""
        state_manager.set_state(TEST_GUILD_ID, sample_match_state)
        state_manager.set_state(TEST_GUILD_ID_SECONDARY, sample_match_state)

        state_manager.clear_state(TEST_GUILD_ID)

        assert state_manager.get_state(TEST_GUILD_ID) is None
        assert state_manager.get_state(TEST_GUILD_ID_SECONDARY) is not None

    def test_null_guild_id_normalized(self, state_manager, sample_match_state):
        """Test that None guild_id is normalized to 0."""
        state_manager.set_state(None, sample_match_state)

        # Should be retrievable with None
        assert state_manager.get_state(None) is not None

        # Should be retrievable with 0 (normalized value)
        assert state_manager.get_state(0) is not None


class TestMatchStateManagerLegacyMethods:
    """Test legacy compatibility methods."""

    def test_get_last_shuffle_returns_dict(self, state_manager, sample_match_state):
        """Test legacy get_last_shuffle returns dictionary."""
        state_manager.set_state(TEST_GUILD_ID, sample_match_state)

        result = state_manager.get_last_shuffle(TEST_GUILD_ID)

        assert isinstance(result, dict)
        assert result["radiant_team_ids"] == sample_match_state.radiant_team_ids

    def test_get_last_shuffle_returns_none_when_empty(self, state_manager):
        """Test legacy get_last_shuffle returns None when no state."""
        result = state_manager.get_last_shuffle(TEST_GUILD_ID)
        assert result is None

    def test_set_last_shuffle_from_dict(self, state_manager, sample_match_state):
        """Test legacy set_last_shuffle from dictionary."""
        payload = sample_match_state.to_dict()

        state_manager.set_last_shuffle(TEST_GUILD_ID, payload)

        state = state_manager.get_state(TEST_GUILD_ID)
        assert state is not None
        assert state.radiant_team_ids == sample_match_state.radiant_team_ids

    def test_clear_last_shuffle(self, state_manager, sample_match_state):
        """Test legacy clear_last_shuffle."""
        state_manager.set_state(TEST_GUILD_ID, sample_match_state)

        state_manager.clear_last_shuffle(TEST_GUILD_ID)

        assert state_manager.get_state(TEST_GUILD_ID) is None


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
