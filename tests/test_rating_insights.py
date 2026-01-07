import pytest

from domain.models.player import Player
from rating_system import CamaRatingSystem
from utils.rating_insights import compute_calibration_stats


def test_compute_calibration_stats_with_predictions_and_drift():
    rating_system = CamaRatingSystem()
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
        rating_system=rating_system,
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

    assert stats["avg_uncertainty"] == pytest.approx(52.4, rel=1e-2)
    assert stats["avg_drift"] == pytest.approx(-47.5, rel=1e-3)
    assert stats["median_drift"] == pytest.approx(-47.5, rel=1e-3)

    prediction_quality = stats["prediction_quality"]
    assert prediction_quality["count"] == 2
    assert prediction_quality["accuracy"] == pytest.approx(0.5, rel=1e-6)
    assert prediction_quality["balance_rate"] == pytest.approx(0.0, rel=1e-6)
    assert prediction_quality["upset_rate"] == pytest.approx(0.5, rel=1e-6)

    rating_movement = stats["rating_movement"]
    assert rating_movement["count"] == 2
    assert rating_movement["avg_delta"] == pytest.approx(15.0, rel=1e-6)
    assert rating_movement["median_delta"] == pytest.approx(15.0, rel=1e-6)
