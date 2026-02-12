"""
Tests for package deal integration with the shuffler.
"""

import pytest
from dataclasses import dataclass
from domain.models.player import Player
from shuffler import BalancedShuffler


@dataclass
class MockPackageDeal:
    """Mock PackageDeal for testing."""
    id: int
    buyer_discord_id: int
    partner_discord_id: int
    games_remaining: int = 10


class TestPackageDealPenalty:
    """Tests for package deal penalty calculations in shuffler."""

    @pytest.fixture
    def shuffler(self):
        """Create a shuffler with default settings."""
        return BalancedShuffler(use_glicko=True, consider_roles=True)

    @pytest.fixture
    def sample_players(self):
        """Create 10 sample players with unique discord_ids."""
        return [
            Player(f"Player{i}", 4000, preferred_roles=["3"], discord_id=100 + i)
            for i in range(10)
        ]

    def test_package_deal_penalty_when_separated(self, shuffler):
        """Test that penalty is applied when deal pair is on different teams."""
        team1_ids = {100, 101, 102, 103, 104}
        team2_ids = {105, 106, 107, 108, 109}

        # Deal between 100 and 105 (different teams)
        deals = [MockPackageDeal(id=1, buyer_discord_id=100, partner_discord_id=105)]

        penalty = shuffler._calculate_package_deal_penalty(team1_ids, team2_ids, deals)
        assert penalty == shuffler.package_deal_penalty

    def test_no_penalty_when_together(self, shuffler):
        """Test that no penalty is applied when deal pair is on same team."""
        team1_ids = {100, 101, 102, 103, 104}
        team2_ids = {105, 106, 107, 108, 109}

        # Deal between 100 and 101 (same team)
        deals = [MockPackageDeal(id=1, buyer_discord_id=100, partner_discord_id=101)]

        penalty = shuffler._calculate_package_deal_penalty(team1_ids, team2_ids, deals)
        assert penalty == 0.0

    def test_multiple_deals_penalty_stacks(self, shuffler):
        """Test that multiple violated deals stack penalties."""
        team1_ids = {100, 101, 102, 103, 104}
        team2_ids = {105, 106, 107, 108, 109}

        # Two deals crossing teams
        deals = [
            MockPackageDeal(id=1, buyer_discord_id=100, partner_discord_id=105),
            MockPackageDeal(id=2, buyer_discord_id=101, partner_discord_id=106),
        ]

        penalty = shuffler._calculate_package_deal_penalty(team1_ids, team2_ids, deals)
        assert penalty == 2 * shuffler.package_deal_penalty

    def test_bidirectional_deals_stack(self, shuffler):
        """Test that A->B and B->A deals both apply penalties."""
        team1_ids = {100, 101, 102, 103, 104}
        team2_ids = {105, 106, 107, 108, 109}

        # Bidirectional deals crossing teams
        deals = [
            MockPackageDeal(id=1, buyer_discord_id=100, partner_discord_id=105),
            MockPackageDeal(id=2, buyer_discord_id=105, partner_discord_id=100),
        ]

        penalty = shuffler._calculate_package_deal_penalty(team1_ids, team2_ids, deals)
        assert penalty == 2 * shuffler.package_deal_penalty

    def test_no_deals_no_penalty(self, shuffler):
        """Test that empty deals list results in no penalty."""
        team1_ids = {100, 101, 102, 103, 104}
        team2_ids = {105, 106, 107, 108, 109}

        penalty = shuffler._calculate_package_deal_penalty(team1_ids, team2_ids, [])
        assert penalty == 0.0

        penalty = shuffler._calculate_package_deal_penalty(team1_ids, team2_ids, None)
        assert penalty == 0.0

    def test_shuffle_respects_package_deals(self, sample_players):
        """Test that shuffle considers package deals in optimization."""
        shuffler = BalancedShuffler(
            use_glicko=True,
            consider_roles=True,
            package_deal_penalty=10000.0,  # Very high penalty to force same team
        )

        # Strong deal between player 0 (id=100) and player 5 (id=105)
        deals = [MockPackageDeal(id=1, buyer_discord_id=100, partner_discord_id=105)]

        team1, team2 = shuffler.shuffle(sample_players, deals=deals)

        # With high penalty, they should be on same team
        team1_ids = {p.discord_id for p in team1.players}
        team2_ids = {p.discord_id for p in team2.players}

        # Check if they're together
        both_team1 = 100 in team1_ids and 105 in team1_ids
        both_team2 = 100 in team2_ids and 105 in team2_ids

        assert both_team1 or both_team2, "Package deal pair should be on same team"

    def test_package_deal_penalty_customizable(self):
        """Test that package deal penalty is configurable."""
        shuffler = BalancedShuffler(package_deal_penalty=999.0)
        assert shuffler.package_deal_penalty == 999.0

    def test_package_deal_vs_soft_avoid_independence(self, shuffler):
        """Test that package deal and soft avoid penalties are independent."""
        team1_ids = {100, 101, 102, 103, 104}
        team2_ids = {105, 106, 107, 108, 109}

        deals = [MockPackageDeal(id=1, buyer_discord_id=100, partner_discord_id=105)]

        # Package deal penalty (opposite teams)
        deal_penalty = shuffler._calculate_package_deal_penalty(team1_ids, team2_ids, deals)

        # Mock soft avoid with same structure
        @dataclass
        class MockSoftAvoid:
            id: int
            avoider_discord_id: int
            avoided_discord_id: int
            games_remaining: int = 10

        avoids = [MockSoftAvoid(id=1, avoider_discord_id=100, avoided_discord_id=101)]

        # Soft avoid penalty (same teams)
        avoid_penalty = shuffler._calculate_soft_avoid_penalty(team1_ids, team2_ids, avoids)

        # Both should be calculated independently
        assert deal_penalty == shuffler.package_deal_penalty
        assert avoid_penalty == shuffler.soft_avoid_penalty
        assert deal_penalty != avoid_penalty  # Different default values


class TestPackageDealWithPoolShuffle:
    """Tests for package deals with >10 player shuffles."""

    @pytest.fixture
    def sample_14_players(self):
        """Create 14 sample players."""
        return [
            Player(f"Player{i}", 4000, preferred_roles=["3"], discord_id=100 + i)
            for i in range(14)
        ]

    def test_pool_shuffle_with_deals(self, sample_14_players):
        """Test that pool shuffle passes deals correctly."""
        shuffler = BalancedShuffler(
            use_glicko=True,
            consider_roles=True,
            package_deal_penalty=5000.0,  # High penalty
        )

        # Deal between player 0 and player 5
        deals = [MockPackageDeal(id=1, buyer_discord_id=100, partner_discord_id=105)]

        team1, team2, excluded = shuffler.shuffle_from_pool(
            sample_14_players,
            deals=deals,
        )

        # If both are included, they should be together
        included_ids = {p.discord_id for p in team1.players} | {p.discord_id for p in team2.players}
        excluded_ids = {p.discord_id for p in excluded}

        if 100 in included_ids and 105 in included_ids:
            team1_ids = {p.discord_id for p in team1.players}
            team2_ids = {p.discord_id for p in team2.players}

            both_team1 = 100 in team1_ids and 105 in team1_ids
            both_team2 = 100 in team2_ids and 105 in team2_ids

            assert both_team1 or both_team2, "Package deal pair should be on same team when both included"
