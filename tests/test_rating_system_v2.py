"""
Tests for Rating System V3: Pure Individual Glicko + Cap

These tests define the expected behavior of the V3 rating system:
1. Each player computes their own Glicko-2 update vs opponent team aggregate
2. Rating swings are capped at ±MAX_RATING_SWING_PER_GAME (400)
3. High-RD players get larger swings naturally (Glicko-2 behavior)
4. RD always decreases after matches (never increases)
5. Calibrated players get small, stable deltas (~10-20)
"""

import math
import pytest
from glicko2 import Player

from rating_system import CamaRatingSystem
from config import CALIBRATION_RD_THRESHOLD, MAX_RATING_SWING_PER_GAME


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
