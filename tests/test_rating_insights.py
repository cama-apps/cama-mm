import pytest

from domain.models.player import Player
from openskill_rating_system import CamaOpenSkillSystem
from rating_system import CamaRatingSystem
from utils.rating_insights import (
    compute_calibration_stats,
    compute_player_calibration,
    get_os_win_probabilities,
)


@pytest.mark.asyncio
async def test_get_os_win_probabilities_bulk_loads_once_and_preserves_alignment():
    class MatchSource:
        def __init__(self):
            self.calls = []

        def get_os_ratings_for_matches(self, match_ids, guild_id):
            self.calls.append((match_ids, guild_id))
            return {
                11: {"team1": [(60.0, 5.0)], "team2": [(40.0, 5.0)]},
                12: {"team1": [(55.0, 5.0)], "team2": []},
            }

    class OpenSkillSystem:
        @staticmethod
        def os_predict_calibrated_win_probability(team, _opponent):
            return team[0][0] / 100

    source = MatchSource()

    probabilities = await get_os_win_probabilities(
        source,
        OpenSkillSystem(),
        [(11, 1), (12, 1), (11, 2), (None, 1)],
        guild_id=77,
    )

    assert probabilities == [0.6, None, 0.4, None]
    assert source.calls == [([11, 12], 77)]


def test_compute_calibration_stats_with_predictions_and_drift():
    players = [
        Player(
            name="Immortal",
            glicko_rating=1355,
            glicko_rd=50,
            glicko_volatility=0.08,
            wins=10,
            losses=0,
            initial_mmr=5000,
        ),
        Player(
            name="Legend",
            glicko_rating=800,
            glicko_rd=200,
            glicko_volatility=0.05,
            wins=5,
            losses=5,
            initial_mmr=4000,
        ),
        Player(
            name="Guardian",
            glicko_rating=300,
            glicko_rd=300,
            glicko_volatility=0.07,
            wins=0,
            losses=0,
            initial_mmr=None,
        ),
    ]

    match_predictions = [
        {"expected_radiant_win_prob": 0.7, "winning_team": 1},
        {"expected_radiant_win_prob": 0.3, "winning_team": 1},
    ]
    rating_history_entries = [
        {"rating_before": 1000.0, "rating": 1020.0},
        {"rating_before": 1020.0, "rating": 1010.0},
    ]

    stats = compute_calibration_stats(
        players=players,
        match_count=12,
        match_predictions=match_predictions,
        rating_history_entries=rating_history_entries,
    )

    assert stats["rating_buckets"]["Immortal"] == 1
    assert stats["rating_buckets"]["Legend"] == 1
    assert stats["rating_buckets"]["Guardian"] == 1

    assert stats["rd_tiers"]["Locked In"] == 1
    assert stats["rd_tiers"]["Developing"] == 1
    assert stats["rd_tiers"]["Fresh"] == 1

    assert stats["avg_certainty"] == pytest.approx(47.6, rel=1e-2)
    assert stats["avg_drift"] == pytest.approx(77.5, rel=1e-3)
    assert stats["median_drift"] == pytest.approx(77.5, rel=1e-3)

    prediction_quality = stats["prediction_quality"]
    assert prediction_quality["count"] == 2
    assert prediction_quality["brier"] == pytest.approx(0.29, rel=1e-6)
    assert prediction_quality["ece"] == pytest.approx(0.5, rel=1e-6)
    assert prediction_quality["accuracy"] == pytest.approx(0.5, rel=1e-6)
    assert prediction_quality["balance_rate"] == pytest.approx(0.0, rel=1e-6)
    assert prediction_quality["upset_rate"] == pytest.approx(0.5, rel=1e-6)
    assert stats["glicko_prediction_quality"] == prediction_quality
    assert stats["openskill_prediction_quality"]["count"] == 0

    rating_movement = stats["rating_movement"]
    assert rating_movement["count"] == 2
    assert rating_movement["avg_delta"] == pytest.approx(15.0, rel=1e-6)
    assert rating_movement["median_delta"] == pytest.approx(15.0, rel=1e-6)


def test_compute_calibration_stats_openskill_prediction_quality_from_history():
    players = []
    rating_history_entries = []
    for match_id in range(2):
        team1_won = match_id == 0
        for _ in range(5):
            rating_history_entries.append({
                "match_id": match_id,
                "team_number": 1,
                "won": team1_won,
                "rating_before": 1000.0,
                "rating": 1010.0,
                "os_mu_before": 60.0,
                "os_sigma_before": 4.0,
            })
            rating_history_entries.append({
                "match_id": match_id,
                "team_number": 2,
                "won": not team1_won,
                "rating_before": 1000.0,
                "rating": 990.0,
                "os_mu_before": 35.0,
                "os_sigma_before": 4.0,
            })

    stats = compute_calibration_stats(
        players=players,
        match_count=2,
        match_predictions=[],
        rating_history_entries=rating_history_entries,
    )

    os_quality = stats["openskill_prediction_quality"]
    os_system = CamaOpenSkillSystem()
    raw_prob = os_system.os_predict_win_probability(
        [(60.0, 4.0)] * 5,
        [(35.0, 4.0)] * 5,
    )
    calibrated_prob = os_system.calibrate_win_probability(raw_prob)
    expected_brier = ((calibrated_prob - 1.0) ** 2 + calibrated_prob**2) / 2

    assert os_quality["count"] == 2
    assert os_quality["brier"] == pytest.approx(expected_brier, rel=1e-6)
    assert os_quality["ece"] is not None
    assert os_quality["accuracy"] == pytest.approx(0.5, rel=1e-6)
    assert calibrated_prob < raw_prob


def test_player_calibration_treats_zero_rating_as_valid_seed_and_percentile():
    rating_system = CamaRatingSystem()
    player = Player(name="Zero", glicko_rating=0.0, initial_mmr=500)

    calibration = compute_player_calibration(
        player,
        history=[],
        rated_players=[player, Player(name="Higher", glicko_rating=100.0)],
        rating_system=rating_system,
    )

    assert calibration.drift == pytest.approx(0.0)
    assert calibration.percentile == pytest.approx(0.0)


def _history_row(rating: float, rating_before: float | None = None) -> dict:
    return {"rating": rating, "rating_before": rating_before}


def _last_5_delta(history: list[dict]) -> float | None:
    from rating_system import CamaRatingSystem
    from utils.rating_insights import compute_player_calibration

    calibration = compute_player_calibration(
        Player(name="p"), history, [], CamaRatingSystem()
    )
    return calibration.last_5_delta


class TestLast5Delta:
    """last_5_delta must span five full games of change.

    Each row's "rating" is the post-game value, so the baseline must be the
    oldest counted game's pre-game rating ("rating_before"); using its
    post-game rating undercounts by one game.
    """

    def test_spans_five_games_with_longer_history(self):
        # Newest-first: each game gained exactly 10 rating.
        history = [
            _history_row(1550, 1540),
            _history_row(1540, 1530),
            _history_row(1530, 1520),
            _history_row(1520, 1510),
            _history_row(1510, 1500),
            _history_row(1500, 1490),
        ]
        # Five games at +10 each: 1550 - 1500 (pre-game of the 5th-most-recent).
        assert _last_5_delta(history) == 50

    def test_short_history_spans_all_games(self):
        history = [
            _history_row(1530, 1520),
            _history_row(1520, 1510),
            _history_row(1510, 1500),
        ]
        # Three games at +10 each.
        assert _last_5_delta(history) == 30

    def test_legacy_rows_without_rating_before_fall_back(self):
        history = [
            _history_row(1550),
            _history_row(1540),
            _history_row(1530),
            _history_row(1520),
            _history_row(1510),
            _history_row(1500),
        ]
        # Without rating_before the old post-game baseline is the best we have.
        assert _last_5_delta(history) == 40
