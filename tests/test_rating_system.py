"""
Tests for rating system edge cases and error handling.
"""


import pytest
from glicko2 import Player

from config import (
    CALIBRATION_RD_THRESHOLD,
    MAX_RATING_SWING_PER_GAME,
    RD_DECAY_CONSTANT,
    RD_DECAY_GRACE_PERIOD_WEEKS,
)
from rating_system import CamaRatingSystem


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
        """Test rating updates for team matches with individual Glicko-2 updates."""
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

        # V3 uses individual Glicko-2 updates - each player computes their own update
        # Players at slightly different ratings will get slightly different deltas
        winner_deltas = [
            rating - initial
            for (rating, _, _, _), initial in zip(team1_updated, initial_ratings_team1)
        ]
        assert all(delta > 0 for delta in winner_deltas), "Winners should gain rating"
        assert all(delta < 100 for delta in winner_deltas), (
            "Calibrated players should have moderate gains"
        )
        # All calibrated players should get similar (but not necessarily identical) deltas
        assert max(winner_deltas) - min(winner_deltas) < 5, (
            "Calibrated players with similar ratings should get similar deltas"
        )

        loser_deltas = [
            rating - initial
            for (rating, _, _, _), initial in zip(team2_updated, initial_ratings_team2)
        ]
        assert all(delta < 0 for delta in loser_deltas), "Losers should lose rating"
        assert all(delta > -100 for delta in loser_deltas), (
            "Calibrated players should have moderate losses"
        )
        # All calibrated losers should get similar deltas
        assert max(loser_deltas) - min(loser_deltas) < 5, (
            "Calibrated losers with similar ratings should get similar deltas"
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


class TestV3IndividualGlickoWithCap:
    """Tests for the V3 pure individual Glicko approach with cap."""

    @pytest.fixture
    def rating_system(self):
        return CamaRatingSystem()

    @pytest.fixture
    def high_rd_on_strong_team(self):
        """
        The problem case: low-rated high-RD player on a strong team.
        Team 1 avg: ~1143, Team 2 avg: ~1256
        """
        team1 = [
            (Player(rating=413, rd=334, vol=0.06), "high_rd_low_rating"),
            (Player(rating=1400, rd=80, vol=0.06), "calibrated_1"),
            (Player(rating=1350, rd=75, vol=0.06), "calibrated_2"),
            (Player(rating=1300, rd=85, vol=0.06), "calibrated_3"),
            (Player(rating=1250, rd=90, vol=0.06), "calibrated_4"),
        ]
        team2 = [
            (Player(rating=1350, rd=80, vol=0.06), "enemy_1"),
            (Player(rating=1300, rd=85, vol=0.06), "enemy_2"),
            (Player(rating=1250, rd=75, vol=0.06), "enemy_3"),
            (Player(rating=1200, rd=90, vol=0.06), "enemy_4"),
            (Player(rating=1180, rd=80, vol=0.06), "enemy_5"),
        ]
        return team1, team2

    @pytest.fixture
    def balanced_calibrated_teams(self):
        """Two balanced teams of all calibrated players."""
        team1 = [
            (Player(rating=1250, rd=75, vol=0.06), "player_1"),
            (Player(rating=1220, rd=80, vol=0.06), "player_2"),
            (Player(rating=1200, rd=85, vol=0.06), "player_3"),
            (Player(rating=1180, rd=70, vol=0.06), "player_4"),
            (Player(rating=1150, rd=90, vol=0.06), "player_5"),
        ]
        team2 = [
            (Player(rating=1240, rd=80, vol=0.06), "enemy_1"),
            (Player(rating=1210, rd=75, vol=0.06), "enemy_2"),
            (Player(rating=1190, rd=85, vol=0.06), "enemy_3"),
            (Player(rating=1170, rd=70, vol=0.06), "enemy_4"),
            (Player(rating=1160, rd=90, vol=0.06), "enemy_5"),
        ]
        return team1, team2

    @pytest.fixture
    def two_new_players_team(self):
        """Team with two brand-new high-RD players."""
        team1 = [
            (Player(rating=500, rd=350, vol=0.06), "new_player_1"),
            (Player(rating=600, rd=320, vol=0.06), "new_player_2"),
            (Player(rating=1400, rd=75, vol=0.06), "calibrated_1"),
            (Player(rating=1350, rd=80, vol=0.06), "calibrated_2"),
            (Player(rating=1300, rd=85, vol=0.06), "calibrated_3"),
        ]
        team2 = [
            (Player(rating=1100, rd=80, vol=0.06), "enemy_1"),
            (Player(rating=1080, rd=85, vol=0.06), "enemy_2"),
            (Player(rating=1050, rd=75, vol=0.06), "enemy_3"),
            (Player(rating=1000, rd=90, vol=0.06), "enemy_4"),
            (Player(rating=980, rd=80, vol=0.06), "enemy_5"),
        ]
        return team1, team2

    # =========================================================================
    # Test 1: High-RD player swings are bounded by cap
    # =========================================================================

    def test_high_rd_player_win_bounded_by_cap(self, rating_system, high_rd_on_strong_team):
        """
        High-RD player should gain significantly on win, but not exceed cap.
        V3 uses individual Glicko updates, capped at MAX_RATING_SWING_PER_GAME.
        """
        team1, team2 = high_rd_on_strong_team
        original_rating = team1[0][0].rating  # high_rd_low_rating

        t1_updated, t2_updated = rating_system.update_ratings_after_match(
            team1, team2, winning_team=1
        )

        high_rd_new_rating = t1_updated[0][0]
        delta = high_rd_new_rating - original_rating

        # Should be significant and bounded by cap
        assert delta > 100, f"High-RD winner should gain significantly, got {delta}"
        assert delta <= MAX_RATING_SWING_PER_GAME, f"High-RD winner gain should be capped at {MAX_RATING_SWING_PER_GAME}, got {delta}"

    def test_high_rd_player_loss_bounded_by_cap(self, rating_system, high_rd_on_strong_team):
        """
        High-RD player loss should be bounded by cap.

        Note: In Glicko-2, when a low-rated player loses to a high-rated team,
        the loss is small because it's "expected". This is correct behavior.
        The key assertion is that the cap is respected.
        """
        team1, team2 = high_rd_on_strong_team
        original_rating = team1[0][0].rating

        t1_updated, t2_updated = rating_system.update_ratings_after_match(
            team1, team2, winning_team=2
        )

        high_rd_new_rating = t1_updated[0][0]
        delta = high_rd_new_rating - original_rating

        # Loss should be negative (they lost)
        assert delta < 0, f"High-RD loser should have negative delta, got {delta}"
        # Bounded by cap
        assert delta >= -MAX_RATING_SWING_PER_GAME, f"High-RD loser loss should be capped at -{MAX_RATING_SWING_PER_GAME}, got {delta}"

    def test_win_and_loss_both_bounded(self, rating_system, high_rd_on_strong_team):
        """
        Both win and loss deltas should be bounded by the cap.

        Note: Glicko-2 naturally has asymmetric win/loss when there's a rating
        difference - a low-rated player loses little when losing to a high-rated
        team (expected) but gains a lot when winning (upset). This is correct.
        V3 ensures both are capped.
        """
        team1, team2 = high_rd_on_strong_team
        original_rating = team1[0][0].rating

        # Test win
        team1_win = [(Player(rating=p.rating, rd=p.rd, vol=p.vol), pid) for p, pid in high_rd_on_strong_team[0]]
        team2_win = [(Player(rating=p.rating, rd=p.rd, vol=p.vol), pid) for p, pid in high_rd_on_strong_team[1]]
        t1_win, _ = rating_system.update_ratings_after_match(team1_win, team2_win, winning_team=1)
        win_delta = t1_win[0][0] - original_rating

        # Test loss
        team1_loss = [(Player(rating=p.rating, rd=p.rd, vol=p.vol), pid) for p, pid in high_rd_on_strong_team[0]]
        team2_loss = [(Player(rating=p.rating, rd=p.rd, vol=p.vol), pid) for p, pid in high_rd_on_strong_team[1]]
        t1_loss, _ = rating_system.update_ratings_after_match(team1_loss, team2_loss, winning_team=2)
        loss_delta = t1_loss[0][0] - original_rating

        # Both should be bounded by cap
        assert win_delta <= MAX_RATING_SWING_PER_GAME, f"Win delta should be capped, got {win_delta}"
        assert loss_delta >= -MAX_RATING_SWING_PER_GAME, f"Loss delta should be capped, got {loss_delta}"
        # Win should be positive, loss should be negative
        assert win_delta > 0, f"Win delta should be positive, got {win_delta}"
        assert loss_delta < 0, f"Loss delta should be negative, got {loss_delta}"

    # =========================================================================
    # Test 2: Calibrated players get stable deltas
    # =========================================================================

    def test_calibrated_players_small_deltas(self, rating_system, balanced_calibrated_teams):
        """
        In a balanced match of calibrated players, everyone should get small deltas.
        Expected: ~10-40 per player (standard Glicko-2 for low RD).
        """
        team1, team2 = balanced_calibrated_teams

        t1_updated, _ = rating_system.update_ratings_after_match(
            team1, team2, winning_team=1
        )

        for i, (orig_player, _) in enumerate(team1):
            new_rating = t1_updated[i][0]
            delta = new_rating - orig_player.rating

            # Calibrated players should have small deltas (5-50 range)
            assert 5 < delta < 60, f"Calibrated winner delta should be moderate, got {delta}"

    def test_calibrated_players_independent_of_teammates(self, rating_system):
        """
        V3 key behavior: calibrated player's swing should NOT increase
        just because they have a high-RD teammate.
        """
        # Team with one high-RD player
        team1_with_high_rd = [
            (Player(rating=500, rd=350, vol=0.06), "new_player"),
            (Player(rating=1200, rd=80, vol=0.06), "calibrated_test"),
            (Player(rating=1200, rd=75, vol=0.06), "calibrated_2"),
            (Player(rating=1200, rd=85, vol=0.06), "calibrated_3"),
            (Player(rating=1200, rd=90, vol=0.06), "calibrated_4"),
        ]
        # Team with all calibrated players
        team1_all_calibrated = [
            (Player(rating=1200, rd=80, vol=0.06), "calibrated_replaced"),
            (Player(rating=1200, rd=80, vol=0.06), "calibrated_test"),
            (Player(rating=1200, rd=75, vol=0.06), "calibrated_2"),
            (Player(rating=1200, rd=85, vol=0.06), "calibrated_3"),
            (Player(rating=1200, rd=90, vol=0.06), "calibrated_4"),
        ]
        team2 = [
            (Player(rating=1200, rd=80, vol=0.06), f"enemy_{i}")
            for i in range(5)
        ]

        # Test with high-RD teammate
        t1_high_rd, _ = rating_system.update_ratings_after_match(
            team1_with_high_rd, team2, winning_team=1
        )
        calibrated_delta_with_high_rd = t1_high_rd[1][0] - 1200  # calibrated_test

        # Test with all calibrated teammates
        team2_copy = [(Player(rating=p.rating, rd=p.rd, vol=p.vol), pid) for p, pid in team2]
        t1_all_cal, _ = rating_system.update_ratings_after_match(
            team1_all_calibrated, team2_copy, winning_team=1
        )
        calibrated_delta_all_cal = t1_all_cal[1][0] - 1200  # calibrated_test

        # In V3, these should be similar (within 50% or 20 points)
        # because each player computes their own update independently
        diff = abs(calibrated_delta_with_high_rd - calibrated_delta_all_cal)
        assert diff < 20 or diff < max(abs(calibrated_delta_with_high_rd), abs(calibrated_delta_all_cal)) * 0.5, \
            f"Calibrated player delta should be similar regardless of teammates: {calibrated_delta_with_high_rd} vs {calibrated_delta_all_cal}"

    # =========================================================================
    # Test 3: High-RD gets larger deltas (natural Glicko-2 behavior)
    # =========================================================================

    def test_higher_rd_gets_larger_delta(self, rating_system, high_rd_on_strong_team):
        """
        Players with higher RD should naturally get larger deltas in Glicko-2.
        """
        team1, team2 = high_rd_on_strong_team

        t1_updated, _ = rating_system.update_ratings_after_match(
            team1, team2, winning_team=1
        )

        # Get absolute deltas (use abs since we're comparing magnitude)
        high_rd_player_delta = abs(t1_updated[0][0] - team1[0][0].rating)
        calibrated_player_delta = abs(t1_updated[1][0] - team1[1][0].rating)

        # Higher RD should naturally produce larger delta
        assert high_rd_player_delta > calibrated_player_delta, \
            f"Higher RD should get larger delta: {high_rd_player_delta} vs {calibrated_player_delta}"

    # =========================================================================
    # Test 4: Cap is applied correctly
    # =========================================================================

    def test_cap_applied_for_extreme_underdog_win(self, rating_system):
        """
        When a very low-rated high-RD player on an underdog team wins,
        the cap should limit their gain.
        """
        # Extreme underdog scenario
        team1 = [
            (Player(rating=200, rd=350, vol=0.06), "very_low_new"),
            (Player(rating=400, rd=350, vol=0.06), "low_new"),
            (Player(rating=500, rd=350, vol=0.06), "low_2"),
            (Player(rating=600, rd=350, vol=0.06), "low_3"),
            (Player(rating=700, rd=350, vol=0.06), "low_4"),
        ]
        team2 = [
            (Player(rating=2500, rd=50, vol=0.06), "high_1"),
            (Player(rating=2500, rd=50, vol=0.06), "high_2"),
            (Player(rating=2500, rd=50, vol=0.06), "high_3"),
            (Player(rating=2500, rd=50, vol=0.06), "high_4"),
            (Player(rating=2500, rd=50, vol=0.06), "high_5"),
        ]

        t1_updated, _ = rating_system.update_ratings_after_match(
            team1, team2, winning_team=1  # Upset!
        )

        # All winners should be capped
        for i, (orig_player, _) in enumerate(team1):
            delta = t1_updated[i][0] - orig_player.rating
            assert delta <= MAX_RATING_SWING_PER_GAME, \
                f"Player {i} delta should be capped: got {delta}"

    def test_cap_applied_for_extreme_favorite_loss(self, rating_system):
        """
        When a very high-rated calibrated team loses to underdogs,
        losses should still be reasonable.
        """
        team1 = [
            (Player(rating=2500, rd=80, vol=0.06), "high_1"),
            (Player(rating=2500, rd=80, vol=0.06), "high_2"),
            (Player(rating=2500, rd=80, vol=0.06), "high_3"),
            (Player(rating=2500, rd=80, vol=0.06), "high_4"),
            (Player(rating=2500, rd=80, vol=0.06), "high_5"),
        ]
        team2 = [
            (Player(rating=500, rd=350, vol=0.06), "low_1"),
            (Player(rating=500, rd=350, vol=0.06), "low_2"),
            (Player(rating=500, rd=350, vol=0.06), "low_3"),
            (Player(rating=500, rd=350, vol=0.06), "low_4"),
            (Player(rating=500, rd=350, vol=0.06), "low_5"),
        ]

        t1_updated, _ = rating_system.update_ratings_after_match(
            team1, team2, winning_team=2  # Upset!
        )

        # All losers should have bounded loss
        for i, (orig_player, _) in enumerate(team1):
            delta = t1_updated[i][0] - orig_player.rating
            assert delta >= -MAX_RATING_SWING_PER_GAME, \
                f"Player {i} loss should be capped: got {delta}"

    # =========================================================================
    # Test 5: RD updates are monotonic (never increase)
    # =========================================================================

    def test_rd_never_increases_after_match(self, rating_system, high_rd_on_strong_team):
        """
        No player's RD should increase after playing a match.
        """
        team1, team2 = high_rd_on_strong_team

        t1_updated, t2_updated = rating_system.update_ratings_after_match(
            team1, team2, winning_team=1
        )

        for i, (orig_player, _) in enumerate(team1):
            new_rd = t1_updated[i][1]
            assert new_rd <= orig_player.rd, \
                f"Team1 player {i} RD increased: {orig_player.rd} -> {new_rd}"

        for i, (orig_player, _) in enumerate(team2):
            new_rd = t2_updated[i][1]
            assert new_rd <= orig_player.rd, \
                f"Team2 player {i} RD increased: {orig_player.rd} -> {new_rd}"

    # =========================================================================
    # Test 6: Edge cases
    # =========================================================================

    def test_rating_never_negative(self, rating_system):
        """
        Rating should never go below 0 even for very low-rated players losing.
        """
        team1 = [
            (Player(rating=50, rd=350, vol=0.06), "very_low"),
            (Player(rating=100, rd=300, vol=0.06), "low_1"),
            (Player(rating=150, rd=250, vol=0.06), "low_2"),
            (Player(rating=200, rd=200, vol=0.06), "low_3"),
            (Player(rating=250, rd=150, vol=0.06), "low_4"),
        ]
        team2 = [
            (Player(rating=2000, rd=80, vol=0.06), "high_1"),
            (Player(rating=2000, rd=80, vol=0.06), "high_2"),
            (Player(rating=2000, rd=80, vol=0.06), "high_3"),
            (Player(rating=2000, rd=80, vol=0.06), "high_4"),
            (Player(rating=2000, rd=80, vol=0.06), "high_5"),
        ]

        t1_updated, _ = rating_system.update_ratings_after_match(
            team1, team2, winning_team=2
        )

        for i in range(5):
            new_rating = t1_updated[i][0]
            assert new_rating >= 0, f"Rating went negative: {new_rating}"

    def test_identical_rd_similar_distribution(self, rating_system):
        """
        When all players have identical RD and rating, deltas should be similar.
        """
        team1 = [
            (Player(rating=1200, rd=100, vol=0.06), f"player_{i}")
            for i in range(5)
        ]
        team2 = [
            (Player(rating=1200, rd=100, vol=0.06), f"enemy_{i}")
            for i in range(5)
        ]

        t1_updated, _ = rating_system.update_ratings_after_match(
            team1, team2, winning_team=1
        )

        deltas = [t1_updated[i][0] - 1200 for i in range(5)]

        # All deltas should be identical (within floating point tolerance)
        assert all(abs(d - deltas[0]) < 0.01 for d in deltas), \
            f"Equal RD and rating should give equal deltas: {deltas}"

    def test_new_player_converges_over_matches(self, rating_system):
        """
        A new player's RD should decrease substantially after a match.
        """
        team1 = [
            (Player(rating=1000, rd=350, vol=0.06), "new_player"),
            (Player(rating=1200, rd=80, vol=0.06), "cal_1"),
            (Player(rating=1200, rd=80, vol=0.06), "cal_2"),
            (Player(rating=1200, rd=80, vol=0.06), "cal_3"),
            (Player(rating=1200, rd=80, vol=0.06), "cal_4"),
        ]
        team2 = [
            (Player(rating=1200, rd=80, vol=0.06), f"enemy_{i}")
            for i in range(5)
        ]

        t1_updated, _ = rating_system.update_ratings_after_match(
            team1, team2, winning_team=1
        )

        new_player_rd = t1_updated[0][1]
        # RD should decrease significantly (at least 10%)
        assert new_player_rd < 350 * 0.9, \
            f"New player RD should decrease significantly: {350} -> {new_player_rd}"


class TestV3TeamBasedExpectedOutcome:
    """Tests verifying that expected outcome is team-vs-team, not individual-vs-team."""

    @pytest.fixture
    def rating_system(self):
        return CamaRatingSystem()

    def test_low_rated_player_on_losing_favorite_loses_appropriately(self, rating_system):
        """
        A low-rated player on a FAVORITE team that LOSES should lose significantly,
        not a tiny amount based on their personal expected outcome.

        This prevents rating compression where low-rated players slowly inflate.
        """
        # Team 1 is the favorite (avg ~1400), Team 2 is underdog (avg ~1000)
        team1 = [
            (Player(rating=500, rd=300, vol=0.06), "low_rated_on_favorite"),  # Low personal rating
            (Player(rating=1600, rd=80, vol=0.06), "high_1"),
            (Player(rating=1500, rd=80, vol=0.06), "high_2"),
            (Player(rating=1500, rd=80, vol=0.06), "high_3"),
            (Player(rating=1500, rd=80, vol=0.06), "high_4"),
        ]
        team2 = [
            (Player(rating=1000, rd=80, vol=0.06), f"underdog_{i}")
            for i in range(5)
        ]

        # Team 1 (favorite) LOSES - this is an upset
        t1_updated, t2_updated = rating_system.update_ratings_after_match(
            team1, team2, winning_team=2
        )

        low_rated_delta = t1_updated[0][0] - 500

        # The low-rated player should LOSE rating because their TEAM lost
        # The loss should be significant because the team was favored
        # (In the broken implementation, they would barely lose anything)
        assert low_rated_delta < -50, \
            f"Low-rated player on losing favorite should lose significantly, got {low_rated_delta}"

    def test_high_rated_player_on_winning_underdog_gains_appropriately(self, rating_system):
        """
        A high-rated player on an UNDERDOG team that WINS should gain significantly,
        not a tiny amount based on their personal expected outcome.

        This prevents rating compression where high-rated players slowly deflate.
        """
        # Team 1 is underdog (avg ~1000), Team 2 is favorite (avg ~1600)
        team1 = [
            (Player(rating=1800, rd=150, vol=0.06), "high_rated_on_underdog"),  # High personal rating
            (Player(rating=800, rd=80, vol=0.06), "low_1"),
            (Player(rating=800, rd=80, vol=0.06), "low_2"),
            (Player(rating=800, rd=80, vol=0.06), "low_3"),
            (Player(rating=800, rd=80, vol=0.06), "low_4"),
        ]
        team2 = [
            (Player(rating=1600, rd=80, vol=0.06), f"favorite_{i}")
            for i in range(5)
        ]

        # Team 1 (underdog) WINS - this is an upset
        t1_updated, t2_updated = rating_system.update_ratings_after_match(
            team1, team2, winning_team=1
        )

        high_rated_delta = t1_updated[0][0] - 1800

        # The high-rated player should GAIN rating because their TEAM won an upset
        # The gain should be significant
        # (In the broken implementation, they would barely gain anything)
        assert high_rated_delta > 50, \
            f"High-rated player on winning underdog should gain significantly, got {high_rated_delta}"

    def test_same_rd_players_get_same_delta_regardless_of_personal_rating(self, rating_system):
        """
        Two players with the same RD on the same team should get similar deltas,
        regardless of their personal ratings.

        This verifies that expected outcome is team-based, not individual-based.
        """
        team1 = [
            (Player(rating=500, rd=150, vol=0.06), "low_rated"),
            (Player(rating=1500, rd=150, vol=0.06), "high_rated"),
            (Player(rating=1000, rd=80, vol=0.06), "filler_1"),
            (Player(rating=1000, rd=80, vol=0.06), "filler_2"),
            (Player(rating=1000, rd=80, vol=0.06), "filler_3"),
        ]
        team2 = [
            (Player(rating=1000, rd=80, vol=0.06), f"enemy_{i}")
            for i in range(5)
        ]

        t1_updated, _ = rating_system.update_ratings_after_match(
            team1, team2, winning_team=1
        )

        low_rated_delta = t1_updated[0][0] - 500
        high_rated_delta = t1_updated[1][0] - 1500

        # Both players have RD=150, so they should get the same delta
        # (within floating point tolerance)
        assert abs(low_rated_delta - high_rated_delta) < 1.0, \
            f"Same RD should give same delta: low={low_rated_delta}, high={high_rated_delta}"


class TestV3InputValidation:
    """Tests for input validation."""

    @pytest.fixture
    def rating_system(self):
        return CamaRatingSystem()

    def test_empty_team1_raises_error(self, rating_system):
        """Empty team1 should raise ValueError."""
        team2 = [(Player(rating=1200, rd=80, vol=0.06), f"player_{i}") for i in range(5)]
        with pytest.raises(ValueError, match="team1_players cannot be empty"):
            rating_system.update_ratings_after_match([], team2, winning_team=1)

    def test_empty_team2_raises_error(self, rating_system):
        """Empty team2 should raise ValueError."""
        team1 = [(Player(rating=1200, rd=80, vol=0.06), f"player_{i}") for i in range(5)]
        with pytest.raises(ValueError, match="team2_players cannot be empty"):
            rating_system.update_ratings_after_match(team1, [], winning_team=1)

    def test_invalid_winning_team_raises_error(self, rating_system):
        """Invalid winning_team should raise ValueError."""
        team1 = [(Player(rating=1200, rd=80, vol=0.06), f"player_{i}") for i in range(5)]
        team2 = [(Player(rating=1200, rd=80, vol=0.06), f"enemy_{i}") for i in range(5)]
        with pytest.raises(ValueError, match="winning_team must be 1 or 2"):
            rating_system.update_ratings_after_match(team1, team2, winning_team=0)
        with pytest.raises(ValueError, match="winning_team must be 1 or 2"):
            rating_system.update_ratings_after_match(team1, team2, winning_team=3)


class TestV3FixesOldBugs:
    """
    These tests verify that V3 fixes the problems with the old systems.
    """

    @pytest.fixture
    def rating_system(self):
        return CamaRatingSystem()

    def test_v3_caps_extreme_wins(self, rating_system):
        """
        V3 should cap extreme win deltas that V2 allowed (e.g., +573).

        Note: Glicko-2 naturally produces asymmetric win/loss when there's a
        large rating difference. A low-rated player losing to a high-rated team
        loses little (expected result), but winning is a huge upset (large gain).
        V3 caps the large gains to prevent +500+ swings.
        """
        team1 = [
            (Player(rating=413, rd=334, vol=0.06), "high_rd"),
            (Player(rating=1400, rd=80, vol=0.06), "cal_1"),
            (Player(rating=1350, rd=75, vol=0.06), "cal_2"),
            (Player(rating=1300, rd=85, vol=0.06), "cal_3"),
            (Player(rating=1250, rd=90, vol=0.06), "cal_4"),
        ]
        team2 = [
            (Player(rating=1350, rd=80, vol=0.06), "e1"),
            (Player(rating=1300, rd=85, vol=0.06), "e2"),
            (Player(rating=1250, rd=75, vol=0.06), "e3"),
            (Player(rating=1200, rd=90, vol=0.06), "e4"),
            (Player(rating=1180, rd=80, vol=0.06), "e5"),
        ]

        # Win case - this is where V2 gave +573
        team1_w = [(Player(rating=p.rating, rd=p.rd, vol=p.vol), pid) for p, pid in team1]
        team2_w = [(Player(rating=p.rating, rd=p.rd, vol=p.vol), pid) for p, pid in team2]
        t1_w, _ = rating_system.update_ratings_after_match(team1_w, team2_w, winning_team=1)
        win_delta = t1_w[0][0] - 413

        # Loss case
        team1_l = [(Player(rating=p.rating, rd=p.rd, vol=p.vol), pid) for p, pid in team1]
        team2_l = [(Player(rating=p.rating, rd=p.rd, vol=p.vol), pid) for p, pid in team2]
        t1_l, _ = rating_system.update_ratings_after_match(team1_l, team2_l, winning_team=2)
        loss_delta = t1_l[0][0] - 413

        # Win delta should be capped (V2 allowed +573, V3 caps at 400)
        assert win_delta <= MAX_RATING_SWING_PER_GAME, \
            f"Win delta should be capped at {MAX_RATING_SWING_PER_GAME}, got {win_delta}"
        # Win should still be substantial (it's an upset)
        assert win_delta > 100, f"Win delta should be substantial for upset, got {win_delta}"
        # Loss should be negative
        assert loss_delta < 0, f"Loss delta should be negative, got {loss_delta}"
        # Loss also bounded
        assert loss_delta >= -MAX_RATING_SWING_PER_GAME, \
            f"Loss delta should be capped, got {loss_delta}"

    def test_v3_no_teammate_rd_inflation(self, rating_system):
        """
        V3 should not inflate a calibrated player's delta just because
        they have a high-RD teammate (the V2 bug).
        """
        # The Michael Horak scenario: one high-RD player among calibrated teammates
        team1_with_high_rd = [
            (Player(rating=413, rd=325, vol=0.06), "michael_horak"),  # High RD
            (Player(rating=1400, rd=80, vol=0.06), "calibrated_1"),
            (Player(rating=1350, rd=75, vol=0.06), "calibrated_2"),
            (Player(rating=1300, rd=85, vol=0.06), "calibrated_3"),
            (Player(rating=1250, rd=90, vol=0.06), "calibrated_4"),
        ]
        team2 = [
            (Player(rating=1256, rd=82, vol=0.06), f"enemy_{i}")
            for i in range(5)
        ]

        t1_updated, _ = rating_system.update_ratings_after_match(
            team1_with_high_rd, team2, winning_team=1
        )

        michael_delta = t1_updated[0][0] - 413

        # V3 should cap the delta at MAX_RATING_SWING_PER_GAME
        # V2 allowed 573 because of RD² concentration
        assert michael_delta <= MAX_RATING_SWING_PER_GAME, \
            f"High-RD player delta should be capped: got {michael_delta}"

        # Also verify calibrated teammates get reasonable deltas
        for i in range(1, 5):
            cal_delta = t1_updated[i][0] - team1_with_high_rd[i][0].rating
            assert 5 < cal_delta < 60, \
                f"Calibrated player {i} should get moderate delta: got {cal_delta}"


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
