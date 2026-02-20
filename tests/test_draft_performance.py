"""Performance tests for draft pool selection algorithm.

Tests beam search quality and performance compared to exhaustive search.
"""

import time
import pytest

from domain.models.player import Player
from shuffler import BalancedShuffler, DraftPoolResult


def _make_player(
    name: str,
    rating: float,
    roles: list[str] | None = None,
    discord_id: int | None = None,
) -> Player:
    """Create a Player for testing."""
    return Player(
        name=name,
        glicko_rating=rating,
        glicko_rd=100.0,
        preferred_roles=roles or ["1", "2", "3", "4", "5"],
        discord_id=discord_id or hash(name) % 100000,
    )


class TestBeamSearchQuality:
    """Tests that beam search finds solutions close to optimal."""

    def test_beam_search_quality_12_candidates(self):
        """Beam search should find a solution within tolerance of exhaustive for 12 candidates."""
        shuffler = BalancedShuffler()
        captain_a = _make_player("CaptA", 1700)
        captain_b = _make_player("CaptB", 1400)

        # 12 candidates with varied ratings
        candidates = [_make_player(f"P{i}", 1300 + i * 50) for i in range(12)]

        # Get exhaustive result (uses exhaustive search for <=12)
        exhaustive_result = shuffler.select_draft_pool(
            captain_a, captain_b, candidates
        )

        # Get beam search result directly
        beam_result = shuffler.select_draft_pool_beam(
            captain_a, captain_b, candidates
        )

        # Beam search score should be within reasonable tolerance of exhaustive
        # Note: Scores can be negative (due to RD priority bonus), so we check
        # absolute difference rather than percentage
        # Allow up to 50 points difference (typical score ranges from -100 to +500)
        score_diff = beam_result.pool_score - exhaustive_result.pool_score
        assert score_diff <= 50.0, (
            f"Beam search score {beam_result.pool_score:.1f} is {score_diff:.1f} points worse "
            f"than exhaustive {exhaustive_result.pool_score:.1f} (max allowed: 50)"
        )

    def test_beam_search_finds_good_pool_with_varied_ratings(self):
        """Beam search should handle large rating variance well."""
        shuffler = BalancedShuffler()
        captain_a = _make_player("CaptA", 1800)
        captain_b = _make_player("CaptB", 1200)

        # 15 candidates with very varied ratings
        candidates = [_make_player(f"P{i}", 1000 + i * 100) for i in range(15)]

        result = shuffler.select_draft_pool_beam(captain_a, captain_b, candidates)

        # Should select 8 players and exclude 7
        assert len(result.selected_players) == 8
        assert len(result.excluded_players) == 7

        # The pool score should be reasonable (the algorithm should compensate
        # for the captain gap by selecting appropriate players)
        assert result.pool_score < 2000.0

    def test_beam_search_respects_exclusion_counts(self):
        """Frequently excluded players should be prioritized in selection."""
        shuffler = BalancedShuffler()
        captain_a = _make_player("CaptA", 1600)
        captain_b = _make_player("CaptB", 1600)

        # 15 candidates with similar ratings
        candidates = [_make_player(f"P{i}", 1500 + i * 10) for i in range(15)]

        # Give last 3 players very high exclusion counts
        exclusion_counts = {
            candidates[12].name: 100,
            candidates[13].name: 100,
            candidates[14].name: 100,
        }

        result = shuffler.select_draft_pool_beam(
            captain_a, captain_b, candidates,
            exclusion_counts=exclusion_counts,
        )

        selected_names = {p.name for p in result.selected_players}

        # At least 2 of the frequently excluded players should be in the pool
        frequently_excluded_in_pool = sum(
            1 for name in [candidates[12].name, candidates[13].name, candidates[14].name]
            if name in selected_names
        )
        assert frequently_excluded_in_pool >= 2, (
            f"Only {frequently_excluded_in_pool} of 3 frequently excluded players "
            f"were selected, expected at least 2"
        )


class TestBeamSearchPerformance:
    """Tests that beam search is fast enough for large candidate pools."""

    def test_beam_search_15_candidates_under_3_seconds(self):
        """15 candidates should complete in under 3 seconds."""
        shuffler = BalancedShuffler()
        captain_a = _make_player("CaptA", 1600)
        captain_b = _make_player("CaptB", 1550)
        candidates = [_make_player(f"P{i}", 1400 + i * 30) for i in range(15)]

        start = time.time()
        result = shuffler.select_draft_pool_beam(captain_a, captain_b, candidates)
        elapsed = time.time() - start

        assert elapsed < 3.0, f"Beam search took {elapsed:.2f}s, expected < 3s"
        assert len(result.selected_players) == 8

    def test_beam_search_18_candidates_under_4_seconds(self):
        """18 candidates should complete in under 4 seconds."""
        shuffler = BalancedShuffler()
        captain_a = _make_player("CaptA", 1600)
        captain_b = _make_player("CaptB", 1550)
        candidates = [_make_player(f"P{i}", 1400 + i * 25) for i in range(18)]

        start = time.time()
        result = shuffler.select_draft_pool_beam(captain_a, captain_b, candidates)
        elapsed = time.time() - start

        assert elapsed < 4.0, f"Beam search took {elapsed:.2f}s, expected < 4s"
        assert len(result.selected_players) == 8

    def test_beam_search_20_candidates_under_5_seconds(self):
        """20 candidates should complete in under 5 seconds."""
        shuffler = BalancedShuffler()
        captain_a = _make_player("CaptA", 1600)
        captain_b = _make_player("CaptB", 1550)
        candidates = [_make_player(f"P{i}", 1400 + i * 20) for i in range(20)]

        start = time.time()
        result = shuffler.select_draft_pool_beam(captain_a, captain_b, candidates)
        elapsed = time.time() - start

        assert elapsed < 5.0, f"Beam search took {elapsed:.2f}s, expected < 5s"
        assert len(result.selected_players) == 8

    def test_beam_search_24_candidates_under_8_seconds(self):
        """24 candidates (max supported) should complete in under 8 seconds."""
        shuffler = BalancedShuffler()
        captain_a = _make_player("CaptA", 1600)
        captain_b = _make_player("CaptB", 1550)
        candidates = [_make_player(f"P{i}", 1400 + i * 15) for i in range(24)]

        start = time.time()
        result = shuffler.select_draft_pool_beam(captain_a, captain_b, candidates)
        elapsed = time.time() - start

        assert elapsed < 8.0, f"Beam search took {elapsed:.2f}s, expected < 8s"
        assert len(result.selected_players) == 8
        assert len(result.excluded_players) == 16


class TestSelectDraftPoolRouting:
    """Tests that select_draft_pool routes to the correct algorithm."""

    def test_8_candidates_uses_direct_scoring(self):
        """8 candidates should return immediately without enumeration."""
        shuffler = BalancedShuffler()
        captain_a = _make_player("CaptA", 1600)
        captain_b = _make_player("CaptB", 1550)
        candidates = [_make_player(f"P{i}", 1400 + i * 30) for i in range(8)]

        result = shuffler.select_draft_pool(captain_a, captain_b, candidates)

        assert len(result.selected_players) == 8
        assert len(result.excluded_players) == 0

    def test_12_candidates_uses_exhaustive(self):
        """12 candidates should use exhaustive search."""
        shuffler = BalancedShuffler()
        captain_a = _make_player("CaptA", 1600)
        captain_b = _make_player("CaptB", 1550)
        candidates = [_make_player(f"P{i}", 1400 + i * 30) for i in range(12)]

        result = shuffler.select_draft_pool(captain_a, captain_b, candidates)

        assert len(result.selected_players) == 8
        assert len(result.excluded_players) == 4

    def test_13_candidates_uses_beam_search(self):
        """13 candidates should use beam search (above threshold)."""
        shuffler = BalancedShuffler()
        captain_a = _make_player("CaptA", 1600)
        captain_b = _make_player("CaptB", 1550)
        candidates = [_make_player(f"P{i}", 1400 + i * 30) for i in range(13)]

        start = time.time()
        result = shuffler.select_draft_pool(captain_a, captain_b, candidates)
        elapsed = time.time() - start

        assert len(result.selected_players) == 8
        assert len(result.excluded_players) == 5
        # Should be fast (beam search)
        assert elapsed < 3.0, f"Expected beam search but took {elapsed:.2f}s"


class TestEdgeCases:
    """Tests for edge cases in draft pool selection."""

    def test_9_candidates_uses_exhaustive(self):
        """9 candidates should use exhaustive search (just above 8)."""
        shuffler = BalancedShuffler()
        captain_a = _make_player("CaptA", 1600)
        captain_b = _make_player("CaptB", 1550)
        candidates = [_make_player(f"P{i}", 1400 + i * 30) for i in range(9)]

        result = shuffler.select_draft_pool(captain_a, captain_b, candidates)

        assert len(result.selected_players) == 8
        assert len(result.excluded_players) == 1

    def test_beam_search_identical_ratings(self):
        """All candidates with identical ratings should return valid result."""
        shuffler = BalancedShuffler()
        captain_a = _make_player("CaptA", 1500)
        captain_b = _make_player("CaptB", 1500)

        # All candidates with identical ratings
        candidates = [
            _make_player(f"P{i}", 1500, roles=["1", "2", "3", "4", "5"])
            for i in range(15)
        ]

        result = shuffler.select_draft_pool_beam(captain_a, captain_b, candidates)

        # Should still return valid result
        assert len(result.selected_players) == 8
        assert len(result.excluded_players) == 7
        # Score should be low (all equally balanced)
        assert result.pool_score < 200.0

    def test_beam_search_with_ties(self):
        """Multiple pools with similar scores should be handled gracefully."""
        shuffler = BalancedShuffler()
        captain_a = _make_player("CaptA", 1600)
        captain_b = _make_player("CaptB", 1600)

        # Candidates with small rating differences (likely to produce ties)
        candidates = [_make_player(f"P{i}", 1500 + i) for i in range(15)]

        result = shuffler.select_draft_pool_beam(captain_a, captain_b, candidates)

        assert len(result.selected_players) == 8
        assert len(result.excluded_players) == 7
        assert isinstance(result.pool_score, float)


class TestEarlyExit:
    """Tests for beam search early exit optimization."""

    def test_early_exit_with_balanced_pool(self):
        """With a well-balanced initial pool, beam search should exit early."""
        shuffler = BalancedShuffler()
        # Identical captains
        captain_a = _make_player("CaptA", 1500, roles=["1", "2", "3", "4", "5"])
        captain_b = _make_player("CaptB", 1500, roles=["1", "2", "3", "4", "5"])

        # All identical candidates - any pool is equally balanced
        candidates = [
            _make_player(f"P{i}", 1500, roles=["1", "2", "3", "4", "5"])
            for i in range(15)
        ]

        start = time.time()
        result = shuffler.select_draft_pool_beam(captain_a, captain_b, candidates)
        elapsed = time.time() - start

        # Should exit early due to good initial pool
        # Score should be negative (due to RD priority bonus)
        assert result.pool_score < 150.0
        # Should be very fast
        assert elapsed < 1.0, f"Should exit early but took {elapsed:.2f}s"
