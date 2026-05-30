"""
Helpers for computing rating system insights.
"""

from __future__ import annotations

import asyncio
import statistics
from collections.abc import Iterable
from dataclasses import dataclass
from typing import Any

from domain.models.player import Player
from openskill_rating_system import CamaOpenSkillSystem
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

    glicko_prediction_quality = _compute_prediction_quality(match_predictions or [])
    openskill_prediction_quality = _compute_openskill_prediction_quality(
        rating_history_entries or []
    )
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
        "prediction_quality": glicko_prediction_quality,
        "glicko_prediction_quality": glicko_prediction_quality,
        "openskill_prediction_quality": openskill_prediction_quality,
        "rating_movement": rating_movement,
        "side_balance": side_balance,
        "rating_stability": rating_stability,
        "team_composition": team_composition,
        "avg_certainty": avg_certainty,
        "avg_rd": avg_rd,
    }


def _compute_prediction_quality(match_predictions: list[dict]) -> dict:
    prob_actual_pairs = []
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
        prob_actual_pairs.append((prob, actual))
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
        "ece": _compute_ece(prob_actual_pairs),
        "accuracy": (correct / count) if count else None,
        "balance_rate": (balanced / count) if count else None,
        "upset_rate": (upset / upset_eligible) if upset_eligible else None,
    }


def _empty_prediction_quality() -> dict:
    return {
        "count": 0,
        "brier": None,
        "ece": None,
        "accuracy": None,
        "balance_rate": None,
        "upset_rate": None,
    }


def _compute_ece(prob_actual_pairs: list[tuple[float, int]], max_bins: int = 5) -> float | None:
    """Compute adaptive-binned expected calibration error for binary probabilities."""
    if not prob_actual_pairs:
        return None

    sorted_pairs = sorted(prob_actual_pairs, key=lambda item: item[0])
    n = len(sorted_pairs)
    bins = min(max_bins, n)
    ece = 0.0
    for i in range(bins):
        start = (i * n) // bins
        end = ((i + 1) * n) // bins
        bucket = sorted_pairs[start:end]
        if not bucket:
            continue
        avg_prob = statistics.mean(prob for prob, _actual in bucket)
        actual_rate = statistics.mean(actual for _prob, actual in bucket)
        ece += (len(bucket) / n) * abs(avg_prob - actual_rate)
    return ece


def _compute_openskill_prediction_quality(rating_history_entries: list[dict]) -> dict:
    if not rating_history_entries:
        return _empty_prediction_quality()

    os_system = CamaOpenSkillSystem()
    matches: dict[int, dict[int, list[dict]]] = {}
    for entry in rating_history_entries:
        match_id = entry.get("match_id")
        team_number = entry.get("team_number")
        if match_id is None or team_number not in (1, 2):
            continue
        matches.setdefault(match_id, {1: [], 2: []})[team_number].append(entry)

    predictions = []
    for teams in matches.values():
        team1_entries = teams.get(1, [])
        team2_entries = teams.get(2, [])
        if len(team1_entries) != 5 or len(team2_entries) != 5:
            continue

        team1_ratings = [
            (entry.get("os_mu_before"), entry.get("os_sigma_before"))
            for entry in team1_entries
        ]
        team2_ratings = [
            (entry.get("os_mu_before"), entry.get("os_sigma_before"))
            for entry in team2_entries
        ]
        if any(mu is None or sigma is None for mu, sigma in team1_ratings + team2_ratings):
            continue

        team1_won = bool(team1_entries[0].get("won"))
        team2_won = bool(team2_entries[0].get("won"))
        if team1_won == team2_won:
            continue

        predictions.append({
            "expected_radiant_win_prob": os_system.os_predict_win_probability(
                team1_ratings, team2_ratings
            ),
            "winning_team": 1 if team1_won else 2,
        })

    return _compute_prediction_quality(predictions)


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


@dataclass
class PlayerCalibration:
    """Shared per-player calibration computations.

    Produced by :func:`compute_player_calibration` and consumed by both the
    ``/calibration`` individual view and the ``/profile`` Rating tab. Holds
    only the values whose *computation* is identical between the two; each
    call site formats these into its own (intentionally divergent) embed.
    """

    percentile: float | None
    drift: float | None
    matches_with_predictions: list[dict]
    actual_wins: int
    expected_wins: float
    overperformance: float | None
    favored_matches: list[dict]
    underdog_matches: list[dict]
    favored_wins: int
    underdog_wins: int
    last_5_delta: float | None
    streak: int
    streak_type: str | None
    upsets: list[tuple[dict, float]]
    chokes: list[tuple[dict, float]]


def compute_player_calibration(
    player: Player,
    history: list[dict],
    rated_players: list[Player],
    rating_system: CamaRatingSystem,
) -> PlayerCalibration:
    """Compute the calibration values shared by the calibration and profile views.

    ``history`` must be the player's detailed rating history newest-first.
    ``rated_players`` is the guild's players with a non-None ``glicko_rating``
    (used for the percentile). Pure: no I/O, no formatting.
    """
    # Percentile vs the rated population
    percentile: float | None = None
    if player.glicko_rating and rated_players:
        lower_count = sum(
            1 for p in rated_players if (p.glicko_rating or 0) < player.glicko_rating
        )
        percentile = (lower_count / len(rated_players)) * 100

    # Drift from the initial MMR seed
    drift: float | None = None
    if player.initial_mmr and player.glicko_rating:
        seed_rating = rating_system.mmr_to_rating(player.initial_mmr)
        drift = player.glicko_rating - seed_rating

    # Performance vs expectations
    matches_with_predictions = [
        h for h in history if h.get("expected_team_win_prob") is not None
    ]
    actual_wins = sum(1 for h in matches_with_predictions if h.get("won"))
    expected_wins = sum(
        h.get("expected_team_win_prob", 0) for h in matches_with_predictions
    )
    overperformance = actual_wins - expected_wins if matches_with_predictions else None

    # Win rate when favored vs underdog
    favored_matches = [
        h for h in matches_with_predictions if (h.get("expected_team_win_prob") or 0) >= 0.55
    ]
    underdog_matches = [
        h for h in matches_with_predictions if (h.get("expected_team_win_prob") or 0) <= 0.45
    ]
    favored_wins = sum(1 for h in favored_matches if h.get("won"))
    underdog_wins = sum(1 for h in underdog_matches if h.get("won"))

    # Rating trend over the last 5 games
    last_5_delta: float | None = None
    if len(history) >= 2:
        if len(history) > 5:
            last_5_delta = (history[0].get("rating") or 0) - (history[4].get("rating") or 0)
        else:
            last_5_delta = (history[0].get("rating") or 0) - (history[-1].get("rating") or 0)

    # Current streak
    streak = 0
    streak_type: str | None = None
    for h in matches_with_predictions:
        won = h.get("won")
        if streak_type is None:
            streak_type = "W" if won else "L"
            streak = 1
        elif (won and streak_type == "W") or (not won and streak_type == "L"):
            streak += 1
        else:
            break

    # Biggest upset (win as underdog) and choke (loss as favorite)
    upsets = [
        (h, h.get("expected_team_win_prob", 0.5))
        for h in matches_with_predictions
        if h.get("won") and (h.get("expected_team_win_prob") or 0.5) < 0.45
    ]
    chokes = [
        (h, h.get("expected_team_win_prob", 0.5))
        for h in matches_with_predictions
        if not h.get("won") and (h.get("expected_team_win_prob") or 0.5) > 0.55
    ]
    upsets.sort(key=lambda x: x[1])  # lowest prob first
    chokes.sort(key=lambda x: x[1], reverse=True)  # highest prob first

    return PlayerCalibration(
        percentile=percentile,
        drift=drift,
        matches_with_predictions=matches_with_predictions,
        actual_wins=actual_wins,
        expected_wins=expected_wins,
        overperformance=overperformance,
        favored_matches=favored_matches,
        underdog_matches=underdog_matches,
        favored_wins=favored_wins,
        underdog_wins=underdog_wins,
        last_5_delta=last_5_delta,
        streak=streak,
        streak_type=streak_type,
        upsets=upsets,
        chokes=chokes,
    )


async def get_os_win_probability(
    match_source: Any,
    os_system: CamaOpenSkillSystem,
    match_id: int | None,
    team_number: int | None,
    guild_id: int | None = None,
) -> float | None:
    """Fetch the OpenSkill expected win probability for a player's match side.

    ``match_source`` is anything exposing ``get_os_ratings_for_match`` (the
    match service or repository). Returns ``None`` when the match has no
    OpenSkill ratings or the team number is unknown.
    """
    if not match_id or match_source is None:
        return None
    os_ratings = await asyncio.to_thread(match_source.get_os_ratings_for_match, match_id, guild_id)
    if not (os_ratings["team1"] and os_ratings["team2"]):
        return None
    if team_number == 1:
        return os_system.os_predict_win_probability(
            os_ratings["team1"], os_ratings["team2"]
        )
    if team_number == 2:
        return os_system.os_predict_win_probability(
            os_ratings["team2"], os_ratings["team1"]
        )
    return None
