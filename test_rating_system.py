"""
Tests for rating system edge cases and error handling.
"""

import pytest

from rating_system import CamaRatingSystem
from config import CALIBRATION_RD_THRESHOLD, RD_DECAY_CONSTANT, RD_DECAY_GRACE_PERIOD_WEEKS


class TestRatingSystemEdgeCases:
    """Test edge cases in the rating system."""

    def test_mmr_to_rating_extreme_values(self):
        """Test MMR to rating conversion with extreme values."""
        rating_system = CamaRatingSystem()

        # Test minimum MMR
        rating_min = rating_system.mmr_to_rating(0)
        assert rating_min >= 0, "Minimum rating should be >= 0"

        # Test maximum MMR
        rating_max = rating_system.mmr_to_rating(12000)
        assert rating_max <= 3000, "Maximum rating should be <= 3000"

        # Test negative MMR (should clamp)
        rating_negative = rating_system.mmr_to_rating(-100)
        assert rating_negative >= 0, "Negative MMR should clamp to >= 0"

        # Test very high MMR (should clamp)
        rating_very_high = rating_system.mmr_to_rating(20000)
        assert rating_very_high <= 3000, "Very high MMR should clamp to <= 3000"

    def test_mmr_to_rating_linear_mapping(self):
        """Test that MMR to rating mapping is linear."""
        rating_system = CamaRatingSystem()

        # Test middle values
        mmr_mid = 6000
        rating_mid = rating_system.mmr_to_rating(mmr_mid)

        # Should be approximately in the middle
        assert 1000 < rating_mid < 2000, f"Middle MMR should map to middle rating, got {rating_mid}"

        # Test that higher MMR gives higher rating
        rating_low = rating_system.mmr_to_rating(3000)
        rating_high = rating_system.mmr_to_rating(9000)
        assert rating_high > rating_low, "Higher MMR should give higher rating"

    def test_create_player_from_none_mmr(self):
        """Test creating player when MMR is None."""
        rating_system = CamaRatingSystem()

        # Create player with None MMR (should use default)
        player = rating_system.create_player_from_mmr(None)

        assert player is not None
        assert player.rating > 0, "Player should have a rating even with None MMR"
        assert player.rd > 0, "Player should have RD"
        assert player.vol > 0, "Player should have volatility"

    def test_rating_update_extreme_ratings(self):
        """Test rating updates with extreme rating values."""
        rating_system = CamaRatingSystem()

        # Create players with extreme ratings
        very_high_player = rating_system.create_player_from_rating(2800.0, 50.0, 0.06)
        very_low_player = rating_system.create_player_from_rating(200.0, 50.0, 0.06)

        # Simulate a match where high-rated player wins
        very_high_player.update_player(
            [very_low_player.rating],
            [very_low_player.rd],
            [1.0],  # High player wins
        )

        # High player's rating should increase (or stay high)
        assert very_high_player.rating > 0, "Rating should remain positive"

        # Low player's rating should decrease (or stay low)
        very_low_player.update_player(
            [very_high_player.rating],
            [very_high_player.rd],
            [0.0],  # Low player loses
        )

        assert very_low_player.rating >= 0, "Rating should remain >= 0"

    def test_rating_update_new_player_vs_experienced(self):
        """Test rating update when new player (high RD) plays experienced player (low RD)."""
        rating_system = CamaRatingSystem()

        # New player: high RD (uncertain)
        new_player = rating_system.create_player_from_rating(1500.0, 350.0, 0.06)

        # Experienced player: low RD (certain)
        experienced_player = rating_system.create_player_from_rating(1500.0, 50.0, 0.06)

        initial_new_rating = new_player.rating
        initial_exp_rating = experienced_player.rating

        # New player wins
        new_player.update_player([experienced_player.rating], [experienced_player.rd], [1.0])

        experienced_player.update_player([new_player.rating], [new_player.rd], [0.0])

        # New player's rating should change more (higher RD = more volatile)
        new_rating_change = abs(new_player.rating - initial_new_rating)
        exp_rating_change = abs(experienced_player.rating - initial_exp_rating)

        # New player should have larger rating change due to higher RD
        assert new_rating_change >= exp_rating_change, (
            "New player (high RD) should have larger rating change than experienced player"
        )

    def test_rapid_rating_changes(self):
        """Test rapid rating changes across multiple matches."""
        rating_system = CamaRatingSystem()

        player1 = rating_system.create_player_from_rating(1500.0, 350.0, 0.06)
        player2 = rating_system.create_player_from_rating(1500.0, 350.0, 0.06)

        initial_rating1 = player1.rating

        # Play 10 matches, player1 wins all
        for _ in range(10):
            player1.update_player([player2.rating], [player2.rd], [1.0])
            player2.update_player([player1.rating], [player1.rd], [0.0])

        # Player1's rating should have increased significantly
        assert player1.rating > initial_rating1, (
            f"Player1's rating should increase after 10 wins, {initial_rating1} -> {player1.rating}"
        )

        # Player2's rating should have decreased
        assert player2.rating < 1500.0, (
            f"Player2's rating should decrease after 10 losses, got {player2.rating}"
        )

    def test_rating_uncertainty_percentage(self):
        """Test rating uncertainty percentage calculation."""
        rating_system = CamaRatingSystem()

        # Very certain player (low RD)
        low_rd = 30.0
        uncertainty_low = rating_system.get_rating_uncertainty_percentage(low_rd)
        assert 0 <= uncertainty_low <= 100, "Uncertainty should be 0-100%"
        assert uncertainty_low < 50, "Low RD should give low uncertainty"

        # Very uncertain player (high RD)
        high_rd = 350.0
        uncertainty_high = rating_system.get_rating_uncertainty_percentage(high_rd)
        assert 0 <= uncertainty_high <= 100, "Uncertainty should be 0-100%"
        assert uncertainty_high > 50, "High RD should give high uncertainty"

        # Uncertainty should increase with RD
        assert uncertainty_high > uncertainty_low, (
            "Higher RD should give higher uncertainty percentage"
        )

    def test_team_rating_update(self):
        """Test rating updates for team matches."""
        rating_system = CamaRatingSystem()

        # Create two teams of 5 players each
        team1_players = [
            (rating_system.create_player_from_rating(1500.0 + i * 10, 350.0, 0.06), 1000 + i)
            for i in range(5)
        ]
        team2_players = [
            (rating_system.create_player_from_rating(1500.0 + i * 10, 350.0, 0.06), 2000 + i)
            for i in range(5)
        ]

        # Get initial ratings
        initial_ratings_team1 = [p.rating for p, _ in team1_players]
        initial_ratings_team2 = [p.rating for p, _ in team2_players]

        # Update ratings (team 1 wins)
        team1_updated, team2_updated = rating_system.update_ratings_after_match(
            team1_players, team2_players, winning_team=1
        )

        # Verify all players got updated
        assert len(team1_updated) == 5, "Team 1 should have 5 updated ratings"
        assert len(team2_updated) == 5, "Team 2 should have 5 updated ratings"

        # Team 1 players share the same rating delta (within small tolerance)
        winner_deltas = [
            rating - initial
            for (rating, _, _, _), initial in zip(team1_updated, initial_ratings_team1)
        ]
        assert all(delta > 0 for delta in winner_deltas)
        assert max(winner_deltas) - min(winner_deltas) < 1e-6, (
            "Winner deltas should match across team"
        )
        assert winner_deltas[0] < 400, "Single team win should not create extreme jumps"

        # Team 2 players share the same rating delta (within small tolerance)
        loser_deltas = [
            rating - initial
            for (rating, _, _, _), initial in zip(team2_updated, initial_ratings_team2)
        ]
        assert all(delta < 0 for delta in loser_deltas)
        assert max(loser_deltas) - min(loser_deltas) < 1e-6, "Loser deltas should match across team"
        assert loser_deltas[0] > -400, "Single team loss should not create extreme drops"

        # RD should remain positive
        for _rating, rd, _vol, _pid in team1_updated + team2_updated:
            assert rd > 0

    def test_single_even_team_match_has_moderate_change(self):
        """Even teams should not yield extreme single-game jumps."""
        rating_system = CamaRatingSystem()
        team1_players = [
            (rating_system.create_player_from_rating(1500.0, 350.0, 0.06), 1) for _ in range(5)
        ]
        team2_players = [
            (rating_system.create_player_from_rating(1500.0, 350.0, 0.06), 6) for _ in range(5)
        ]

        team1_updated, team2_updated = rating_system.update_ratings_after_match(
            team1_players, team2_players, winning_team=1
        )

        # Winners should go up, losers down, but bounded (<300 from 1500 baseline)
        for rating, _rd, _vol, _pid in team1_updated:
            assert rating > 1500
            assert rating < 1800, f"Winner jump too large for single even match: {rating}"
        for rating, _rd, _vol, _pid in team2_updated:
            assert rating < 1500
            assert rating > 1200, f"Loser drop too large for single even match: {rating}"

    def test_weak_team_beats_strong_team_has_larger_gain(self):
        """Weak team win should yield bigger delta than strong team win."""
        rating_system = CamaRatingSystem()
        # Strong team around 1800, weak team around 1200
        strong_team = [
            (rating_system.create_player_from_rating(1800.0, 100.0, 0.06), i) for i in range(5)
        ]
        weak_team = [
            (rating_system.create_player_from_rating(1200.0, 200.0, 0.06), i + 10) for i in range(5)
        ]

        # Case 1: weak team wins
        weak_win_updates, strong_loss_updates = rating_system.update_ratings_after_match(
            weak_team, strong_team, winning_team=1
        )
        weak_win_delta = weak_win_updates[0][0] - 1200.0
        strong_loss_delta = strong_loss_updates[0][0] - 1800.0

        # Reset players for opposite outcome
        strong_team = [
            (rating_system.create_player_from_rating(1800.0, 100.0, 0.06), i) for i in range(5)
        ]
        weak_team = [
            (rating_system.create_player_from_rating(1200.0, 200.0, 0.06), i + 10) for i in range(5)
        ]

        # Case 2: strong team wins
        strong_win_updates, weak_loss_updates = rating_system.update_ratings_after_match(
            strong_team, weak_team, winning_team=1
        )
        strong_win_delta = strong_win_updates[0][0] - 1800.0
        weak_loss_delta = weak_loss_updates[0][0] - 1200.0

        assert weak_win_delta > strong_win_delta, "Upset should award bigger winner delta"
        assert abs(strong_loss_delta) < abs(weak_loss_delta), (
            "Stronger team loss should hurt less than weak team loss"
        )


class TestRatingSystemBoundaryConditions:
    """Test boundary conditions in rating system."""

    def test_zero_rating(self):
        """Test handling of zero rating."""
        rating_system = CamaRatingSystem()

        # Create player with zero rating
        player = rating_system.create_player_from_rating(0.0, 350.0, 0.06)

        assert player.rating == 0.0, "Zero rating should be preserved"

        # Update rating
        opponent = rating_system.create_player_from_rating(1500.0, 350.0, 0.06)
        player.update_player(
            [opponent.rating],
            [opponent.rd],
            [1.0],  # Win
        )

        # Rating should increase from 0
        assert player.rating > 0, "Rating should increase from 0 after win"

    def test_maximum_rating(self):
        """Test handling of maximum rating."""
        rating_system = CamaRatingSystem()

        # Create player with maximum rating
        max_rating = 3000.0
        player = rating_system.create_player_from_rating(max_rating, 50.0, 0.06)

        # Update rating (lose to lower-rated player)
        opponent = rating_system.create_player_from_rating(1500.0, 350.0, 0.06)
        player.update_player(
            [opponent.rating],
            [opponent.rd],
            [0.0],  # Lose
        )

        # Rating should decrease from maximum
        assert player.rating < max_rating, "Rating should decrease from max after loss"
        assert player.rating > 0, "Rating should remain positive"

    def test_very_small_rd(self):
        """Test handling of very small RD (very certain player)."""
        rating_system = CamaRatingSystem()

        # Create player with very small RD
        very_small_rd = 10.0
        player = rating_system.create_player_from_rating(1500.0, very_small_rd, 0.06)

        # Update rating
        opponent = rating_system.create_player_from_rating(1500.0, 350.0, 0.06)
        initial_rating = player.rating

        player.update_player(
            [opponent.rating],
            [opponent.rd],
            [1.0],  # Win
        )

        # Rating should change, but not dramatically (low RD = stable)
        rating_change = abs(player.rating - initial_rating)
        assert rating_change > 0, "Rating should change"
        assert rating_change < 100, "Rating change should be small with very low RD"


class TestRatingSystemCalibrationAndDecay:
    """Tests for calibration status and RD decay behavior."""

    def test_is_calibrated_threshold(self):
        assert CamaRatingSystem.is_calibrated(CALIBRATION_RD_THRESHOLD)
        assert not CamaRatingSystem.is_calibrated(CALIBRATION_RD_THRESHOLD + 0.1)

    def test_rd_decay_grace_and_floor_weeks(self):
        # Grace period: no decay when below grace period (14 days default)
        rd_start = 150.0
        rd_after_13_days = CamaRatingSystem.apply_rd_decay(rd_start, RD_DECAY_GRACE_PERIOD_WEEKS * 7 - 1)
        assert rd_after_13_days == rd_start, "No decay should apply during grace period"

        # After grace: use floor weeks; 21 days => 3 weeks
        days = RD_DECAY_GRACE_PERIOD_WEEKS * 7 + 7  # 21 days if grace is 14
        rd_after_21_days = CamaRatingSystem.apply_rd_decay(rd_start, days)
        expected_weeks = days // 7
        expected_rd = min(350.0, (rd_start * rd_start + (RD_DECAY_CONSTANT * RD_DECAY_CONSTANT) * expected_weeks) ** 0.5)
        assert rd_after_21_days == expected_rd
        assert rd_after_21_days > rd_start, "RD should increase after inactivity past grace period"

    def test_rd_decay_cap_and_already_max(self):
        # Already at cap stays at cap
        assert CamaRatingSystem.apply_rd_decay(350.0, 100) == 350.0

        # Large gap should cap at 350
        rd_start = 340.0
        rd_after_long_break = CamaRatingSystem.apply_rd_decay(rd_start, 70)  # 10 weeks
        assert rd_after_long_break == 350.0, "RD decay should not exceed 350"


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
