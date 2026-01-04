"""
Tests for the application services layer.
"""

from domain.models.player import Player
from domain.models.team import Team
from services.match_state_manager import MatchState, MatchStateManager


class TestMatchStateManager:
    """Tests for MatchStateManager."""

    def test_set_and_get_state(self):
        """Test setting and getting match state."""
        manager = MatchStateManager()

        # Create mock teams
        players = [Player(name=f"P{i}", mmr=3000, preferred_roles=["1"]) for i in range(5)]
        team = Team(players)

        state = MatchState(
            radiant_team_ids=[1, 2, 3, 4, 5],
            dire_team_ids=[6, 7, 8, 9, 10],
            excluded_player_ids=[],
            radiant_team=team,
            dire_team=team,
            radiant_roles=["1", "2", "3", "4", "5"],
            dire_roles=["1", "2", "3", "4", "5"],
            radiant_value=7500,
            dire_value=7500,
            first_pick_team="Radiant",
        )

        manager.set_state(123, state)

        retrieved = manager.get_state(123)
        assert retrieved is not None
        assert retrieved.radiant_team_ids == [1, 2, 3, 4, 5]
        assert retrieved.first_pick_team == "Radiant"

    def test_clear_state(self):
        """Test clearing match state."""
        manager = MatchStateManager()

        players = [Player(name=f"P{i}", mmr=3000, preferred_roles=["1"]) for i in range(5)]
        team = Team(players)

        state = MatchState(
            radiant_team_ids=[1, 2, 3, 4, 5],
            dire_team_ids=[6, 7, 8, 9, 10],
            excluded_player_ids=[],
            radiant_team=team,
            dire_team=team,
            radiant_roles=["1", "2", "3", "4", "5"],
            dire_roles=["1", "2", "3", "4", "5"],
            radiant_value=7500,
            dire_value=7500,
            first_pick_team="Radiant",
        )

        manager.set_state(123, state)
        assert manager.has_pending_match(123) is True

        manager.clear_state(123)
        assert manager.has_pending_match(123) is False
        assert manager.get_state(123) is None

    def test_get_winning_losing_ids(self):
        """Test getting winner/loser IDs from state."""
        players = [Player(name=f"P{i}", mmr=3000, preferred_roles=["1"]) for i in range(5)]
        team = Team(players)

        state = MatchState(
            radiant_team_ids=[1, 2, 3, 4, 5],
            dire_team_ids=[6, 7, 8, 9, 10],
            excluded_player_ids=[],
            radiant_team=team,
            dire_team=team,
            radiant_roles=["1", "2", "3", "4", "5"],
            dire_roles=["1", "2", "3", "4", "5"],
            radiant_value=7500,
            dire_value=7500,
            first_pick_team="Radiant",
        )

        assert state.get_winning_ids("radiant") == [1, 2, 3, 4, 5]
        assert state.get_losing_ids("radiant") == [6, 7, 8, 9, 10]
        assert state.get_winning_ids("dire") == [6, 7, 8, 9, 10]
        assert state.get_losing_ids("dire") == [1, 2, 3, 4, 5]
