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
        seed_rating = CamaRatingSystem().mmr_to_rating(player.initial_mmr)
        drift = player.glicko_rating - seed_rating
        drifts.append((player, drift))
    drift_values = [drift for _, drift in drifts]
    drifts_sorted = sorted(drifts, key=lambda x: x[1], reverse=True)

    prediction_quality = _compute_prediction_quality(match_predictions or [])
    rating_movement = _compute_rating_movement(rating_history_entries or [])
    side_balance = _compute_side_balance(match_predictions or [])
    rating_stability = _compute_rating_stability(rating_history_entries or [])
    team_composition = _compute_team_composition_stats(rating_history_entries or [])

    # Calculate average certainty (inverse of uncertainty)
    avg_rd = _mean(rd_values)
    avg_certainty = rd_to_certainty(avg_rd) if avg_rd is not None else None

    return {
        "total_players": len(players),
        "match_count": match_count,
        "rated_players": len(rated_players),
        "avg_games": avg_games,
        "rating_buckets": rating_buckets,
        "avg_rating": _mean(rating_values),
        "median_rating": _median(rating_values),
        "rd_tiers": rd_tiers,
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
        "side_balance": side_balance,
        "rating_stability": rating_stability,
        "team_composition": team_composition,
        "avg_certainty": avg_certainty,
        "avg_rd": avg_rd,
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


def _compute_side_balance(match_predictions: list[dict]) -> dict:
    """Compute Radiant vs Dire win statistics."""
    radiant_wins = 0
    dire_wins = 0

    for entry in match_predictions:
        winning_team = entry.get("winning_team")
        if winning_team == 1:
            radiant_wins += 1
        elif winning_team == 2:
            dire_wins += 1

    total = radiant_wins + dire_wins
    return {
        "radiant_wins": radiant_wins,
        "dire_wins": dire_wins,
        "total": total,
        "radiant_rate": (radiant_wins / total) if total else None,
        "dire_rate": (dire_wins / total) if total else None,
    }


def rd_to_certainty(rd: float) -> float:
    """Convert RD to certainty percentage. Higher = more certain."""
    # RD 350 = 0% certain, RD 0 = 100% certain
    uncertainty = min(100, (rd / 350.0) * 100)
    return 100 - uncertainty


def get_rd_tier_name(rd: float) -> str:
    """Get calibration tier name from RD value."""
    if rd <= 75:
        return "Locked In"
    elif rd <= 150:
        return "Settling"
    elif rd <= 250:
        return "Developing"
    else:
        return "Fresh"


def _gini_coefficient(values: list[float]) -> float:
    """Compute the Gini coefficient for a list of values.

    Returns 0.0 for equal distributions, approaching 1.0 for maximum inequality.
    """
    n = len(values)
    if n < 2:
        return 0.0
    mean_val = statistics.mean(values)
    if mean_val <= 0:
        return 0.0
    abs_diffs = sum(abs(a - b) for a in values for b in values)
    return abs_diffs / (2 * n * n * mean_val)


def _pearson_r(xs: list[float], ys: list[float]) -> float | None:
    """Compute the Pearson correlation coefficient between two sequences.

    Returns None if fewer than 3 data points or if either sequence is constant.
    """
    n = len(xs)
    if n < 3:
        return None
    mean_x = statistics.mean(xs)
    mean_y = statistics.mean(ys)
    num = sum((x - mean_x) * (y - mean_y) for x, y in zip(xs, ys))
    den_x = sum((x - mean_x) ** 2 for x in xs) ** 0.5
    den_y = sum((y - mean_y) ** 2 for y in ys) ** 0.5
    if den_x == 0 or den_y == 0:
        return None
    return num / (den_x * den_y)


def _compute_team_composition_stats(rating_history_entries: list[dict]) -> dict:
    """Analyze how team rating spread (Gini) correlates with winrate.

    Groups entries by (match_id, team_number), computes Gini coefficient for
    each team's ratings, splits into two halves by index-based median, and
    computes Pearson correlation between Gini and overperformance.
    """
    if not rating_history_entries:
        return {"halves": [], "total_teams": 0, "gini_correlation": None}

    # Group by (match_id, team_number)
    teams: dict[tuple, list[dict]] = {}
    for entry in rating_history_entries:
        match_id = entry.get("match_id")
        team_num = entry.get("team_number")
        if match_id is None or team_num is None:
            continue
        key = (match_id, team_num)
        teams.setdefault(key, []).append(entry)

    # Build team data — only include complete 5-player teams
    team_data = []
    for key, entries in teams.items():
        if len(entries) != 5:
            continue
        ratings = [e["rating_before"] for e in entries if e.get("rating_before") is not None]
        if len(ratings) != 5:
            continue
        expected = entries[0].get("expected_team_win_prob")
        won = entries[0].get("won")
        if expected is None or won is None:
            continue
        gini = _gini_coefficient(ratings)
        overperf = int(bool(won)) - expected
        team_data.append({
            "gini": gini,
            "expected": expected,
            "won": bool(won),
            "overperf": overperf,
        })

    total_teams = len(team_data)
    if total_teams == 0:
        return {"halves": [], "total_teams": 0, "gini_correlation": None}

    # Pearson correlation between Gini and overperformance
    gini_values = [t["gini"] for t in team_data]
    overperf_values = [t["overperf"] for t in team_data]
    r = _pearson_r(gini_values, overperf_values)

    # Index-based median split: sort by Gini, first n//2 are "Similar ratings"
    if total_teams < 6:
        return {"halves": [], "total_teams": total_teams, "gini_correlation": r}

    sorted_teams = sorted(team_data, key=lambda t: t["gini"])
    mid = total_teams // 2
    lower_half = sorted_teams[:mid]
    upper_half = sorted_teams[mid:]

    def _aggregate(items: list[dict], name: str) -> dict:
        wins = sum(1 for t in items if t["won"])
        total = len(items)
        winrate = wins / total if total > 0 else 0.0
        avg_expected = statistics.mean(t["expected"] for t in items)
        avg_gini = statistics.mean(t["gini"] for t in items)
        return {
            "name": name,
            "wins": wins,
            "total": total,
            "winrate": winrate,
            "avg_expected": avg_expected,
            "overperformance": winrate - avg_expected,
            "avg_gini": avg_gini,
        }

    halves = [
        _aggregate(lower_half, "Similar ratings"),
        _aggregate(upper_half, "Mixed ratings"),
    ]

    return {
        "halves": halves,
        "total_teams": total_teams,
        "gini_correlation": r,
    }


def _compute_rating_stability(rating_history_entries: list[dict]) -> dict:
    """
    Compare rating changes between calibrated vs uncalibrated players.

    Uses RD at time of match:
    - Calibrated: RD ≤150 (Locked In + Settling, 57%+ certain)
    - Uncalibrated: RD >150 (Developing + Fresh, <57% certain)

    Calibrated players should have smaller rating swings if system is working.
    """
    calibrated_deltas = []  # RD ≤150
    uncalibrated_deltas = []  # RD >150

    for entry in rating_history_entries:
        before = entry.get("rating_before")
        after = entry.get("rating")
        rd_before = entry.get("rd_before")

        if before is None or after is None or rd_before is None:
            continue

        delta = abs(after - before)

        if rd_before <= 150:
            calibrated_deltas.append(delta)
        else:
            uncalibrated_deltas.append(delta)

    calibrated_avg = _mean(calibrated_deltas)
    uncalibrated_avg = _mean(uncalibrated_deltas)

    # Stability ratio: calibrated should swing less than uncalibrated
    # ratio < 1 = good (calibrated players are more stable)
    # ratio > 1 = bad (calibrated players still swinging a lot)
    if calibrated_avg is not None and uncalibrated_avg is not None and uncalibrated_avg > 0:
        stability_ratio = calibrated_avg / uncalibrated_avg
    else:
        stability_ratio = None

    return {
        "calibrated_avg_delta": calibrated_avg,
        "calibrated_count": len(calibrated_deltas),
        "uncalibrated_avg_delta": uncalibrated_avg,
        "uncalibrated_count": len(uncalibrated_deltas),
        "stability_ratio": stability_ratio,
    }
