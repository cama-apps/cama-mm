"""Tests for team composition winrate analysis in rating_insights."""

import pytest

from utils.rating_insights import (
    _gini_coefficient,
    _pearson_r,
    _compute_team_composition_stats,
    compute_calibration_stats,
)
from domain.models.player import Player


class TestGiniCoefficient:
    """Tests for _gini_coefficient."""

    def test_equal_values_returns_zero(self):
        assert _gini_coefficient([1200] * 5) == 0.0

    def test_single_value_returns_zero(self):
        assert _gini_coefficient([1200]) == 0.0

    def test_empty_returns_zero(self):
        assert _gini_coefficient([]) == 0.0

    def test_unequal_values_positive(self):
        result = _gini_coefficient([800, 1000, 1200, 1400, 1600])
        assert result > 0

    def test_max_inequality(self):
        result = _gini_coefficient([0, 0, 0, 0, 1000])
        assert abs(result - 0.8) < 0.001

    def test_symmetric(self):
        """Order of values shouldn't affect the result."""
        a = _gini_coefficient([800, 1000, 1200, 1400, 1600])
        b = _gini_coefficient([1600, 800, 1400, 1000, 1200])
        assert abs(a - b) < 1e-10

    def test_zero_mean_returns_zero(self):
        assert _gini_coefficient([0, 0, 0]) == 0.0

    def test_negative_mean_returns_zero(self):
        assert _gini_coefficient([-10, -20, -30]) == 0.0


class TestPearsonR:
    """Tests for _pearson_r."""

    def test_perfect_positive(self):
        r = _pearson_r([1, 2, 3], [1, 2, 3])
        assert r is not None
        assert abs(r - 1.0) < 1e-10

    def test_perfect_negative(self):
        r = _pearson_r([1, 2, 3], [3, 2, 1])
        assert r is not None
        assert abs(r - (-1.0)) < 1e-10

    def test_too_few_points(self):
        assert _pearson_r([1, 2], [1, 2]) is None

    def test_constant_x_returns_none(self):
        assert _pearson_r([1, 1, 1], [1, 2, 3]) is None

    def test_constant_y_returns_none(self):
        assert _pearson_r([1, 2, 3], [5, 5, 5]) is None

    def test_empty_returns_none(self):
        assert _pearson_r([], []) is None


def _make_rating_history_entry(match_id, team_number, rating_before, expected_win_prob, won):
    """Helper to create a rating_history dict entry."""
    return {
        "match_id": match_id,
        "team_number": team_number,
        "rating_before": rating_before,
        "rating": rating_before + (10 if won else -10),
        "rd_before": 100,
        "expected_team_win_prob": expected_win_prob,
        "won": won,
    }


def _make_team_entries(match_id, team_number, ratings, expected_win_prob, won):
    """Helper to create 5 rating_history entries for a team."""
    return [
        _make_rating_history_entry(match_id, team_number, r, expected_win_prob, won)
        for r in ratings
    ]


class TestComputeTeamCompositionStats:
    """Tests for _compute_team_composition_stats with Gini-based analysis."""

    def test_empty_input(self):
        result = _compute_team_composition_stats([])
        assert result["halves"] == []
        assert result["total_teams"] == 0
        assert result["gini_correlation"] is None

    def test_teams_with_fewer_than_5_players_excluded(self):
        entries = [
            _make_rating_history_entry(1, 1, 1200, 0.5, True),
            _make_rating_history_entry(1, 1, 1210, 0.5, True),
            _make_rating_history_entry(1, 1, 1190, 0.5, True),
        ]
        result = _compute_team_composition_stats(entries)
        assert result["total_teams"] == 0

    def test_below_display_threshold(self):
        """Fewer than 6 teams should produce empty halves."""
        entries = []
        for i in range(4):
            entries.extend(
                _make_team_entries(i, 1, [1200, 1210, 1190, 1205, 1195], 0.5, True)
            )
        result = _compute_team_composition_stats(entries)
        assert result["total_teams"] == 4
        assert result["halves"] == []

    def test_index_split_with_ties(self):
        """Teams with identical Gini still split deterministically into n//2 halves."""
        entries = []
        # 6 teams all with identical ratings -> identical Gini
        for i in range(6):
            won = i < 3
            entries.extend(
                _make_team_entries(i, 1, [1200, 1200, 1200, 1200, 1200], 0.5, won)
            )
        result = _compute_team_composition_stats(entries)
        assert result["total_teams"] == 6
        halves = result["halves"]
        assert len(halves) == 2
        assert halves[0]["total"] == 3  # first n//2
        assert halves[1]["total"] == 3
        assert halves[0]["name"] == "Similar ratings"
        assert halves[1]["name"] == "Mixed ratings"

    def test_overperformance_calculation(self):
        """Verify winrate - avg_expected for each half."""
        entries = []
        # 3 low-spread teams: all win, expected 0.4
        for i in range(3):
            entries.extend(
                _make_team_entries(i, 1, [1200, 1200, 1200, 1200, 1200], 0.4, True)
            )
        # 3 high-spread teams: all lose, expected 0.6
        for i in range(3, 6):
            entries.extend(
                _make_team_entries(i, 1, [800, 1000, 1200, 1400, 1600], 0.6, False)
            )
        result = _compute_team_composition_stats(entries)
        halves = result["halves"]
        assert len(halves) == 2
        # Lower Gini half (similar ratings): winrate=1.0, expected=0.4
        similar = halves[0]
        assert similar["name"] == "Similar ratings"
        assert abs(similar["overperformance"] - (1.0 - 0.4)) < 0.001
        # Upper Gini half (mixed ratings): winrate=0.0, expected=0.6
        mixed = halves[1]
        assert mixed["name"] == "Mixed ratings"
        assert abs(mixed["overperformance"] - (0.0 - 0.6)) < 0.001

    def test_gini_correlation_computed(self):
        """Verify r is a float when enough teams with varying Gini."""
        entries = []
        # 3 low-spread teams
        for i in range(3):
            entries.extend(
                _make_team_entries(i, 1, [1200, 1200, 1200, 1200, 1200], 0.5, True)
            )
        # 3 high-spread teams
        for i in range(3, 6):
            entries.extend(
                _make_team_entries(i, 1, [800, 1000, 1200, 1400, 1600], 0.5, False)
            )
        result = _compute_team_composition_stats(entries)
        r = result["gini_correlation"]
        assert r is not None
        assert isinstance(r, float)

    def test_gini_correlation_none_when_constant_gini(self):
        """All teams with the same Gini -> r is None."""
        entries = []
        for i in range(6):
            entries.extend(
                _make_team_entries(i, 1, [1200, 1200, 1200, 1200, 1200], 0.5, i < 3)
            )
        result = _compute_team_composition_stats(entries)
        assert result["gini_correlation"] is None

    def test_mixed_teams_both_sides(self):
        """Both teams in a match should be analyzed independently."""
        entries = []
        for i in range(5):
            entries.extend(
                _make_team_entries(i, 1, [1200, 1210, 1190, 1205, 1195], 0.5, True)
            )
            entries.extend(
                _make_team_entries(i, 2, [800, 1000, 1200, 1400, 1600], 0.5, False)
            )
        result = _compute_team_composition_stats(entries)
        assert result["total_teams"] == 10

    def test_missing_rating_before_skipped(self):
        """Entries with None rating_before should cause team to be skipped."""
        entries = _make_team_entries(1, 1, [1200, 1210, 1190, 1205, 1195], 0.5, True)
        entries[0]["rating_before"] = None
        result = _compute_team_composition_stats(entries)
        assert result["total_teams"] == 0

    def test_missing_expected_win_prob_skipped(self):
        """Teams where expected_team_win_prob is None should be skipped."""
        entries = _make_team_entries(1, 1, [1200, 1210, 1190, 1205, 1195], None, True)
        result = _compute_team_composition_stats(entries)
        assert result["total_teams"] == 0


class TestComputeCalibrationStatsIntegration:
    """Integration test: compute_calibration_stats returns team_composition."""

    def test_returns_team_composition_key(self):
        players = [
            Player(
                name="Player1", mmr=3000, initial_mmr=3000,
                preferred_roles=["1"], main_role="1",
                glicko_rating=1200, glicko_rd=100, glicko_volatility=0.06,
                os_mu=None, os_sigma=None, discord_id=1,
            )
        ]
        result = compute_calibration_stats(players, match_count=0)
        assert "team_composition" in result

    def test_team_composition_structure(self):
        players = [
            Player(
                name="Player1", mmr=3000, initial_mmr=3000,
                preferred_roles=["1"], main_role="1",
                glicko_rating=1200, glicko_rd=100, glicko_volatility=0.06,
                os_mu=None, os_sigma=None, discord_id=1,
            )
        ]
        result = compute_calibration_stats(players, match_count=0)
        tc = result["team_composition"]
        assert "halves" in tc
        assert "total_teams" in tc
        assert "gini_correlation" in tc
        assert isinstance(tc["halves"], list)

    def test_with_rating_history(self):
        players = [
            Player(
                name=f"Player{i}", mmr=3000, initial_mmr=3000,
                preferred_roles=["1"], main_role="1",
                glicko_rating=1200, glicko_rd=100, glicko_volatility=0.06,
                os_mu=None, os_sigma=None, discord_id=i,
            )
            for i in range(1, 6)
        ]
        entries = _make_team_entries(1, 1, [1200, 1210, 1190, 1205, 1195], 0.5, True)
        result = compute_calibration_stats(
            players, match_count=1, rating_history_entries=entries,
        )
        tc = result["team_composition"]
        assert tc["total_teams"] == 1
