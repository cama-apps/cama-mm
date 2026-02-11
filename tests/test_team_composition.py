"""Tests for team composition winrate analysis in rating_insights."""

import pytest

from utils.rating_insights import (
    _classify_team_archetype,
    _compute_team_composition_stats,
    compute_calibration_stats,
)
from domain.models.player import Player


class TestClassifyTeamArchetype:
    """Tests for _classify_team_archetype using population std dev."""

    def test_balanced_similar_ratings(self):
        assert _classify_team_archetype([1200, 1210, 1190, 1205, 1195]) == "balanced"

    def test_star_carry_one_high_outlier(self):
        assert _classify_team_archetype([1500, 1100, 1120, 1080, 1110]) == "star-carry"

    def test_anchor_drag_one_low_outlier(self):
        assert _classify_team_archetype([1400, 1380, 1420, 1390, 1050]) == "anchor-drag"

    def test_polarized_both_outliers(self):
        assert _classify_team_archetype([1600, 1200, 1210, 1190, 800]) == "polarized"

    def test_zero_spread_returns_balanced(self):
        assert _classify_team_archetype([1200, 1200, 1200, 1200, 1200]) == "balanced"

    def test_near_zero_spread_guard(self):
        # pstdev < 1 should return balanced
        assert _classify_team_archetype([1200, 1200, 1200, 1200, 1201]) == "balanced"

    def test_single_player_returns_balanced(self):
        assert _classify_team_archetype([1200]) == "balanced"

    def test_empty_returns_balanced(self):
        assert _classify_team_archetype([]) == "balanced"


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
    """Tests for _compute_team_composition_stats."""

    def test_empty_input(self):
        result = _compute_team_composition_stats([])
        assert result["categories"] == []
        assert result["total_teams"] == 0

    def test_teams_with_fewer_than_5_players_excluded(self):
        entries = [
            _make_rating_history_entry(1, 1, 1200, 0.5, True),
            _make_rating_history_entry(1, 1, 1210, 0.5, True),
            _make_rating_history_entry(1, 1, 1190, 0.5, True),
        ]
        result = _compute_team_composition_stats(entries)
        assert result["total_teams"] == 0

    def test_single_balanced_team(self):
        entries = _make_team_entries(1, 1, [1200, 1210, 1190, 1205, 1195], 0.5, True)
        result = _compute_team_composition_stats(entries)
        # Single team won't meet >= 3 threshold for display
        assert result["total_teams"] == 1
        assert result["categories"] == []  # filtered out (< 3 teams)

    def test_categories_filtered_below_3_teams(self):
        """Categories with < 3 teams should not appear in display results."""
        # Create 2 balanced teams (not enough) and 3 star-carry teams
        entries = []
        for i in range(2):
            entries.extend(_make_team_entries(i, 1, [1200, 1210, 1190, 1205, 1195], 0.5, True))
        for i in range(2, 5):
            entries.extend(_make_team_entries(i, 1, [1500, 1100, 1120, 1080, 1110], 0.5, True))
        result = _compute_team_composition_stats(entries)
        assert result["total_teams"] == 5
        cat_names = [c["name"] for c in result["categories"]]
        # Star Carry with 3 teams should appear, Balanced with 2 should not
        assert "Star Carry" in cat_names
        assert "Balanced" not in cat_names

    def test_overperformance_calculation(self):
        """Overperformance = actual_winrate - avg_expected_win_prob."""
        entries = []
        # 3 teams with same archetype: 2 wins, 1 loss, all expected at 0.4
        for i in range(3):
            won = i < 2  # 2 wins, 1 loss
            entries.extend(
                _make_team_entries(i, 1, [1200, 1210, 1190, 1205, 1195], 0.4, won)
            )
        result = _compute_team_composition_stats(entries)
        assert result["total_teams"] == 3
        cats = result["categories"]
        assert len(cats) == 1
        cat = cats[0]
        assert cat["name"] == "Balanced"
        assert cat["wins"] == 2
        assert cat["total"] == 3
        assert abs(cat["winrate"] - 2 / 3) < 0.001
        assert abs(cat["avg_expected"] - 0.4) < 0.001
        # overperformance = 2/3 - 0.4 = 0.2667
        assert abs(cat["overperformance"] - (2 / 3 - 0.4)) < 0.001

    def test_sorted_by_overperformance_descending(self):
        """Categories should be sorted by overperformance, highest first."""
        entries = []
        # Category A: balanced - wins all, expected 0.3
        for i in range(3):
            entries.extend(
                _make_team_entries(i, 1, [1200, 1210, 1190, 1205, 1195], 0.3, True)
            )
        # Category B: star-carry - loses all, expected 0.7
        for i in range(3, 6):
            entries.extend(
                _make_team_entries(i, 1, [1500, 1100, 1120, 1080, 1110], 0.7, False)
            )
        result = _compute_team_composition_stats(entries)
        cats = result["categories"]
        assert len(cats) == 2
        assert cats[0]["name"] == "Balanced"
        assert cats[1]["name"] == "Star Carry"
        assert cats[0]["overperformance"] > cats[1]["overperformance"]

    def test_expected_win_prob_extracted(self):
        """expected_team_win_prob should be correctly used from entries."""
        entries = []
        for i in range(3):
            entries.extend(
                _make_team_entries(i, 1, [1200, 1200, 1200, 1200, 1200], 0.65, i % 2 == 0)
            )
        result = _compute_team_composition_stats(entries)
        assert len(result["categories"]) == 1
        cat = result["categories"][0]
        assert cat["name"] == "Balanced"
        assert abs(cat["avg_expected"] - 0.65) < 0.001

    def test_no_spread_thresholds_in_result(self):
        """Result should not contain spread_thresholds (archetype-only grouping)."""
        entries = []
        for i in range(3):
            entries.extend(
                _make_team_entries(i, 1, [1200, 1210, 1190, 1205, 1195], 0.5, True)
            )
        result = _compute_team_composition_stats(entries)
        assert "spread_thresholds" not in result

    def test_mixed_teams_both_sides_of_match(self):
        """Both teams in a match should be analyzed independently."""
        entries = []
        # Match 1: team 1 (balanced) wins, team 2 (star-carry) loses
        for i in range(5):
            entries.extend(
                _make_team_entries(i, 1, [1200, 1210, 1190, 1205, 1195], 0.5, True)
            )
            entries.extend(
                _make_team_entries(i, 2, [1500, 1100, 1120, 1080, 1110], 0.5, False)
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
        assert "categories" in tc
        assert "total_teams" in tc
        assert isinstance(tc["categories"], list)

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
