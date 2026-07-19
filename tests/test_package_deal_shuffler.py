"""
Tests for package deal integration with the shuffler.
"""

from dataclasses import dataclass

import pytest

from domain.models.player import Player
from domain.models.team import Team
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
    def sample_pool_players(self):
        """Create 11 sample players — the smallest pool that still exercises
        the >10 ``shuffle_from_pool`` path (one player is excluded).

        Kept at 11 (not 14) deliberately: ``shuffle_from_pool`` cost grows
        combinatorially with pool size (~0.13s at 11 vs ~11s at 14), and this
        test only needs the pool branch + deal pass-through, not a specific
        pool size. The 14-player path is covered by the branch-and-bound tests
        below, which genuinely require exactly 14.
        """
        return [
            Player(f"Player{i}", 4000, preferred_roles=["3"], discord_id=100 + i)
            for i in range(11)
        ]

    def test_pool_shuffle_with_deals(self, sample_pool_players):
        """Test that pool shuffle passes deals correctly."""
        shuffler = BalancedShuffler(
            use_glicko=True,
            consider_roles=True,
            package_deal_penalty=5000.0,  # High penalty: keep the pair on the same team
            package_deal_split_penalty=5000.0,  # High penalty: keep the pair in the match
        )

        # Deal between player 0 and player 5
        deals = [MockPackageDeal(id=1, buyer_discord_id=100, partner_discord_id=105)]

        team1, team2, _ = shuffler.shuffle_from_pool(
            sample_pool_players,
            deals=deals,
        )

        team1_ids = {p.discord_id for p in team1.players}
        team2_ids = {p.discord_id for p in team2.players}
        included_ids = team1_ids | team2_ids

        # All 11 players are identical, so excluding a non-deal player is always
        # available; with the high split penalty both deal members must be included.
        assert 100 in included_ids and 105 in included_ids, (
            "Package deal pair should both be included when splitting them is penalized"
        )

        both_team1 = 100 in team1_ids and 105 in team1_ids
        both_team2 = 100 in team2_ids and 105 in team2_ids
        assert both_team1 or both_team2, "Package deal pair should be on same team"

    def test_branch_bound_respects_split_penalty(self, monkeypatch):
        """Test that 14-player branch-and-bound shuffle respects split penalty."""
        # Create 14 players with equal ratings so split penalty is the deciding factor
        players = [
            Player(f"Player{i}", 2000, preferred_roles=["3"], discord_id=100 + i, glicko_rating=2000)
            for i in range(14)
        ]

        shuffler = BalancedShuffler(
            use_glicko=True,
            consider_roles=True,
            package_deal_split_penalty=5000.0,  # Very high penalty to force keeping together
        )

        # Split the pair in both the mocked greedy result and the first
        # branch-and-bound combination so the penalty must change the result.
        deals = [MockPackageDeal(id=1, buyer_discord_id=100, partner_discord_id=110)]
        roles = ["1", "2", "3", "4", "5"]
        greedy_result = (
            Team(players[:5], role_assignments=roles),
            Team(players[5:10], role_assignments=roles),
            players[10:],
            float("inf"),
        )
        forwarded_deals = []

        def greedy(*_args, deals=None, **_kwargs):
            forwarded_deals.append(deals)
            return greedy_result

        monkeypatch.setattr(shuffler, "_greedy_shuffle", greedy)
        monkeypatch.setattr(
            shuffler,
            "_optimize_role_assignments_for_matchup",
            lambda team1, team2, **kwargs: (
                Team(team1, role_assignments=roles),
                Team(team2, role_assignments=roles),
                0.0,
            ),
        )

        team1, team2, excluded = shuffler.shuffle_branch_bound(
            players,
            deals=deals,
        )

        # With high split penalty, both should be either included or excluded together
        included_ids = {p.discord_id for p in team1.players} | {p.discord_id for p in team2.players}
        excluded_ids = {p.discord_id for p in excluded}

        both_included = 100 in included_ids and 110 in included_ids
        both_excluded = 100 in excluded_ids and 110 in excluded_ids

        assert forwarded_deals == [deals]
        assert both_included or both_excluded, "Package deal pair should not be split in branch-and-bound"

    def test_greedy_shuffle_includes_split_penalty(self, monkeypatch):
        """Test that greedy shuffle (used as upper bound) includes split penalty."""
        players = [
            Player(f"Player{i}", 2000, preferred_roles=["3"], discord_id=100 + i, glicko_rating=2000)
            for i in range(14)
        ]

        shuffler = BalancedShuffler(
            use_glicko=True,
            consider_roles=True,
            package_deal_split_penalty=1000.0,
        )

        # Deal between player 0 (selected) and player 13 (likely excluded in greedy)
        deals = [MockPackageDeal(id=1, buyer_discord_id=100, partner_discord_id=113)]
        roles = ["1", "2", "3", "4", "5"]
        monkeypatch.setattr(
            shuffler,
            "_optimize_role_assignments_for_matchup",
            lambda team1, team2, **kwargs: (
                Team(team1, role_assignments=roles),
                Team(team2, role_assignments=roles),
                0.0,
            ),
        )

        team1, team2, excluded, score_with_deal = shuffler._greedy_shuffle(
            players,
            deals=deals,
        )
        _, _, _, score_without_deal = shuffler._greedy_shuffle(players)

        excluded_ids = {p.discord_id for p in excluded}
        included_ids = {p.discord_id for p in team1.players + team2.players}

        assert (100 in included_ids) != (113 in included_ids)
        assert (100 in excluded_ids) != (113 in excluded_ids)
        assert score_with_deal - score_without_deal == shuffler.package_deal_split_penalty


class TestPackageDealSplitPenalty:
    """Tests for package deal split penalty calculations in shuffler."""

    @pytest.fixture
    def shuffler(self):
        """Create a shuffler with default settings."""
        return BalancedShuffler(use_glicko=True, consider_roles=True)

    def test_split_penalty_when_one_excluded(self, shuffler):
        """Test that penalty is applied when one of the pair is excluded."""
        selected_ids = {100, 101, 102, 103, 104, 105, 106, 107, 108, 109}
        excluded_ids = {110, 111, 112, 113}

        # Deal between 100 (selected) and 110 (excluded)
        deals = [MockPackageDeal(id=1, buyer_discord_id=100, partner_discord_id=110)]

        penalty = shuffler._calculate_package_deal_split_penalty(selected_ids, excluded_ids, deals)
        assert penalty == shuffler.package_deal_split_penalty

    def test_no_split_penalty_when_both_selected(self, shuffler):
        """Test that no penalty is applied when both are selected."""
        selected_ids = {100, 101, 102, 103, 104, 105, 106, 107, 108, 109}
        excluded_ids = {110, 111, 112, 113}

        # Deal between 100 and 101 (both selected)
        deals = [MockPackageDeal(id=1, buyer_discord_id=100, partner_discord_id=101)]

        penalty = shuffler._calculate_package_deal_split_penalty(selected_ids, excluded_ids, deals)
        assert penalty == 0.0

    def test_no_split_penalty_when_both_excluded(self, shuffler):
        """Test that no penalty is applied when both are excluded."""
        selected_ids = {100, 101, 102, 103, 104, 105, 106, 107, 108, 109}
        excluded_ids = {110, 111, 112, 113}

        # Deal between 110 and 111 (both excluded)
        deals = [MockPackageDeal(id=1, buyer_discord_id=110, partner_discord_id=111)]

        penalty = shuffler._calculate_package_deal_split_penalty(selected_ids, excluded_ids, deals)
        assert penalty == 0.0

    def test_multiple_splits_stack(self, shuffler):
        """Test that multiple split deals stack penalties."""
        selected_ids = {100, 101, 102, 103, 104, 105, 106, 107, 108, 109}
        excluded_ids = {110, 111, 112, 113}

        # Two deals where pairs are split
        deals = [
            MockPackageDeal(id=1, buyer_discord_id=100, partner_discord_id=110),
            MockPackageDeal(id=2, buyer_discord_id=101, partner_discord_id=111),
        ]

        penalty = shuffler._calculate_package_deal_split_penalty(selected_ids, excluded_ids, deals)
        assert penalty == 2 * shuffler.package_deal_split_penalty

    def test_reverse_split_direction(self, shuffler):
        """Test that split is detected regardless of buyer/partner direction."""
        selected_ids = {100, 101, 102, 103, 104, 105, 106, 107, 108, 109}
        excluded_ids = {110, 111, 112, 113}

        # Deal where buyer is excluded, partner is selected
        deals = [MockPackageDeal(id=1, buyer_discord_id=110, partner_discord_id=100)]

        penalty = shuffler._calculate_package_deal_split_penalty(selected_ids, excluded_ids, deals)
        assert penalty == shuffler.package_deal_split_penalty

    def test_no_deals_no_split_penalty(self, shuffler):
        """Test that empty deals list results in no penalty."""
        selected_ids = {100, 101, 102, 103, 104, 105, 106, 107, 108, 109}
        excluded_ids = {110, 111, 112, 113}

        penalty = shuffler._calculate_package_deal_split_penalty(selected_ids, excluded_ids, [])
        assert penalty == 0.0

        penalty = shuffler._calculate_package_deal_split_penalty(selected_ids, excluded_ids, None)
        assert penalty == 0.0

    def test_split_penalty_customizable(self):
        """Test that split penalty is configurable."""
        shuffler = BalancedShuffler(package_deal_split_penalty=888.0)
        assert shuffler.package_deal_split_penalty == 888.0

    def test_pool_shuffle_prefers_keeping_deals_together(self):
        """Integration test: pool shuffle should prefer including both deal members."""
        # Create 12 players with widely varying ratings
        # Make the deal pair have middling ratings so they could be excluded
        players = []
        for i in range(12):
            if i < 5:
                # High rated players
                rating = 2500
            elif i < 7:
                # Deal pair - middling ratings
                rating = 2000
            else:
                # Lower rated players
                rating = 1500
            players.append(
                Player(f"Player{i}", rating, preferred_roles=["3"], discord_id=100 + i, glicko_rating=rating)
            )

        # High split penalty - should strongly prefer keeping deal together
        shuffler = BalancedShuffler(
            use_glicko=True,
            consider_roles=True,
            package_deal_split_penalty=5000.0,  # Very high penalty
        )

        # Deal between player 5 (id=105) and player 6 (id=106) - the middling pair
        deals = [MockPackageDeal(id=1, buyer_discord_id=105, partner_discord_id=106)]

        team1, team2, excluded = shuffler.shuffle_from_pool(
            players,
            deals=deals,
        )

        included_ids = {p.discord_id for p in team1.players} | {p.discord_id for p in team2.players}
        excluded_ids = {p.discord_id for p in excluded}

        # With high split penalty, both should be either included or excluded together
        both_included = 105 in included_ids and 106 in included_ids
        both_excluded = 105 in excluded_ids and 106 in excluded_ids

        assert both_included or both_excluded, "Package deal pair should not be split"
