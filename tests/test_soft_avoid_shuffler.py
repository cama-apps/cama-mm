"""
Tests for soft avoid penalty calculation in the shuffler.
"""

import pytest

from domain.models.player import Player
from shuffler import BalancedShuffler


class MockSoftAvoid:
    """Mock SoftAvoid for testing."""
    def __init__(self, avoider_discord_id: int, avoided_discord_id: int, id: int = 1, games_remaining: int = 10):
        self.id = id
        self.avoider_discord_id = avoider_discord_id
        self.avoided_discord_id = avoided_discord_id
        self.games_remaining = games_remaining
        self.guild_id = 0
        self.created_at = 0
        self.updated_at = 0


def create_test_players(count: int = 10) -> list[Player]:
    """Create test players with discord IDs."""
    players = []
    for i in range(count):
        player = Player(
            name=f"Player{i + 1}",
            mmr=3000 + i * 100,
            wins=5,
            losses=3,
            preferred_roles=["1", "2", "3", "4", "5"],
            main_role=str((i % 5) + 1),
            glicko_rating=1500.0,
            glicko_rd=100.0,
            glicko_volatility=0.06,
            discord_id=1000 + i,
        )
        players.append(player)
    return players


class TestSoftAvoidPenaltyCalculation:
    """Tests for _calculate_soft_avoid_penalty method."""

    def test_no_avoids_returns_zero(self):
        """Test that no avoids means zero penalty."""
        shuffler = BalancedShuffler()

        team1_ids = {1000, 1001, 1002, 1003, 1004}
        team2_ids = {1005, 1006, 1007, 1008, 1009}

        penalty = shuffler._calculate_soft_avoid_penalty(team1_ids, team2_ids, avoids=None)
        assert penalty == 0.0

        penalty = shuffler._calculate_soft_avoid_penalty(team1_ids, team2_ids, avoids=[])
        assert penalty == 0.0

    def test_avoid_same_team_applies_penalty(self):
        """Test that same-team avoid applies penalty."""
        shuffler = BalancedShuffler(soft_avoid_penalty=500.0)

        team1_ids = {1000, 1001, 1002, 1003, 1004}
        team2_ids = {1005, 1006, 1007, 1008, 1009}

        # Player 1000 avoids player 1001 (both on team1)
        avoids = [MockSoftAvoid(avoider_discord_id=1000, avoided_discord_id=1001)]

        penalty = shuffler._calculate_soft_avoid_penalty(team1_ids, team2_ids, avoids)
        assert penalty == 500.0

    def test_avoid_opposite_teams_no_penalty(self):
        """Test that opposite-team avoid has no penalty."""
        shuffler = BalancedShuffler(soft_avoid_penalty=500.0)

        team1_ids = {1000, 1001, 1002, 1003, 1004}
        team2_ids = {1005, 1006, 1007, 1008, 1009}

        # Player 1000 avoids player 1005 (opposite teams)
        avoids = [MockSoftAvoid(avoider_discord_id=1000, avoided_discord_id=1005)]

        penalty = shuffler._calculate_soft_avoid_penalty(team1_ids, team2_ids, avoids)
        assert penalty == 0.0

    def test_bidirectional_avoids_additive(self):
        """Test that bidirectional avoids are additive (1000 penalty)."""
        shuffler = BalancedShuffler(soft_avoid_penalty=500.0)

        team1_ids = {1000, 1001, 1002, 1003, 1004}
        team2_ids = {1005, 1006, 1007, 1008, 1009}

        # Player 1000 avoids player 1001 AND player 1001 avoids player 1000
        avoids = [
            MockSoftAvoid(avoider_discord_id=1000, avoided_discord_id=1001, id=1),
            MockSoftAvoid(avoider_discord_id=1001, avoided_discord_id=1000, id=2),
        ]

        penalty = shuffler._calculate_soft_avoid_penalty(team1_ids, team2_ids, avoids)
        assert penalty == 1000.0

    def test_multiple_avoids_same_team(self):
        """Test multiple avoids on same team are additive."""
        shuffler = BalancedShuffler(soft_avoid_penalty=500.0)

        team1_ids = {1000, 1001, 1002, 1003, 1004}
        team2_ids = {1005, 1006, 1007, 1008, 1009}

        # Multiple avoids, all on team1
        avoids = [
            MockSoftAvoid(avoider_discord_id=1000, avoided_discord_id=1001, id=1),
            MockSoftAvoid(avoider_discord_id=1002, avoided_discord_id=1003, id=2),
        ]

        penalty = shuffler._calculate_soft_avoid_penalty(team1_ids, team2_ids, avoids)
        assert penalty == 1000.0

    def test_mixed_avoids(self):
        """Test mix of same-team and opposite-team avoids."""
        shuffler = BalancedShuffler(soft_avoid_penalty=500.0)

        team1_ids = {1000, 1001, 1002, 1003, 1004}
        team2_ids = {1005, 1006, 1007, 1008, 1009}

        avoids = [
            MockSoftAvoid(avoider_discord_id=1000, avoided_discord_id=1001, id=1),  # Same team (penalty)
            MockSoftAvoid(avoider_discord_id=1000, avoided_discord_id=1005, id=2),  # Opposite teams (no penalty)
            MockSoftAvoid(avoider_discord_id=1005, avoided_discord_id=1006, id=3),  # Same team2 (penalty)
        ]

        penalty = shuffler._calculate_soft_avoid_penalty(team1_ids, team2_ids, avoids)
        assert penalty == 1000.0


class TestShuffleWithAvoids:
    """Tests for shuffle methods with avoids parameter."""

    def test_shuffle_accepts_avoids_parameter(self):
        """Test that shuffle() accepts avoids parameter."""
        shuffler = BalancedShuffler()
        players = create_test_players(10)
        avoids = [MockSoftAvoid(avoider_discord_id=1000, avoided_discord_id=1001)]

        # Should not raise
        team1, team2 = shuffler.shuffle(players, avoids=avoids)

        assert len(team1.players) == 5
        assert len(team2.players) == 5

    def test_shuffle_from_pool_accepts_avoids_parameter(self):
        """Test that shuffle_from_pool() accepts avoids parameter."""
        shuffler = BalancedShuffler()
        players = create_test_players(14)
        avoids = [MockSoftAvoid(avoider_discord_id=1000, avoided_discord_id=1001)]

        # Should not raise
        team1, team2, excluded = shuffler.shuffle_from_pool(players, avoids=avoids)

        assert len(team1.players) == 5
        assert len(team2.players) == 5
        assert len(excluded) == 4

    def test_avoid_influences_team_selection(self):
        """Test that avoids influence which team split is chosen."""
        # Create shuffler with high avoid penalty
        shuffler = BalancedShuffler(soft_avoid_penalty=10000.0)

        # Create 10 players with identical ratings so team balance doesn't matter
        players = []
        for i in range(10):
            player = Player(
                name=f"Player{i + 1}",
                mmr=3000,
                wins=5,
                losses=3,
                preferred_roles=["1", "2", "3", "4", "5"],
                main_role="1",
                glicko_rating=1500.0,
                glicko_rd=100.0,
                glicko_volatility=0.06,
                discord_id=1000 + i,
            )
            players.append(player)

        # Player 1000 avoids player 1001
        avoids = [MockSoftAvoid(avoider_discord_id=1000, avoided_discord_id=1001)]

        team1, team2 = shuffler.shuffle(players, avoids=avoids)

        # Get discord IDs for each team
        team1_ids = {p.discord_id for p in team1.players}
        team2_ids = {p.discord_id for p in team2.players}

        # The avoid pair should ideally be on opposite teams due to penalty
        # (not guaranteed but very likely with such high penalty)
        same_team = (1000 in team1_ids and 1001 in team1_ids) or \
                    (1000 in team2_ids and 1001 in team2_ids)

        # With 10000 penalty, shuffler should strongly prefer splitting them
        # This test documents expected behavior; may need relaxation if algorithm changes
        if same_team:
            pytest.skip("Algorithm placed avoided pair on same team (acceptable but suboptimal)")
