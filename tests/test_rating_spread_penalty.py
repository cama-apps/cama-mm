"""
Tests for rating spread penalty in the shuffler.

The spread penalty = (max_rating - min_rating) / rating_spread_divisor
incentivizes pool shuffles to exclude outlier-rated players.
"""

import pytest

from domain.models.player import Player
from shuffler import BalancedShuffler


class TestRatingSpreadPenalty:
    """Tests for _calculate_rating_spread_penalty and its integration."""

    @pytest.fixture
    def shuffler(self):
        return BalancedShuffler(use_glicko=False, consider_roles=False)

    def test_spread_penalty_calculation(self, shuffler):
        """Basic penalty: (max - min) / divisor."""
        values = [1000.0, 1500.0, 2000.0]
        penalty = shuffler._calculate_rating_spread_penalty(values)
        assert penalty == pytest.approx(100.0)  # (2000 - 1000) / 10

    def test_spread_penalty_zero_when_equal(self, shuffler):
        """All same rating -> 0 penalty."""
        values = [1500.0] * 10
        penalty = shuffler._calculate_rating_spread_penalty(values)
        assert penalty == 0.0

    def test_spread_penalty_single_player(self, shuffler):
        """Single player -> 0 penalty (< 2 players)."""
        penalty = shuffler._calculate_rating_spread_penalty([1500.0])
        assert penalty == 0.0

    def test_spread_penalty_empty(self, shuffler):
        """Empty list -> 0 penalty."""
        penalty = shuffler._calculate_rating_spread_penalty([])
        assert penalty == 0.0

    def test_custom_divisor(self):
        """Custom divisor changes the penalty magnitude."""
        shuffler = BalancedShuffler(use_glicko=False, consider_roles=False, rating_spread_divisor=5.0)
        values = [1000.0, 2000.0]
        penalty = shuffler._calculate_rating_spread_penalty(values)
        assert penalty == pytest.approx(200.0)  # (2000 - 1000) / 5

    def test_default_divisor_from_config(self):
        """Default divisor should match RATING_SPREAD_DIVISOR config."""
        from config import RATING_SPREAD_DIVISOR
        shuffler = BalancedShuffler(use_glicko=False, consider_roles=False)
        assert shuffler.rating_spread_divisor == RATING_SPREAD_DIVISOR

    def test_pool_shuffle_penalizes_outlier(self):
        """Pool shuffle with 12 players should prefer excluding outliers over close-rated players."""
        import random
        random.seed(42)

        # 11 players at ~1500 MMR, 1 outlier at 3000 MMR
        close_players = [
            Player(f"Close{i}", 1400 + i * 20, preferred_roles=["1"], discord_id=100 + i)
            for i in range(11)
        ]
        outlier = Player("Outlier", 3000, preferred_roles=["1"], discord_id=200)
        players = close_players + [outlier]

        # Low spread divisor = strong penalty -> outlier should be excluded
        strong_shuffler = BalancedShuffler(
            use_glicko=False, consider_roles=False, rating_spread_divisor=1.0
        )

        # With strong penalty, the outlier should be excluded
        _, _, excluded = strong_shuffler.shuffle_from_pool(players)
        excluded_ids = {p.discord_id for p in excluded}
        assert 200 in excluded_ids, "Outlier (discord_id=200) should be excluded with strong spread penalty"
