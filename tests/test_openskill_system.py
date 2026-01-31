"""
Tests for OpenSkill Plackett-Luce rating system.

Tests cover:
- Weight blending (25% FP, 75% equal)
- Asymmetric weight behavior (high FP = more credit on win, less blame on loss)
- Per-game mu swing cap (±2.0)
- Mu floor enforcement (minimum 25)
"""

import pytest

from openskill_rating_system import CamaOpenSkillSystem


class TestWeightBlending:
    """Test that FP impact is limited to 10% blend with equal weight."""

    def test_weight_blending_reduces_fp_impact(self):
        """High FP player should only get ~1.1x the gain, not 3x."""
        system = CamaOpenSkillSystem()

        # Two players on winning team: one with high FP, one with low FP
        # Both start at same rating
        starting_mu = 30.0
        starting_sigma = 8.0

        high_fp_player = (1, starting_mu, starting_sigma, 30.0)  # Max FP
        low_fp_player = (2, starting_mu, starting_sigma, 5.0)   # Min FP

        # Both on winning team 1
        team1_data = [high_fp_player, low_fp_player]
        # Dummy losing team
        team2_data = [
            (3, starting_mu, starting_sigma, 15.0),
            (4, starting_mu, starting_sigma, 15.0),
        ]

        results = system.update_ratings_after_match(team1_data, team2_data, winning_team=1)

        high_fp_new_mu = results[1][0]
        low_fp_new_mu = results[2][0]

        # Both should gain rating (winners)
        assert high_fp_new_mu > starting_mu, "High FP winner should gain rating"
        assert low_fp_new_mu > starting_mu, "Low FP winner should gain rating"

        # High FP should gain more, but not dramatically more
        high_fp_gain = high_fp_new_mu - starting_mu
        low_fp_gain = low_fp_new_mu - starting_mu

        # With 25% blend, the ratio should be much less than 3:1 (raw weight ratio)
        # Expected: ~1.1-1.2x difference, not 3x
        ratio = high_fp_gain / low_fp_gain if low_fp_gain > 0 else float('inf')
        assert ratio < 1.5, f"FP impact too high: {ratio:.2f}x gain ratio (should be < 1.5x)"
        assert ratio > 1.0, f"High FP should still gain more: {ratio:.2f}x"

    def test_equal_fp_gives_equal_gains(self):
        """Players with equal FP should get equal rating changes."""
        system = CamaOpenSkillSystem()

        starting_mu = 30.0
        starting_sigma = 8.0

        team1_data = [
            (1, starting_mu, starting_sigma, 15.0),
            (2, starting_mu, starting_sigma, 15.0),
        ]
        team2_data = [
            (3, starting_mu, starting_sigma, 15.0),
            (4, starting_mu, starting_sigma, 15.0),
        ]

        results = system.update_ratings_after_match(team1_data, team2_data, winning_team=1)

        # Winners should have equal gains
        assert abs(results[1][0] - results[2][0]) < 0.01, "Equal FP winners should have equal mu"

        # Losers should have equal losses
        assert abs(results[3][0] - results[4][0]) < 0.01, "Equal FP losers should have equal mu"


class TestLossWeightInversion:
    """Test that high FP on losing team means less rating loss."""

    def test_loss_weight_inversion(self):
        """On losing team, high FP player should lose LESS rating than low FP player."""
        system = CamaOpenSkillSystem()

        starting_mu = 35.0
        starting_sigma = 8.0

        # Winning team (dummy)
        team1_data = [
            (1, starting_mu, starting_sigma, 15.0),
            (2, starting_mu, starting_sigma, 15.0),
        ]

        # Losing team: high FP and low FP players
        high_fp_loser = (3, starting_mu, starting_sigma, 30.0)  # High FP (played well but lost)
        low_fp_loser = (4, starting_mu, starting_sigma, 5.0)    # Low FP (contributed to loss)
        team2_data = [high_fp_loser, low_fp_loser]

        results = system.update_ratings_after_match(team1_data, team2_data, winning_team=1)

        high_fp_new_mu = results[3][0]
        low_fp_new_mu = results[4][0]

        # Both should lose rating (losers)
        assert high_fp_new_mu < starting_mu, "High FP loser should lose rating"
        assert low_fp_new_mu < starting_mu, "Low FP loser should lose rating"

        # High FP should lose LESS than low FP (inverted blame)
        high_fp_loss = starting_mu - high_fp_new_mu
        low_fp_loss = starting_mu - low_fp_new_mu

        assert high_fp_loss < low_fp_loss, (
            f"High FP loser should lose less ({high_fp_loss:.3f}) than "
            f"low FP loser ({low_fp_loss:.3f})"
        )

    def test_compute_match_weights_blending(self):
        """Test compute_match_weights produces correct blended weights."""
        system = CamaOpenSkillSystem()

        # High FP = 30, Low FP = 5
        team1_fantasy = [30.0, 5.0]
        team2_fantasy = [30.0, 5.0]

        # Team 1 wins (doesn't affect weights - no inversion)
        team1_weights, team2_weights = system.compute_match_weights(
            team1_fantasy, team2_fantasy, team1_won=True
        )

        # Both teams should have same weights (no inversion)
        # High FP (30) → raw 3.0 → blended = 0.10*3.0 + 0.90*1.0 = 1.2
        # Low FP (5) → raw 1.0 → blended = 0.10*1.0 + 0.90*1.0 = 1.0
        assert team1_weights[0] > team1_weights[1], (
            f"High FP should have higher weight: "
            f"high FP={team1_weights[0]:.3f}, low FP={team1_weights[1]:.3f}"
        )
        assert team2_weights[0] > team2_weights[1], (
            f"High FP should have higher weight: "
            f"high FP={team2_weights[0]:.3f}, low FP={team2_weights[1]:.3f}"
        )

        # Verify blending reduces impact (weights should be closer to 1.0 than raw)
        assert 1.0 < team1_weights[0] < 2.0, "High FP weight should be blended"
        assert 0.9 < team1_weights[1] < 1.1, "Low FP weight should be near 1.0"


class TestMuSwingCap:
    """Test that per-game mu change is capped at ±2.0."""

    def test_mu_swing_cap_on_win(self):
        """Winner's mu gain should be capped at MAX_MU_SWING_PER_GAME."""
        system = CamaOpenSkillSystem()

        # Very low rated player beats very high rated players
        # Without cap, this would result in massive gain
        low_mu = 25.0
        high_sigma = 8.333  # High uncertainty = large swings

        team1_data = [(1, low_mu, high_sigma, 30.0)]
        team2_data = [(2, 60.0, 4.0, 5.0)]  # Very high mu, calibrated

        results = system.update_ratings_after_match(team1_data, team2_data, winning_team=1)

        new_mu = results[1][0]
        mu_change = new_mu - low_mu

        assert mu_change <= system.MAX_MU_SWING_PER_GAME, (
            f"Mu gain should be capped at {system.MAX_MU_SWING_PER_GAME}, "
            f"got {mu_change:.3f}"
        )

    def test_mu_swing_cap_on_loss(self):
        """Loser's mu loss should be capped at MAX_MU_SWING_PER_GAME."""
        system = CamaOpenSkillSystem()

        # High rated player loses to low rated players
        high_mu = 60.0
        high_sigma = 8.333

        team1_data = [(1, 25.0, 4.0, 30.0)]  # Low mu, calibrated
        team2_data = [(2, high_mu, high_sigma, 5.0)]  # High mu, uncertain

        results = system.update_ratings_after_match(team1_data, team2_data, winning_team=1)

        new_mu = results[2][0]
        mu_change = high_mu - new_mu  # Loss is positive

        assert mu_change <= system.MAX_MU_SWING_PER_GAME, (
            f"Mu loss should be capped at {system.MAX_MU_SWING_PER_GAME}, "
            f"got {mu_change:.3f}"
        )


class TestMuFloorEnforcement:
    """Test that mu cannot go below MIN_MU (25.0)."""

    def test_mu_floor_enforcement(self):
        """Player at minimum mu should not go below floor after loss."""
        system = CamaOpenSkillSystem()

        # Player at floor
        floor_mu = system.MIN_MU
        team1_data = [(1, 60.0, 4.0, 30.0)]  # High rated winner
        team2_data = [(2, floor_mu, 8.0, 5.0)]  # At floor, loses

        results = system.update_ratings_after_match(team1_data, team2_data, winning_team=1)

        new_mu = results[2][0]

        assert new_mu >= system.MIN_MU, (
            f"Mu should not go below {system.MIN_MU}, got {new_mu:.3f}"
        )

    def test_mu_floor_applied_after_clamping(self):
        """Floor should be applied even if unclamped mu would go below."""
        system = CamaOpenSkillSystem()

        # Player just above floor with high uncertainty
        near_floor_mu = 26.0
        high_sigma = 8.333

        team1_data = [(1, 60.0, 4.0, 30.0)]
        team2_data = [(2, near_floor_mu, high_sigma, 5.0)]

        results = system.update_ratings_after_match(team1_data, team2_data, winning_team=1)

        new_mu = results[2][0]

        assert new_mu >= system.MIN_MU, (
            f"Mu should be floored at {system.MIN_MU}, got {new_mu:.3f}"
        )

    def test_multiple_losses_cannot_break_floor(self):
        """Repeated losses should keep player at floor, not below."""
        system = CamaOpenSkillSystem()

        current_mu = system.MIN_MU
        sigma = 8.0

        # Simulate 5 consecutive losses
        for i in range(5):
            team1_data = [(1, 60.0, 4.0, 30.0)]
            team2_data = [(2, current_mu, sigma, 5.0)]

            results = system.update_ratings_after_match(team1_data, team2_data, winning_team=1)
            current_mu = results[2][0]
            sigma = results[2][1]

            assert current_mu >= system.MIN_MU, (
                f"After loss {i+1}, mu should be >= {system.MIN_MU}, got {current_mu:.3f}"
            )


class TestEqualWeightUpdate:
    """Test equal weight updates (for non-enriched matches)."""

    def test_equal_weight_uses_clamping(self):
        """Equal weight updates should also use mu clamping and floor."""
        system = CamaOpenSkillSystem()

        # Player at floor
        team1_data = [(1, 60.0, 4.0)]  # No fantasy points
        team2_data = [(2, system.MIN_MU, 8.0)]

        results = system.update_ratings_equal_weight(team1_data, team2_data, winning_team=1)

        new_mu = results[2][0]
        assert new_mu >= system.MIN_MU, "Equal weight should enforce floor"

    def test_equal_weight_symmetry(self):
        """Equal weight updates should give symmetric changes to equal players."""
        system = CamaOpenSkillSystem()

        starting_mu = 35.0
        starting_sigma = 8.0

        team1_data = [(1, starting_mu, starting_sigma), (2, starting_mu, starting_sigma)]
        team2_data = [(3, starting_mu, starting_sigma), (4, starting_mu, starting_sigma)]

        results = system.update_ratings_equal_weight(team1_data, team2_data, winning_team=1)

        # All winners should have same mu
        assert abs(results[1][0] - results[2][0]) < 0.01

        # All losers should have same mu
        assert abs(results[3][0] - results[4][0]) < 0.01


class TestDisplayScaling:
    """Test that mu_to_display correctly maps to 0-3000 range."""

    def test_mu_to_display_floor(self):
        """MIN_MU should map to display 0."""
        system = CamaOpenSkillSystem()
        display = system.mu_to_display(system.MIN_MU)
        assert display == 0, f"MIN_MU ({system.MIN_MU}) should map to display 0, got {display}"

    def test_mu_to_display_ceiling(self):
        """mu=65 should map to display 3000."""
        system = CamaOpenSkillSystem()
        display = system.mu_to_display(65.0)
        assert display == 3000, f"mu=65 should map to display 3000, got {display}"

    def test_mu_to_display_mid(self):
        """mu=45 should map to display ~1500."""
        system = CamaOpenSkillSystem()
        display = system.mu_to_display(45.0)
        assert 1400 < display < 1600, f"mu=45 should map to ~1500, got {display}"
