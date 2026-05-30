"""Tests for RatingComparisonService scoring math.

The Brier-score / accuracy aggregation in ``_calculate_system_stats``, the
calibration bucketing in ``_get_bucket_key``, and the insufficient-data guard in
``analyze_rating_systems`` had zero coverage. These tests feed known
predicted-probability / outcome pairs and assert hand-computed expected values,
so they fail if the scoring formula changes.
"""

from __future__ import annotations

import pytest

from services.rating_comparison_service import RatingComparisonService

# --------------------------------------------------------------------------- #
# Fixtures / fakes
# --------------------------------------------------------------------------- #


class _FakeMatchRepo:
    """Returns a controllable list from get_all_matches_with_predictions."""

    def __init__(
        self,
        matches: list[dict],
        os_ratings_by_match: dict[int, dict] | None = None,
    ):
        self._matches = matches
        self._os_ratings_by_match = os_ratings_by_match or {}
        self.os_calls = []

    def get_all_matches_with_predictions(self, guild_id=None):
        return self._matches

    def get_os_ratings_for_match(self, match_id, guild_id=None):
        self.os_calls.append((match_id, guild_id))
        return self._os_ratings_by_match.get(
            match_id,
            {
                "team1": [(30.0, 8.0)] * 5,
                "team2": [(30.0, 8.0)] * 5,
            },
        )


class _FakeMatchService:
    """OpenSkill prediction stub; not exercised by the stats-math tests."""

    def __init__(self):
        self.calls = []

    def get_openskill_predictions_for_match(self, team1_ids, team2_ids, guild_id=None):
        self.calls.append((team1_ids, team2_ids, guild_id))
        return {"team1_win_prob": 0.5}


def _service(
    matches: list[dict] | None = None,
    os_ratings_by_match: dict[int, dict] | None = None,
) -> RatingComparisonService:
    fake_match_service = _FakeMatchService()
    return RatingComparisonService(
        match_repo=_FakeMatchRepo(matches or [], os_ratings_by_match),
        player_repo=None,
        match_service=fake_match_service,
    )


def _service_with_fake(
    matches: list[dict] | None = None,
    os_ratings_by_match: dict[int, dict] | None = None,
) -> tuple[RatingComparisonService, _FakeMatchRepo, _FakeMatchService]:
    fake_match_repo = _FakeMatchRepo(matches or [], os_ratings_by_match)
    fake_match_service = _FakeMatchService()
    return (
        RatingComparisonService(
            match_repo=fake_match_repo,
            player_repo=None,
            match_service=fake_match_service,
        ),
        fake_match_repo,
        fake_match_service,
    )


def _md(prob: float, radiant_won: bool) -> dict:
    """A minimal match_data row as consumed by _calculate_system_stats."""
    return {"glicko_radiant_prob": prob, "radiant_won": radiant_won}


# --------------------------------------------------------------------------- #
# _calculate_system_stats: Brier + accuracy aggregation
# --------------------------------------------------------------------------- #


def test_calculate_system_stats_brier_and_accuracy_hand_computed():
    """Brier = mean((p-outcome)^2); accuracy = fraction where favorite won.

    Four matches, worked out by hand:
      p=0.8 won  -> (0.8-1)^2 = 0.04 ; favorite (>=.5) won  -> correct
      p=0.6 lost -> (0.6-0)^2 = 0.36 ; favorite (>=.5) lost -> wrong
      p=0.3 lost -> (0.3-0)^2 = 0.09 ; underdog (<.5) lost   -> correct
      p=0.5 won  -> (0.5-1)^2 = 0.25 ; favorite (>=.5) won   -> correct
    Brier sum = 0.74 / 4 = 0.185 ; accuracy = 3 / 4 = 0.75.
    """
    match_data = [
        _md(0.8, True),
        _md(0.6, False),
        _md(0.3, False),
        _md(0.5, True),
    ]
    stats = _service()._calculate_system_stats(
        "Glicko-2", match_data, prob_key="glicko_radiant_prob"
    )

    assert stats.total_predictions == 4
    assert stats.brier_score == pytest.approx(0.185)
    assert stats.accuracy == pytest.approx(0.75)


def test_calculate_system_stats_perfect_predictions_score_zero_brier():
    """Certainty that matches the outcome every time -> Brier 0, accuracy 1."""
    match_data = [_md(1.0, True), _md(0.0, False), _md(1.0, True)]
    stats = _service()._calculate_system_stats(
        "Glicko-2", match_data, prob_key="glicko_radiant_prob"
    )
    assert stats.brier_score == pytest.approx(0.0)
    assert stats.accuracy == pytest.approx(1.0)


def test_calculate_system_stats_accuracy_counts_underdog_wins_correctly():
    """A <0.5 prediction is 'correct' only when radiant loses.

    p=0.5 is treated as favoring radiant (>=0.5), so a radiant loss at 0.5 is a
    miss. This pins the >=/< boundary used by the accuracy rule.
    """
    match_data = [_md(0.5, False), _md(0.49, False)]
    stats = _service()._calculate_system_stats(
        "Glicko-2", match_data, prob_key="glicko_radiant_prob"
    )
    # 0.5 favors radiant -> radiant lost -> wrong. 0.49 underdog -> lost -> correct.
    assert stats.accuracy == pytest.approx(0.5)


def test_calculate_system_stats_empty_returns_coinflip_defaults():
    """Zero predictions returns the documented neutral defaults."""
    stats = _service()._calculate_system_stats("Glicko-2", [], prob_key="glicko_radiant_prob")
    assert stats.total_predictions == 0
    assert stats.brier_score == pytest.approx(0.25)
    assert stats.accuracy == pytest.approx(0.5)
    assert stats.log_loss == pytest.approx(1.0)


def test_calculate_system_stats_calibration_bucket_rates():
    """Calibration buckets aggregate count, avg predicted, and actual win rate.

    Two matches land in 80-90% (one win, one loss) -> actual_rate 0.5,
    avg_predicted = (0.82 + 0.88) / 2 = 0.85. One match in 30-40% that lost ->
    actual_rate 0.0. Empty buckets report zeros.
    """
    match_data = [_md(0.82, True), _md(0.88, False), _md(0.35, False)]
    stats = _service()._calculate_system_stats(
        "Glicko-2", match_data, prob_key="glicko_radiant_prob"
    )
    buckets = stats.calibration_buckets

    hi = buckets["80-90%"]
    assert hi["count"] == 2
    assert hi["actual_wins"] == 1
    assert hi["avg_predicted"] == pytest.approx(0.85)
    assert hi["actual_rate"] == pytest.approx(0.5)

    lo = buckets["30-40%"]
    assert lo["count"] == 1
    assert lo["actual_rate"] == pytest.approx(0.0)

    empty = buckets["50-60%"]
    assert empty["count"] == 0
    assert empty["avg_predicted"] == pytest.approx(0.0)
    assert empty["actual_rate"] == pytest.approx(0.0)


# --------------------------------------------------------------------------- #
# _get_bucket_key: boundary behavior
# --------------------------------------------------------------------------- #


@pytest.mark.parametrize(
    "prob, expected",
    [
        (0.0, "0-10%"),
        (0.05, "0-10%"),
        (0.099, "0-10%"),
        (0.1, "10-20%"),  # boundary lands in the upper bucket (uses <)
        (0.25, "20-30%"),
        (0.4, "40-50%"),
        (0.49, "40-50%"),
        (0.5, "50-60%"),  # exact 0.5 is the favorite-side bucket
        (0.7, "70-80%"),
        (0.89, "80-90%"),
        (0.9, "90-100%"),
        (1.0, "90-100%"),
    ],
)
def test_get_bucket_key_boundaries(prob, expected):
    assert _service()._get_bucket_key(prob) == expected


# --------------------------------------------------------------------------- #
# analyze_rating_systems: insufficient-data guard
# --------------------------------------------------------------------------- #


def test_analyze_returns_none_when_no_matches():
    """No predictions at all -> None (the empty-list early return)."""
    assert _service([]).analyze_rating_systems(guild_id=1) is None


def test_analyze_returns_none_below_ten_complete_matches():
    """Fewer than 10 fully-populated matches -> None.

    Nine valid matches (Glicko prob + both team rosters present) still trip the
    'len(match_data) < 10' guard, so analysis is suppressed.
    """
    matches = [
        {
            "match_id": i,
            "match_date": 0,
            "winning_team": 1,
            "expected_radiant_win_prob": 0.6,
            "team1_players": [1, 2, 3, 4, 5],
            "team2_players": [6, 7, 8, 9, 10],
        }
        for i in range(9)
    ]
    assert _service(matches).analyze_rating_systems(guild_id=1) is None


def test_analyze_skips_matches_missing_prob_or_rosters_then_guards():
    """Rows lacking a Glicko prob or a roster are dropped before the count guard.

    Here only 2 of 12 rows are complete, so after filtering match_data has 2
    entries (< 10) and the guard returns None — proving the filter runs before
    the threshold check.
    """
    complete = [
        {
            "match_id": i,
            "match_date": 0,
            "winning_team": 1,
            "expected_radiant_win_prob": 0.6,
            "team1_players": [1, 2, 3, 4, 5],
            "team2_players": [6, 7, 8, 9, 10],
        }
        for i in range(2)
    ]
    missing_prob = [
        {
            "match_id": 100 + i,
            "match_date": 0,
            "winning_team": 1,
            "expected_radiant_win_prob": None,
            "team1_players": [1, 2, 3, 4, 5],
            "team2_players": [6, 7, 8, 9, 10],
        }
        for i in range(5)
    ]
    missing_roster = [
        {
            "match_id": 200 + i,
            "match_date": 0,
            "winning_team": 1,
            "expected_radiant_win_prob": 0.6,
            "team1_players": [],
            "team2_players": [],
        }
        for i in range(5)
    ]
    svc = _service(complete + missing_prob + missing_roster)
    assert svc.analyze_rating_systems(guild_id=1) is None


def test_analyze_passes_guild_id_to_historical_openskill_snapshots():
    """OpenSkill comparison must read pre-match ratings from the same guild."""
    guild_id = 12345
    matches = [
        {
            "match_id": i,
            "match_date": 0,
            "winning_team": 1 if i % 2 == 0 else 2,
            "expected_radiant_win_prob": 0.5,
            "team1_players": [1, 2, 3, 4, 5],
            "team2_players": [6, 7, 8, 9, 10],
        }
        for i in range(10)
    ]
    svc, fake_match_repo, fake_match_service = _service_with_fake(matches)

    result = svc.analyze_rating_systems(guild_id=guild_id)

    assert result is not None
    assert fake_match_repo.os_calls
    assert all(call[1] == guild_id for call in fake_match_repo.os_calls)
    assert fake_match_service.calls == []


def test_analyze_uses_historical_openskill_not_current_rating_lookup():
    """OpenSkill comparison uses rating_history snapshots, not current ratings."""
    matches = [
        {
            "match_id": i,
            "match_date": 0,
            "winning_team": 1,
            "expected_radiant_win_prob": 0.5,
            "team1_players": [1, 2, 3, 4, 5],
            "team2_players": [6, 7, 8, 9, 10],
        }
        for i in range(10)
    ]
    os_ratings = {
        i: {
            "team1": [(60.0, 4.0)] * 5,
            "team2": [(35.0, 4.0)] * 5,
        }
        for i in range(10)
    }
    svc, _fake_match_repo, fake_match_service = _service_with_fake(matches, os_ratings)

    result = svc.analyze_rating_systems(guild_id=1)

    assert result is not None
    assert fake_match_service.calls == []
    assert all(m["openskill_radiant_prob"] > 0.5 for m in result.match_data)
