"""Tests for balanced draft pool selection algorithm."""

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


class TestScoreDraftPool:
    """Tests for _score_draft_pool helper."""

    def test_returns_single_score(self):
        shuffler = BalancedShuffler()
        captain_a = _make_player("CaptA", 1600)
        captain_b = _make_player("CaptB", 1600)
        pool = [_make_player(f"P{i}", 1500 + i * 20) for i in range(8)]

        score = shuffler._score_draft_pool(captain_a, captain_b, pool)

        assert isinstance(score, float)

    def test_symmetric_captains_and_pool(self):
        """With identical captains and pool, score should be near zero."""
        shuffler = BalancedShuffler()
        captain_a = _make_player("CaptA", 1600, roles=["1", "2", "3", "4", "5"])
        captain_b = _make_player("CaptB", 1600, roles=["1", "2", "3", "4", "5"])

        pool = [
            _make_player(f"P{i}", 1500, roles=["1", "2", "3", "4", "5"])
            for i in range(8)
        ]

        score = shuffler._score_draft_pool(captain_a, captain_b, pool)

        # With identical captains and identical pool players, the best split
        # should produce very balanced teams (score can be negative due to rd_priority bonus)
        assert abs(score) < 150.0

    def test_evaluates_all_splits(self):
        """Score should reflect the best possible split, not a greedy heuristic."""
        shuffler = BalancedShuffler()
        # Give captains very different ratings
        captain_a = _make_player("CaptA", 1800)
        captain_b = _make_player("CaptB", 1200)

        # Pool with varied ratings — the algorithm should find a split
        # that compensates for the captain rating difference
        pool = [_make_player(f"P{i}", 1300 + i * 80) for i in range(8)]

        score = shuffler._score_draft_pool(captain_a, captain_b, pool)

        # The best split should produce a reasonably balanced game
        # despite the 600-point captain gap (by giving stronger players to captain_b)
        assert score < 1000.0


class TestSelectDraftPool:
    """Tests for select_draft_pool method."""

    def test_exactly_8_candidates_returns_all(self):
        """With exactly 8 candidates, all should be selected."""
        shuffler = BalancedShuffler()
        captain_a = _make_player("CaptA", 1600)
        captain_b = _make_player("CaptB", 1550)
        candidates = [_make_player(f"P{i}", 1400 + i * 30) for i in range(8)]

        result = shuffler.select_draft_pool(captain_a, captain_b, candidates)

        assert len(result.selected_players) == 8
        assert len(result.excluded_players) == 0
        assert set(p.name for p in result.selected_players) == set(p.name for p in candidates)

    def test_fewer_than_8_raises_valueerror(self):
        """Should raise ValueError with fewer than 8 candidates."""
        shuffler = BalancedShuffler()
        captain_a = _make_player("CaptA", 1600)
        captain_b = _make_player("CaptB", 1550)
        candidates = [_make_player(f"P{i}", 1400 + i * 30) for i in range(7)]

        with pytest.raises(ValueError, match="at least 8"):
            shuffler.select_draft_pool(captain_a, captain_b, candidates)

    def test_10_candidates_selects_8_excludes_2(self):
        """With 10 candidates, should select 8 and exclude 2."""
        shuffler = BalancedShuffler()
        captain_a = _make_player("CaptA", 1600)
        captain_b = _make_player("CaptB", 1550)
        candidates = [_make_player(f"P{i}", 1300 + i * 40) for i in range(10)]

        result = shuffler.select_draft_pool(captain_a, captain_b, candidates)

        assert len(result.selected_players) == 8
        assert len(result.excluded_players) == 2
        # All players accounted for
        all_names = {p.name for p in result.selected_players} | {p.name for p in result.excluded_players}
        assert all_names == {p.name for p in candidates}

    def test_12_candidates_selects_8_excludes_4(self):
        """With 12 candidates, should select 8 and exclude 4."""
        shuffler = BalancedShuffler()
        captain_a = _make_player("CaptA", 1600)
        captain_b = _make_player("CaptB", 1550)
        candidates = [_make_player(f"P{i}", 1300 + i * 30) for i in range(12)]

        result = shuffler.select_draft_pool(captain_a, captain_b, candidates)

        assert len(result.selected_players) == 8
        assert len(result.excluded_players) == 4
        all_names = {p.name for p in result.selected_players} | {p.name for p in result.excluded_players}
        assert all_names == {p.name for p in candidates}

    def test_pool_score_reflects_best_split(self):
        """pool_score should equal best split score + penalties."""
        shuffler = BalancedShuffler()
        captain_a = _make_player("CaptA", 1700)
        captain_b = _make_player("CaptB", 1400)
        # Exactly 8 candidates (no exclusion penalty)
        candidates = [_make_player(f"P{i}", 1300 + i * 50) for i in range(8)]

        result = shuffler.select_draft_pool(captain_a, captain_b, candidates)

        # With no exclusion and no recent match penalties,
        # pool_score should equal the best split score
        expected = shuffler._score_draft_pool(captain_a, captain_b, candidates)
        assert result.pool_score == pytest.approx(expected, abs=0.01)

    def test_exclusion_penalty_keeps_frequently_excluded_in_pool(self):
        """Players with high exclusion counts should be preferentially included."""
        shuffler = BalancedShuffler()
        captain_a = _make_player("CaptA", 1600)
        captain_b = _make_player("CaptB", 1600)

        # 10 candidates: 8 normal + 2 with high exclusion counts
        candidates = [_make_player(f"P{i}", 1500 + i * 10) for i in range(10)]

        # Give last two players very high exclusion counts
        exclusion_counts = {
            candidates[8].name: 100,
            candidates[9].name: 100,
        }

        result = shuffler.select_draft_pool(
            captain_a, captain_b, candidates,
            exclusion_counts=exclusion_counts,
        )

        selected_names = {p.name for p in result.selected_players}

        # The frequently excluded players should be in the pool
        # (excluding them would incur a large penalty)
        assert candidates[8].name in selected_names
        assert candidates[9].name in selected_names

    def test_recent_match_penalty_affects_selection(self):
        """Recent match participants should be penalized when selected."""
        shuffler = BalancedShuffler()
        captain_a = _make_player("CaptA", 1600)
        captain_b = _make_player("CaptB", 1600)

        # 10 candidates with similar ratings
        candidates = [_make_player(f"P{i}", 1500) for i in range(10)]

        # All candidates are recent match participants
        recent_names = {p.name for p in candidates}

        result_with_recent = shuffler.select_draft_pool(
            captain_a, captain_b, candidates,
            recent_match_names=recent_names,
        )

        result_without_recent = shuffler.select_draft_pool(
            captain_a, captain_b, candidates,
        )

        # Score should be higher when all selected players have recent match penalty
        assert result_with_recent.pool_score >= result_without_recent.pool_score

    def test_result_is_draft_pool_result(self):
        """Should return a DraftPoolResult dataclass."""
        shuffler = BalancedShuffler()
        captain_a = _make_player("CaptA", 1600)
        captain_b = _make_player("CaptB", 1550)
        candidates = [_make_player(f"P{i}", 1400 + i * 30) for i in range(8)]

        result = shuffler.select_draft_pool(captain_a, captain_b, candidates)

        assert isinstance(result, DraftPoolResult)
        assert isinstance(result.pool_score, float)
