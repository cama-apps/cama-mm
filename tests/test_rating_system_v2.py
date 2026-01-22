"""
Tests for Rating System V2: RD-Weighted Team Delta

These tests define the expected behavior of the new rating system:
1. High-RD players swing significantly but not absurdly
2. Win/loss is symmetric (~2:1 ratio, not 46:1)
3. Calibrated players get stable, similar deltas
4. RD² weighting distributes correctly
5. No rating inflation (total change is bounded)

Tests are written BEFORE implementation to define the contract.
"""

import math
import pytest
from glicko2 import Player

from rating_system import CamaRatingSystem
from config import CALIBRATION_RD_THRESHOLD


class TestRDWeightedTeamDelta:
    """Tests for the new RD-weighted team delta approach."""

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
    # Test 1: High-RD player swings significantly but bounded
    # =========================================================================

    def test_high_rd_player_win_bounded(self, rating_system, high_rd_on_strong_team):
        """
        High-RD player should gain significantly on win, but not +600.
        Expected: +250 to +400 range (was +598 in old system)
        """
        team1, team2 = high_rd_on_strong_team
        original_rating = team1[0][0].rating  # high_rd_low_rating

        t1_updated, t2_updated = rating_system.update_ratings_after_match(
            team1, team2, winning_team=1
        )

        high_rd_new_rating = t1_updated[0][0]
        delta = high_rd_new_rating - original_rating

        # Should be significant (>200) but not insane (<450)
        assert delta > 200, f"High-RD winner should gain significantly, got {delta}"
        assert delta < 450, f"High-RD winner gain should be bounded, got {delta}"

    def test_high_rd_player_loss_significant(self, rating_system, high_rd_on_strong_team):
        """
        High-RD player should lose significantly on loss, not just -13.
        Expected: -100 to -250 range (was -13 in old system)
        """
        team1, team2 = high_rd_on_strong_team
        original_rating = team1[0][0].rating

        t1_updated, t2_updated = rating_system.update_ratings_after_match(
            team1, team2, winning_team=2
        )

        high_rd_new_rating = t1_updated[0][0]
        delta = high_rd_new_rating - original_rating

        # Should be significant loss (more negative than -100)
        assert delta < -100, f"High-RD loser should lose significantly, got {delta}"
        # But not catastrophic
        assert delta > -300, f"High-RD loser loss should be bounded, got {delta}"

    def test_win_loss_symmetry(self, rating_system, high_rd_on_strong_team):
        """
        Win/loss ratio for high-RD player should be roughly symmetric (~2:1 to 3:1).
        Old system had 46:1 ratio which is broken.
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

        # Win/loss ratio should be reasonable (1.5:1 to 4:1)
        ratio = abs(win_delta / loss_delta) if loss_delta != 0 else float('inf')
        assert 1.0 < ratio < 5.0, f"Win/loss ratio should be ~2:1, got {ratio:.1f}:1"

    # =========================================================================
    # Test 2: Calibrated players get stable deltas
    # =========================================================================

    def test_calibrated_players_similar_deltas(self, rating_system, balanced_calibrated_teams):
        """
        In a balanced match of calibrated players, everyone should get similar deltas.
        """
        team1, team2 = balanced_calibrated_teams

        t1_updated, _ = rating_system.update_ratings_after_match(
            team1, team2, winning_team=1
        )

        deltas = []
        for i, (orig_player, _) in enumerate(team1):
            new_rating = t1_updated[i][0]
            deltas.append(new_rating - orig_player.rating)

        # All deltas should be within 50% of each other
        min_delta = min(deltas)
        max_delta = max(deltas)
        spread = max_delta - min_delta

        assert spread < max_delta * 0.6, f"Calibrated player deltas too spread: {deltas}"

    def test_calibrated_match_reasonable_magnitude(self, rating_system, balanced_calibrated_teams):
        """
        Balanced calibrated match should produce reasonable per-player deltas (~10-30).
        """
        team1, team2 = balanced_calibrated_teams

        t1_updated, _ = rating_system.update_ratings_after_match(
            team1, team2, winning_team=1
        )

        for i, (orig_player, _) in enumerate(team1):
            new_rating = t1_updated[i][0]
            delta = new_rating - orig_player.rating

            assert 5 < delta < 50, f"Calibrated winner delta should be moderate, got {delta}"

    # =========================================================================
    # Test 3: RD² weighting distributes correctly
    # =========================================================================

    def test_higher_rd_gets_larger_share(self, rating_system, high_rd_on_strong_team):
        """
        Players with higher RD should get larger share of team delta.
        """
        team1, team2 = high_rd_on_strong_team

        t1_updated, _ = rating_system.update_ratings_after_match(
            team1, team2, winning_team=1
        )

        # Get deltas
        deltas = []
        for i, (orig_player, _) in enumerate(team1):
            new_rating = t1_updated[i][0]
            deltas.append((team1[i][0].rd, new_rating - orig_player.rating))

        # Sort by RD
        deltas.sort(key=lambda x: x[0], reverse=True)

        # Highest RD should have highest delta
        highest_rd_delta = deltas[0][1]
        lowest_rd_delta = deltas[-1][1]

        assert highest_rd_delta > lowest_rd_delta, \
            f"Higher RD should get larger delta: RD={deltas[0][0]} got {highest_rd_delta}, RD={deltas[-1][0]} got {lowest_rd_delta}"

    def test_rd_squared_proportionality(self, rating_system, two_new_players_team):
        """
        Deltas should be roughly proportional to RD².
        """
        team1, team2 = two_new_players_team

        t1_updated, _ = rating_system.update_ratings_after_match(
            team1, team2, winning_team=1
        )

        # Calculate weights and deltas
        total_var = sum(p.rd**2 for p, _ in team1)
        results = []

        for i, (orig_player, pid) in enumerate(team1):
            expected_weight = orig_player.rd**2 / total_var
            actual_delta = t1_updated[i][0] - orig_player.rating
            results.append({
                'pid': pid,
                'rd': orig_player.rd,
                'expected_weight': expected_weight,
                'delta': actual_delta,
            })

        # Check that relative deltas match relative weights (within 20%)
        total_delta = sum(r['delta'] for r in results)
        for r in results:
            actual_share = r['delta'] / total_delta if total_delta != 0 else 0
            expected_share = r['expected_weight']

            # Allow 30% tolerance for implementation differences
            assert abs(actual_share - expected_share) < 0.30, \
                f"Player {r['pid']} share mismatch: expected {expected_share:.2f}, got {actual_share:.2f}"

    # =========================================================================
    # Test 4: No excessive rating inflation
    # =========================================================================

    def test_total_rating_change_bounded(self, rating_system, high_rd_on_strong_team):
        """
        Total rating created should not be excessive.
        Old system created +693 for winners vs -107 for losers (6.5x inflation).
        New system should be better (Glicko-2 inherently creates more for upsets).
        """
        team1, team2 = high_rd_on_strong_team

        t1_updated, t2_updated = rating_system.update_ratings_after_match(
            team1, team2, winning_team=1
        )

        # Calculate total changes
        t1_total = sum(t1_updated[i][0] - team1[i][0].rating for i in range(5))
        t2_total = sum(t2_updated[i][0] - team2[i][0].rating for i in range(5))

        # Total created should be within 5x of total destroyed
        # (Glicko-2 creates more rating for upsets, but not 6.5x like old system)
        ratio = abs(t1_total / t2_total) if t2_total != 0 else float('inf')
        assert ratio < 5.0, f"Rating inflation too high: created {t1_total}, destroyed {abs(t2_total)}, ratio {ratio:.1f}"

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

    def test_extreme_rd_skew(self, rating_system):
        """
        One 350 RD player vs four 50 RD players.
        High-RD player should get ~98% of weight but system should still work.
        """
        team1 = [
            (Player(rating=1000, rd=350, vol=0.06), "extreme_rd"),
            (Player(rating=1200, rd=50, vol=0.06), "low_rd_1"),
            (Player(rating=1200, rd=50, vol=0.06), "low_rd_2"),
            (Player(rating=1200, rd=50, vol=0.06), "low_rd_3"),
            (Player(rating=1200, rd=50, vol=0.06), "low_rd_4"),
        ]
        team2 = [
            (Player(rating=1150, rd=80, vol=0.06), "enemy_1"),
            (Player(rating=1150, rd=80, vol=0.06), "enemy_2"),
            (Player(rating=1150, rd=80, vol=0.06), "enemy_3"),
            (Player(rating=1150, rd=80, vol=0.06), "enemy_4"),
            (Player(rating=1150, rd=80, vol=0.06), "enemy_5"),
        ]

        # Should not raise
        t1_updated, t2_updated = rating_system.update_ratings_after_match(
            team1, team2, winning_team=1
        )

        # Extreme RD player should get vast majority of delta
        extreme_rd_delta = t1_updated[0][0] - team1[0][0].rating
        low_rd_delta = t1_updated[1][0] - team1[1][0].rating

        # 350² / (350² + 4*50²) = 122500 / 132500 ≈ 0.92
        assert extreme_rd_delta > low_rd_delta * 5, \
            f"Extreme RD player should dominate: {extreme_rd_delta} vs {low_rd_delta}"

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

    def test_identical_rd_equal_distribution(self, rating_system):
        """
        When all players have identical RD, deltas should be equal.
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
            f"Equal RD should give equal deltas: {deltas}"


class TestV2FixesOldBugs:
    """
    These tests verify that V2 fixes the problems with the old system.
    """

    @pytest.fixture
    def rating_system(self):
        return CamaRatingSystem()

    def test_v2_has_symmetric_win_loss(self, rating_system):
        """
        V2 should have symmetric win/loss ratio (~2:1) instead of old 46:1.
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

        # Win case
        team1_w = [(Player(rating=p.rating, rd=p.rd, vol=p.vol), pid) for p, pid in team1]
        team2_w = [(Player(rating=p.rating, rd=p.rd, vol=p.vol), pid) for p, pid in team2]
        t1_w, _ = rating_system.update_ratings_after_match(team1_w, team2_w, winning_team=1)
        win_delta = t1_w[0][0] - 413

        # Loss case
        team1_l = [(Player(rating=p.rating, rd=p.rd, vol=p.vol), pid) for p, pid in team1]
        team2_l = [(Player(rating=p.rating, rd=p.rd, vol=p.vol), pid) for p, pid in team2]
        t1_l, _ = rating_system.update_ratings_after_match(team1_l, team2_l, winning_team=2)
        loss_delta = t1_l[0][0] - 413

        ratio = abs(win_delta / loss_delta) if loss_delta != 0 else float('inf')

        # V2 should have ratio < 5 (old system had 46:1)
        assert ratio < 5.0, f"V2 should have symmetric win/loss, got {ratio:.1f}:1"
        # And win delta should be significant but bounded
        assert 200 < win_delta < 450, f"Win delta should be bounded, got {win_delta}"
        # And loss delta should be significant (not -13 like old system)
        assert loss_delta < -100, f"Loss delta should be significant, got {loss_delta}"
