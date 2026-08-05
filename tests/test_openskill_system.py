"""
Tests for OpenSkill Plackett-Luce rating system.

Tests cover:
- Native bounded contribution weights without min/max expansion
- Asymmetric performance behavior (high FP = more credit on win, less blame on loss)
- Uncapped native mu and sigma posteriors
- Post-hoc streak scaling of native mu deltas
- Inactivity uncertainty decay
"""

import pytest

from openskill_rating_system import CamaOpenSkillSystem


class TestWeightBlending:
    """Test that direct contribution weights stay bounded."""

    def test_weight_blending_reduces_fp_impact(self):
        """High FP player should only get ~1.1x the gain, not 3x."""
        system = CamaOpenSkillSystem()

        # Two players on winning team: one with high FP, one with low FP
        # Both start at same rating
        starting_mu = 30.0
        starting_sigma = 8.0

        high_fp_player = (1, starting_mu, starting_sigma, 30.0)  # Max FP
        low_fp_player = (2, starting_mu, starting_sigma, 5.0)  # Min FP

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

        # The revised absolute-score nudge must remain small.
        ratio = high_fp_gain / low_fp_gain if low_fp_gain > 0 else float("inf")
        assert ratio < 1.25, f"FP impact too high: {ratio:.2f}x gain ratio"
        assert ratio > 1.0, f"High FP should still gain more: {ratio:.2f}x"

    def test_one_point_difference_does_not_expand_to_two_x(self):
        system = CamaOpenSkillSystem()
        team1 = [
            (1, 45.0, 4.0, 15.0),
            (2, 45.0, 4.0, 15.0),
            (3, 45.0, 4.0, 15.0),
            (4, 45.0, 4.0, 15.0),
            (5, 45.0, 4.0, 16.0),
        ]
        team2 = [(pid, 45.0, 4.0, 15.0) for pid in range(6, 11)]

        weighted = system.update_ratings_after_match(team1, team2, 1)
        equal = system.update_ratings_equal_weight(
            [(pid, mu, sigma) for pid, mu, sigma, _fp in team1],
            [(pid, mu, sigma) for pid, mu, sigma, _fp in team2],
            1,
        )

        base = equal[5][0] - 45.0
        weighted_delta = weighted[5][0] - 45.0
        assert weighted_delta / base < 1.01

    def test_performance_can_change_each_teams_total_movement(self):
        system = CamaOpenSkillSystem()
        team1 = [
            (1, 45.0, 4.0, 30.0),
            (2, 45.0, 8.0, 5.0),
        ]
        team2 = [
            (3, 45.0, 4.0, 30.0),
            (4, 45.0, 8.0, 5.0),
        ]
        weighted = system.update_ratings_after_match(team1, team2, 1)
        equal = system.update_ratings_equal_weight(
            [(pid, mu, sigma) for pid, mu, sigma, _fp in team1],
            [(pid, mu, sigma) for pid, mu, sigma, _fp in team2],
            1,
        )

        for player_ids in ((1, 2), (3, 4)):
            weighted_total = sum(weighted[pid][0] - 45.0 for pid in player_ids)
            equal_total = sum(equal[pid][0] - 45.0 for pid in player_ids)
            assert weighted_total != pytest.approx(equal_total)

    def test_model_does_not_expand_direct_weights(self):
        system = CamaOpenSkillSystem()

        assert system.model.weight_bounds is None

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
        low_fp_loser = (4, starting_mu, starting_sigma, 5.0)  # Low FP (contributed to loss)
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

    def test_native_weights_are_persistable_and_bounded(self):
        """The returned third value is the contribution weight sent to OpenSkill."""
        system = CamaOpenSkillSystem()
        team1 = [(1, 35.0, 4.0, 30.0), (2, 35.0, 4.0, 5.0)]
        team2 = [(3, 35.0, 4.0, 30.0), (4, 35.0, 4.0, 5.0)]

        results = system.update_ratings_after_match(team1, team2, 1)

        assert 1.0 < results[1][2] < 1.15
        assert 0.85 < results[2][2] < 1.0
        assert 1.0 < results[3][2] < 1.15
        assert 0.85 < results[4][2] < 1.0


class TestNativePosterior:
    """The default 1x streak policy preserves OpenSkill's native posterior."""

    def test_weighted_update_matches_installed_plackett_luce_exactly(self):
        system = CamaOpenSkillSystem()
        team1 = [(1, 42.0, 5.0, 30.0), (2, 38.0, 7.0, 5.0)]
        team2 = [(3, 45.0, 4.0, 20.0), (4, 35.0, 6.0, 10.0)]
        factors = [
            system._performance_factors([30.0, 5.0]),
            system._performance_factors([20.0, 10.0]),
        ]

        actual = system.update_ratings_after_match(team1, team2, 1)
        direct = system.model.rate(
            [
                [system.create_rating(mu, sigma) for _pid, mu, sigma, _fp in team1],
                [system.create_rating(mu, sigma) for _pid, mu, sigma, _fp in team2],
            ],
            ranks=[1, 2],
            weights=factors,
        )

        assert actual[1][:2] == pytest.approx((direct[0][0].mu, direct[0][0].sigma))
        assert actual[2][:2] == pytest.approx((direct[0][1].mu, direct[0][1].sigma))
        assert actual[3][:2] == pytest.approx((direct[1][0].mu, direct[1][0].sigma))
        assert actual[4][:2] == pytest.approx((direct[1][1].mu, direct[1][1].sigma))

    def test_uncertain_upset_is_not_capped_at_two_mu(self):
        system = CamaOpenSkillSystem()

        low_mu = 25.0
        high_sigma = 8.333

        team1_data = [(1, low_mu, high_sigma, 30.0)]
        team2_data = [(2, 60.0, 4.0, 5.0)]

        results = system.update_ratings_after_match(team1_data, team2_data, winning_team=1)

        assert results[1][0] - low_mu > 2.0

    def test_native_mu_can_move_below_display_origin(self):
        system = CamaOpenSkillSystem()
        team1_data = [(1, 60.0, 4.0, 30.0)]
        team2_data = [(2, system.MIN_MU, 8.0, 5.0)]

        results = system.update_ratings_after_match(team1_data, team2_data, winning_team=1)

        assert results[2][0] < system.MIN_MU
        assert system.mu_to_display(results[2][0]) == 0


class TestStreakOverlay:
    """Streaks scale only native mu deltas, preserving the OpenSkill posterior."""

    def test_weighted_update_scales_only_selected_players_mu_delta(self):
        system = CamaOpenSkillSystem()
        starting_mu = 40.0
        team1 = [
            (1, starting_mu, 5.0, 30.0),
            (2, starting_mu, 5.0, 5.0),
        ]
        team2 = [
            (3, starting_mu, 5.0, 30.0),
            (4, starting_mu, 5.0, 5.0),
        ]

        native = system.update_ratings_after_match(team1, team2, 1)
        streaked = system.update_ratings_after_match(
            team1,
            team2,
            1,
            streak_multipliers={1: 1.75, 4: 1.5},
        )

        assert streaked[1][0] == pytest.approx(starting_mu + (native[1][0] - starting_mu) * 1.75)
        assert streaked[4][0] == pytest.approx(starting_mu + (native[4][0] - starting_mu) * 1.5)
        assert streaked[2][0] == native[2][0]
        assert streaked[3][0] == native[3][0]

        for player_id in (1, 2, 3, 4):
            assert streaked[player_id][1] == native[player_id][1]
            assert streaked[player_id][2] == native[player_id][2]

    def test_explicit_one_x_preserves_native_values_exactly(self):
        system = CamaOpenSkillSystem()
        team1 = [(1, 42.0, 5.0, 30.0), (2, 38.0, 7.0, 5.0)]
        team2 = [(3, 45.0, 4.0, 20.0), (4, 35.0, 6.0, 10.0)]

        native = system.update_ratings_after_match(team1, team2, 1)
        explicit_one = system.update_ratings_after_match(
            team1,
            team2,
            1,
            streak_multipliers=dict.fromkeys((1, 2, 3, 4), 1.0),
        )

        assert explicit_one == native

    def test_equal_weight_api_forwards_streak_multipliers(self):
        system = CamaOpenSkillSystem()
        starting_mu = 35.0
        team1 = [(1, starting_mu, 8.0), (2, starting_mu, 8.0)]
        team2 = [(3, starting_mu, 8.0), (4, starting_mu, 8.0)]

        native = system.update_ratings_equal_weight(team1, team2, 1)
        streaked = system.update_ratings_equal_weight(
            team1,
            team2,
            1,
            streak_multipliers={1: 1.75, 3: 1.5},
        )

        assert streaked[1][0] == pytest.approx(starting_mu + (native[1][0] - starting_mu) * 1.75)
        assert streaked[3][0] == pytest.approx(starting_mu + (native[3][0] - starting_mu) * 1.5)
        assert streaked[2] == native[2]
        assert streaked[4] == native[4]
        assert streaked[1][1] == native[1][1]
        assert streaked[3][1] == native[3][1]


class TestEqualWeightUpdate:
    """Test equal weight updates (for non-enriched matches)."""

    def test_equal_weight_uses_native_posterior(self):
        system = CamaOpenSkillSystem()

        team1_data = [(1, 60.0, 4.0)]
        team2_data = [(2, system.MIN_MU, 8.0)]

        results = system.update_ratings_equal_weight(team1_data, team2_data, winning_team=1)

        direct = system.model.rate(
            [[system.create_rating(60.0, 4.0)], [system.create_rating(system.MIN_MU, 8.0)]],
            ranks=[1, 2],
        )
        assert results[1] == pytest.approx((direct[0][0].mu, direct[0][0].sigma))
        assert results[2] == pytest.approx((direct[1][0].mu, direct[1][0].sigma))

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


class TestSigmaDecay:
    def test_decay_respects_grace_period(self):
        system = CamaOpenSkillSystem()

        assert system.apply_sigma_decay(4.0, 7) == pytest.approx(4.0)

    def test_decay_increases_uncertainty_by_variance(self):
        system = CamaOpenSkillSystem()

        decayed = system.apply_sigma_decay(4.0, 14)

        assert decayed == pytest.approx((4.0**2 + 2.0**2) ** 0.5)

    def test_decay_caps_at_new_player_uncertainty(self):
        system = CamaOpenSkillSystem()

        decayed = system.apply_sigma_decay(4.0, 1000)

        assert decayed == pytest.approx(system.DEFAULT_SIGMA)


class TestWinProbabilityCalibration:
    """Test post-hoc calibration of OpenSkill win probabilities."""

    def test_calibration_preserves_coin_flip(self):
        system = CamaOpenSkillSystem()

        assert system.calibrate_win_probability(0.5) == pytest.approx(0.5)

    def test_calibration_softens_extreme_favorite_without_flipping(self):
        system = CamaOpenSkillSystem()

        calibrated = system.calibrate_win_probability(0.9)

        assert 0.5 < calibrated < 0.9

    def test_calibration_is_symmetric(self):
        system = CamaOpenSkillSystem()

        favorite = system.calibrate_win_probability(0.85)
        underdog = system.calibrate_win_probability(0.15)

        assert favorite == pytest.approx(1.0 - underdog)

    def test_calibrated_prediction_keeps_raw_probability_available(self):
        system = CamaOpenSkillSystem()
        team1 = [(60.0, 4.0)] * 5
        team2 = [(35.0, 4.0)] * 5

        raw = system.os_predict_win_probability(team1, team2)
        calibrated = system.os_predict_calibrated_win_probability(team1, team2)

        assert raw > 0.5
        assert 0.5 < calibrated < raw


class TestDisplayScaling:
    """Test that mu_to_display correctly maps to 0-3000 range matching Glicko-2."""

    def test_mu_to_display_floor(self):
        """MIN_MU should map to display 0."""
        system = CamaOpenSkillSystem()
        display = system.mu_to_display(system.MIN_MU)
        assert display == 0, f"MIN_MU ({system.MIN_MU}) should map to display 0, got {display}"

    def test_mu_to_display_ceiling_at_max_mmr(self):
        """mu=85 (the mu produced by MMR_MAX=12000) should map to display 3000."""
        system = CamaOpenSkillSystem()
        display = system.mu_to_display(85.0)
        assert display == 3000, f"mu=85 should map to display 3000, got {display}"

    def test_mu_to_display_mid(self):
        """mu=45 should map to display ~1000 (matches MMR 4000 on the 0-3000 scale)."""
        system = CamaOpenSkillSystem()
        display = system.mu_to_display(45.0)
        assert 900 < display < 1100, f"mu=45 should map to ~1000, got {display}"

    def test_mu_to_display_matches_glicko_scale_at_max_mmr(self):
        """mu_to_display(mmr_to_os_mu(MMR_MAX)) must equal Glicko-2's RATING_MAX.

        This is the core invariant: a player with max MMR should have the same
        display rating under either system, so team-value calculations don't
        favor one over the other.
        """
        from rating_system import CamaRatingSystem

        system = CamaOpenSkillSystem()
        max_mmr = CamaRatingSystem.MMR_MAX
        mu_at_max = system.mmr_to_os_mu(max_mmr)
        os_display = system.mu_to_display(mu_at_max)
        glicko_display = CamaRatingSystem.RATING_MAX
        assert os_display == glicko_display, (
            f"OpenSkill display at max MMR ({os_display}) must equal "
            f"Glicko-2 RATING_MAX ({glicko_display})"
        )


def test_algorithm_fingerprint_changes_with_rating_configuration(monkeypatch):
    original = CamaOpenSkillSystem.algorithm_fingerprint()

    monkeypatch.setattr(
        CamaOpenSkillSystem,
        "PERFORMANCE_STRENGTH",
        CamaOpenSkillSystem.PERFORMANCE_STRENGTH + 0.01,
    )

    assert CamaOpenSkillSystem.algorithm_fingerprint() != original


def test_algorithm_fingerprint_includes_streak_configuration(monkeypatch):
    original = CamaOpenSkillSystem.algorithm_fingerprint()

    monkeypatch.setattr(
        CamaOpenSkillSystem,
        "STREAK_MULTIPLIER_PER_GAME",
        CamaOpenSkillSystem.STREAK_MULTIPLIER_PER_GAME + 0.01,
    )

    assert CamaOpenSkillSystem.algorithm_fingerprint() != original
