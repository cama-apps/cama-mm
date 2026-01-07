"""
Helpers for computing rating system insights.
"""

from __future__ import annotations

import statistics
from typing import Iterable

from domain.models.player import Player
from rating_system import CamaRatingSystem

RATING_BUCKETS = [
    ("Immortal", 1355),
    ("Divine", 1155),
    ("Ancient", 962),
    ("Legend", 770),
    ("Archon", 578),
    ("Crusader", 385),
    ("Guardian", 192),
    ("Herald", 0),
]


def _mean(values: Iterable[float]) -> float | None:
    values = list(values)
    if not values:
        return None
    return statistics.mean(values)


def _median(values: Iterable[float]) -> float | None:
    values = list(values)
    if not values:
        return None
    return statistics.median(values)


def compute_calibration_stats(
    players: list[Player],
    rating_system: CamaRatingSystem,
    match_count: int = 0,
    match_predictions: list[dict] | None = None,
    rating_history_entries: list[dict] | None = None,
) -> dict:
    rated_players = [p for p in players if p.glicko_rating is not None]
    rating_values = [p.glicko_rating for p in rated_players if p.glicko_rating is not None]
    rd_values = [p.glicko_rd if p.glicko_rd is not None else 350.0 for p in rated_players]

    rating_buckets = {label: 0 for label, _ in RATING_BUCKETS}
    for rating in rating_values:
        for label, threshold in RATING_BUCKETS:
            if rating >= threshold:
                rating_buckets[label] += 1
                break

    rd_tiers = {
        "Locked In": 0,
        "Settling": 0,
        "Developing": 0,
        "Fresh": 0,
    }
    for rd in rd_values:
        if rd <= 75:
            rd_tiers["Locked In"] += 1
        elif rd <= 150:
            rd_tiers["Settling"] += 1
        elif rd <= 250:
            rd_tiers["Developing"] += 1
        else:
            rd_tiers["Fresh"] += 1

    avg_rd = _mean(rd_values)
    avg_uncertainty = (
        rating_system.get_rating_uncertainty_percentage(avg_rd) if avg_rd is not None else None
    )

    total_games = [p.wins + p.losses for p in players]
    avg_games = _mean(total_games)

    top_rated = sorted(rated_players, key=lambda p: p.glicko_rating or 0, reverse=True)[:3]
    lowest_rated = sorted(rated_players, key=lambda p: p.glicko_rating or 0)[:3]
    most_calibrated = sorted(
        rated_players, key=lambda p: p.glicko_rd if p.glicko_rd is not None else 350
    )[:3]
    least_calibrated = sorted(
        rated_players,
        key=lambda p: p.glicko_rd if p.glicko_rd is not None else 350,
        reverse=True,
    )[:3]
    highest_volatility = sorted(
        rated_players,
        key=lambda p: p.glicko_volatility if p.glicko_volatility is not None else 0.06,
        reverse=True,
    )[:3]
    most_experienced = sorted(players, key=lambda p: p.wins + p.losses, reverse=True)[:3]

    drifts = []
    for player in rated_players:
        if player.initial_mmr is None or player.glicko_rating is None:
            continue
        seed_rating = rating_system.mmr_to_rating(player.initial_mmr)
        drift = player.glicko_rating - seed_rating
        drifts.append((player, drift))
    drift_values = [drift for _, drift in drifts]
    drifts_sorted = sorted(drifts, key=lambda x: x[1], reverse=True)

    prediction_quality = _compute_prediction_quality(match_predictions or [])
    rating_movement = _compute_rating_movement(rating_history_entries or [])

    return {
        "total_players": len(players),
        "match_count": match_count,
        "rated_players": len(rated_players),
        "avg_games": avg_games,
        "rating_buckets": rating_buckets,
        "avg_rating": _mean(rating_values),
        "median_rating": _median(rating_values),
        "rd_tiers": rd_tiers,
        "avg_uncertainty": avg_uncertainty,
        "top_rated": top_rated,
        "lowest_rated": lowest_rated,
        "most_calibrated": most_calibrated,
        "least_calibrated": least_calibrated,
        "highest_volatility": highest_volatility,
        "most_experienced": most_experienced,
        "avg_drift": _mean(drift_values),
        "median_drift": _median(drift_values),
        "biggest_gainers": drifts_sorted[:3],
        "biggest_drops": list(reversed(drifts_sorted[-3:])),
        "prediction_quality": prediction_quality,
        "rating_movement": rating_movement,
    }


def _compute_prediction_quality(match_predictions: list[dict]) -> dict:
    brier_scores = []
    correct = 0
    balanced = 0
    upset = 0
    upset_eligible = 0

    for entry in match_predictions:
        prob = entry.get("expected_radiant_win_prob")
        winning_team = entry.get("winning_team")
        if prob is None or winning_team not in (1, 2):
            continue
        actual = 1 if winning_team == 1 else 0
        brier_scores.append((prob - actual) ** 2)
        predicted = 1 if prob >= 0.5 else 0
        if predicted == actual:
            correct += 1
        if 0.45 <= prob <= 0.55:
            balanced += 1
        if prob >= 0.6 or prob <= 0.4:
            upset_eligible += 1
            if (prob >= 0.6 and actual == 0) or (prob <= 0.4 and actual == 1):
                upset += 1

    count = len(brier_scores)
    return {
        "count": count,
        "brier": _mean(brier_scores),
        "accuracy": (correct / count) if count else None,
        "balance_rate": (balanced / count) if count else None,
        "upset_rate": (upset / upset_eligible) if upset_eligible else None,
    }


def _compute_rating_movement(rating_history_entries: list[dict]) -> dict:
    deltas = []
    for entry in rating_history_entries:
        before = entry.get("rating_before")
        after = entry.get("rating")
        if before is None or after is None:
            continue
        deltas.append(abs(after - before))

    return {
        "count": len(deltas),
        "avg_delta": _mean(deltas),
        "median_delta": _median(deltas),
    }
