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
        """Test rating updates for team matches with hybrid deltas."""
        rating_system = CamaRatingSystem()

        # Create two teams of 5 calibrated players each (RD = 80, below threshold 100)
        team1_players = [
            (rating_system.create_player_from_rating(1500.0 + i * 10, 80.0, 0.06), 1000 + i)
            for i in range(5)
        ]
        team2_players = [
            (rating_system.create_player_from_rating(1500.0 + i * 10, 80.0, 0.06), 2000 + i)
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

        # With hybrid deltas, calibrated players (RD <= threshold) get uniform team delta
        winner_deltas = [
            rating - initial
            for (rating, _, _, _), initial in zip(team1_updated, initial_ratings_team1)
        ]
        assert all(delta > 0 for delta in winner_deltas), "Winners should gain rating"
        assert all(delta < 100 for delta in winner_deltas), (
            "Calibrated players should have moderate gains"
        )
        # All calibrated players should get the SAME delta
        assert max(winner_deltas) - min(winner_deltas) < 0.01, (
            "Calibrated players should get identical team delta"
        )

        loser_deltas = [
            rating - initial
            for (rating, _, _, _), initial in zip(team2_updated, initial_ratings_team2)
        ]
        assert all(delta < 0 for delta in loser_deltas), "Losers should lose rating"
        assert all(delta > -100 for delta in loser_deltas), (
            "Calibrated players should have moderate losses"
        )
        # All calibrated losers should get the SAME delta
        assert max(loser_deltas) - min(loser_deltas) < 0.01, (
            "Calibrated losers should get identical team delta"
        )

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

    def test_individual_deltas_depend_on_rd(self):
        """With hybrid deltas, calibrating players get individual deltas based on RD."""
        rating_system = CamaRatingSystem()

        # Create teams with different RDs to test individual delta behavior
        # High RD team (250, calibrating) vs Low RD team (80, calibrated)
        high_rd_team = [
            (rating_system.create_player_from_rating(1500.0, 250.0, 0.06), i) for i in range(5)
        ]
        low_rd_team = [
            (rating_system.create_player_from_rating(1500.0, 80.0, 0.06), i + 10) for i in range(5)
        ]

        # High RD team wins
        high_rd_win, low_rd_loss = rating_system.update_ratings_after_match(
            high_rd_team, low_rd_team, winning_team=1
        )
        high_rd_win_delta = high_rd_win[0][0] - 1500.0
        low_rd_loss_delta = low_rd_loss[0][0] - 1500.0

        # High RD (calibrating) players should have larger swings than low RD (calibrated) players
        assert abs(high_rd_win_delta) > abs(low_rd_loss_delta), (
            "Calibrating winner should gain more than calibrated loser loses"
        )

        # Reset for opposite outcome
        high_rd_team = [
            (rating_system.create_player_from_rating(1500.0, 250.0, 0.06), i) for i in range(5)
        ]
        low_rd_team = [
            (rating_system.create_player_from_rating(1500.0, 80.0, 0.06), i + 10) for i in range(5)
        ]

        # Low RD team wins
        low_rd_win, high_rd_loss = rating_system.update_ratings_after_match(
            low_rd_team, high_rd_team, winning_team=1
        )
        low_rd_win_delta = low_rd_win[0][0] - 1500.0
        high_rd_loss_delta = high_rd_loss[0][0] - 1500.0

        # High RD (calibrating) players should still have larger swings
        assert abs(high_rd_loss_delta) > abs(low_rd_win_delta), (
            "Calibrating loser should lose more than calibrated winner gains"
        )

    def test_upset_rewards_underdog(self):
        """Underdog winning an upset should be rewarded appropriately."""
        rating_system = CamaRatingSystem()

        # Use same RD (calibrated) to isolate the upset effect
        favorite_team = [
            (rating_system.create_player_from_rating(1800.0, 80.0, 0.06), i) for i in range(5)
        ]
        underdog_team = [
            (rating_system.create_player_from_rating(1200.0, 80.0, 0.06), i + 10) for i in range(5)
        ]

        # Underdog wins (upset)
        underdog_win, favorite_loss = rating_system.update_ratings_after_match(
            underdog_team, favorite_team, winning_team=1
        )
        underdog_win_delta = underdog_win[0][0] - 1200.0
        favorite_loss_delta = favorite_loss[0][0] - 1800.0

        # Reset for expected outcome
        favorite_team = [
            (rating_system.create_player_from_rating(1800.0, 80.0, 0.06), i) for i in range(5)
        ]
        underdog_team = [
            (rating_system.create_player_from_rating(1200.0, 80.0, 0.06), i + 10) for i in range(5)
        ]

        # Favorite wins (expected)
        favorite_win, underdog_loss = rating_system.update_ratings_after_match(
            favorite_team, underdog_team, winning_team=1
        )
        favorite_win_delta = favorite_win[0][0] - 1800.0
        underdog_loss_delta = underdog_loss[0][0] - 1200.0

        # Upset win should be rewarded more than expected win
        assert underdog_win_delta > favorite_win_delta, (
            "Underdog upset win should gain more than favorite expected win"
        )
        # Upset loss should hurt more than expected loss
        assert abs(favorite_loss_delta) > abs(underdog_loss_delta), (
            "Favorite upset loss should hurt more than underdog expected loss"
        )

    def test_hybrid_delta_guardrails_winner(self):
        """Test that calibrating winners get at least the team delta."""
        rating_system = CamaRatingSystem()

        # Mixed team: calibrated (RD=80) + calibrating (RD=150)
        # Use same rating so we can isolate the RD effect
        mixed_team = [
            (rating_system.create_player_from_rating(1500.0, 80.0, 0.06), 1),  # calibrated
            (rating_system.create_player_from_rating(1500.0, 80.0, 0.06), 2),  # calibrated
            (rating_system.create_player_from_rating(1500.0, 80.0, 0.06), 3),  # calibrated
            (rating_system.create_player_from_rating(1500.0, 150.0, 0.06), 4),  # calibrating
            (rating_system.create_player_from_rating(1500.0, 150.0, 0.06), 5),  # calibrating
        ]
        opponent_team = [
            (rating_system.create_player_from_rating(1500.0, 80.0, 0.06), i + 10) for i in range(5)
        ]

        # Mixed team wins
        mixed_updated, _ = rating_system.update_ratings_after_match(
            mixed_team, opponent_team, winning_team=1
        )

        # All calibrated players should have identical deltas (team delta)
        calibrated_deltas = [mixed_updated[i][0] - 1500.0 for i in range(3)]
        team_delta = calibrated_deltas[0]
        assert max(calibrated_deltas) - min(calibrated_deltas) < 0.01, (
            "Calibrated players should have identical team delta"
        )
        assert team_delta > 0, "Team delta should be positive for win"

        # Calibrating players should have delta >= team delta (guardrail: max)
        calibrating_deltas = [mixed_updated[i][0] - 1500.0 for i in range(3, 5)]
        for delta in calibrating_deltas:
            assert delta >= team_delta - 0.01, (
                f"Calibrating winner should get at least team delta: {delta} < {team_delta}"
            )

    def test_hybrid_delta_guardrails_loser(self):
        """Test that calibrating losers get at least the team delta (loss)."""
        rating_system = CamaRatingSystem()

        # Mixed team: calibrated + calibrating
        mixed_team = [
            (rating_system.create_player_from_rating(1500.0, 80.0, 0.06), 1),  # calibrated
            (rating_system.create_player_from_rating(1500.0, 80.0, 0.06), 2),  # calibrated
            (rating_system.create_player_from_rating(1500.0, 80.0, 0.06), 3),  # calibrated
            (rating_system.create_player_from_rating(1500.0, 150.0, 0.06), 4),  # calibrating
            (rating_system.create_player_from_rating(1500.0, 150.0, 0.06), 5),  # calibrating
        ]
        opponent_team = [
            (rating_system.create_player_from_rating(1500.0, 80.0, 0.06), i + 10) for i in range(5)
        ]

        # Mixed team loses
        mixed_updated, _ = rating_system.update_ratings_after_match(
            mixed_team, opponent_team, winning_team=2
        )

        # All calibrated players should have identical deltas
        calibrated_deltas = [mixed_updated[i][0] - 1500.0 for i in range(3)]
        team_delta = calibrated_deltas[0]
        assert max(calibrated_deltas) - min(calibrated_deltas) < 0.01
        assert team_delta < 0, "Team delta should be negative for loss"

        # Calibrating players should have delta <= team delta (guardrail: min for loss)
        calibrating_deltas = [mixed_updated[i][0] - 1500.0 for i in range(3, 5)]
        for delta in calibrating_deltas:
            assert delta <= team_delta + 0.01, (
                f"Calibrating loser should get at least team loss: {delta} > {team_delta}"
            )

    def test_mixed_team_rd_weighted_behavior(self):
        """Test complete mixed team scenario with various RDs using V2 RD²-weighted system."""
        rating_system = CamaRatingSystem()

        # Simulates a real match scenario with varied RDs
        # V2: All players get RD²-weighted deltas (no calibrated/calibrating distinction)
        team1 = [
            (rating_system.create_player_from_rating(1600.0, 80.0, 0.06), 1),   # low RD
            (rating_system.create_player_from_rating(1400.0, 250.0, 0.06), 2),  # high RD
            (rating_system.create_player_from_rating(1500.0, 120.0, 0.06), 3),  # medium RD
            (rating_system.create_player_from_rating(1550.0, 90.0, 0.06), 4),   # low RD
            (rating_system.create_player_from_rating(1450.0, 180.0, 0.06), 5),  # medium-high RD
        ]
        team2 = [
            (rating_system.create_player_from_rating(1500.0, 80.0, 0.06), i + 10) for i in range(5)
        ]

        # Team 1 wins
        t1_updated, t2_updated = rating_system.update_ratings_after_match(
            team1, team2, winning_team=1
        )

        # Extract deltas and RDs
        t1_rds = [80.0, 250.0, 120.0, 90.0, 180.0]
        t1_deltas = [
            t1_updated[i][0] - [1600.0, 1400.0, 1500.0, 1550.0, 1450.0][i]
            for i in range(5)
        ]

        # V2: Higher RD players should get larger deltas (RD² weighting)
        # Player 1 (RD 250) should have largest delta
        # Players 0, 3 (RD 80, 90) should have smallest deltas
        high_rd_delta = t1_deltas[1]  # RD 250
        low_rd_deltas = [t1_deltas[0], t1_deltas[3]]  # RD 80, 90
        assert high_rd_delta > max(low_rd_deltas), (
            f"Higher RD should get larger delta: RD250={high_rd_delta}, RD80/90={low_rd_deltas}"
        )

        # All winners should gain rating
        assert all(d > 0 for d in t1_deltas), "All winners should gain rating"

        # Team 2 (all same RD) should all have same delta
        t2_deltas = [t2_updated[i][0] - 1500.0 for i in range(5)]
        assert max(t2_deltas) - min(t2_deltas) < 0.01, (
            "Same-RD teammates should have identical deltas"
        )
        assert all(d < 0 for d in t2_deltas), "All losers should lose rating"

    def test_all_calibrating_team_uses_threshold_rd(self):
        """Test that a team with no calibrated players uses threshold RD for team delta."""
        rating_system = CamaRatingSystem()

        # Both teams are all calibrating (no one with RD <= 100)
        team1 = [
            (rating_system.create_player_from_rating(1500.0, 150.0, 0.06), i) for i in range(5)
        ]
        team2 = [
            (rating_system.create_player_from_rating(1500.0, 150.0, 0.06), i + 10) for i in range(5)
        ]

        # Team 1 wins
        t1_updated, t2_updated = rating_system.update_ratings_after_match(
            team1, team2, winning_team=1
        )

        # All players should gain/lose rating appropriately
        for rating, _, _, _ in t1_updated:
            assert rating > 1500.0, "Winner should gain rating"
        for rating, _, _, _ in t2_updated:
            assert rating < 1500.0, "Loser should lose rating"

        # Since all have same RD and rating, their individual deltas should be similar
        t1_deltas = [r[0] - 1500.0 for r in t1_updated]
        assert max(t1_deltas) - min(t1_deltas) < 1.0, (
            "Same-RD calibrating players should have very similar deltas"
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
