"""Deterministic chronological replay for OpenSkill rating state.

The replay is intentionally database-agnostic. Both the one-shot schema
migration and the administrative backfill feed it player/match/participant
records, then persist the returned current snapshot and per-match history in
one transaction.
"""

from __future__ import annotations

import json
import math
from dataclasses import dataclass, field
from datetime import UTC, datetime
from typing import Any

from config import NEW_PLAYER_MMR_DISCOUNT
from openskill_rating_system import CamaOpenSkillSystem
from rating_system import CamaRatingSystem

OPENSKILL_ALGORITHM_VERSION = 4
_RECENT_OUTCOME_LIMIT = 20
_LEGACY_STREAK_MULTIPLIER_PER_GAME = 0.20


def _value(record: Any, key: str, default=None):
    if isinstance(record, dict):
        return record.get(key, default)
    try:
        return record[key]
    except (KeyError, IndexError, TypeError):
        return getattr(record, key, default)


def _parse_team_ids(value: Any) -> list[int]:
    if value is None:
        return []
    if isinstance(value, str):
        try:
            value = json.loads(value)
        except (TypeError, ValueError):
            return []
    if not isinstance(value, (list, tuple)):
        return []
    try:
        return [int(player_id) for player_id in value]
    except (TypeError, ValueError):
        return []


def _parse_match_datetime(value: Any) -> datetime | None:
    if isinstance(value, datetime):
        parsed = value
    elif value:
        try:
            parsed = datetime.fromisoformat(str(value).replace("Z", "+00:00"))
        except ValueError:
            return None
    else:
        return None
    if parsed.tzinfo is not None:
        parsed = parsed.astimezone(UTC).replace(tzinfo=None)
    return parsed


def _participant_team(participant: Any) -> int | None:
    team_number = _value(participant, "team_number")
    if team_number in (1, 2):
        return int(team_number)
    side = _value(participant, "side")
    if side == "radiant":
        return 1
    if side == "dire":
        return 2
    return None


def _streak_rate(match: Any) -> float:
    """Return the rating-streak rate recorded for a match.

    Matches recorded before the rate column was introduced used the historical
    20% curve. Replay inputs without a matching history row therefore use the
    same legacy value rather than today's configuration.
    """
    stored = _value(match, "streak_multiplier_per_game")
    if stored is None:
        return _LEGACY_STREAK_MULTIPLIER_PER_GAME
    try:
        return float(stored)
    except (TypeError, ValueError):
        return _LEGACY_STREAK_MULTIPLIER_PER_GAME


@dataclass
class OpenSkillReplayResult:
    final_ratings: dict[tuple[int, int], tuple[float, float]]
    history_rows: list[dict] = field(default_factory=list)
    prediction_rows: list[dict] = field(default_factory=list)
    matches_processed: int = 0
    matches_with_fantasy: int = 0
    matches_equal_weight: int = 0
    players_touched: set[tuple[int, int]] = field(default_factory=set)
    errors: list[str] = field(default_factory=list)

    def summary(self, total_matches: int) -> dict:
        return {
            "matches_processed": self.matches_processed,
            "matches_with_fantasy": self.matches_with_fantasy,
            "matches_equal_weight": self.matches_equal_weight,
            "players_updated": len(self.players_touched),
            "total_matches": total_matches,
            "errors": self.errors[:10],
        }


def _seed_mu(
    system: CamaOpenSkillSystem,
    initial_mmr: int | None,
) -> float:
    # Match Glicko's established missing-MMR fallback and newcomer underseed.
    source_mmr = 4000 if initial_mmr is None else int(initial_mmr)
    capped_mmr = max(0, min(source_mmr, 12000))
    seed_mmr = max(0, capped_mmr - NEW_PLAYER_MMR_DISCOUNT)
    return system.mmr_to_os_mu(seed_mmr)


def replay_openskill(
    *,
    players: list[Any],
    matches: list[Any],
    participants_by_match: dict[int, list[Any]],
    rating_events: list[Any] | None = None,
    system: CamaOpenSkillSystem | None = None,
    reset_first: bool = True,
) -> OpenSkillReplayResult:
    """Replay every supplied match in deterministic chronological order."""
    if not reset_first:
        raise ValueError(
            "OpenSkill replay requires reset_first=True; continuing from current "
            "ratings would apply the supplied history twice"
        )
    rating_system = system or CamaOpenSkillSystem()
    algorithm_fingerprint = rating_system.algorithm_fingerprint()
    current: dict[tuple[int, int], tuple[float, float]] = {}
    last_match_at: dict[tuple[int, int], datetime] = {}
    recent_outcomes: dict[tuple[int, int], list[bool]] = {}
    streak_system = CamaRatingSystem()

    for player in players:
        player_id = int(_value(player, "discord_id"))
        guild_id = int(_value(player, "guild_id", 0) or 0)
        current[(guild_id, player_id)] = (
            _seed_mu(rating_system, _value(player, "initial_mmr")),
            rating_system.DEFAULT_SIGMA,
        )

    def chronological_key(record: Any, date_key: str, id_key: str) -> tuple:
        parsed = _parse_match_datetime(_value(record, date_key))
        return (
            int(_value(record, "guild_id", 0) or 0),
            parsed is None,
            parsed or datetime.max,
            int(_value(record, id_key, 0) or 0),
        )

    ordered_matches = sorted(
        matches,
        key=lambda match: chronological_key(match, "match_date", "match_id"),
    )
    replay = OpenSkillReplayResult(final_ratings=current)
    events_by_guild: dict[int, list[Any]] = {}
    for event in sorted(
        rating_events or [],
        key=lambda item: chronological_key(item, "event_at", "event_id"),
    ):
        event_guild = int(_value(event, "guild_id", 0) or 0)
        events_by_guild.setdefault(event_guild, []).append(event)
    event_offsets = dict.fromkeys(events_by_guild, 0)

    def apply_event(event: Any) -> None:
        guild_id = int(_value(event, "guild_id", 0) or 0)
        player_id = int(_value(event, "discord_id"))
        key = (guild_id, player_id)
        if key not in current:
            replay.errors.append(
                f"OpenSkill event {_value(event, 'event_id')}: unknown player {player_id}"
            )
            return
        event_type = str(_value(event, "event_type", ""))
        raw_value = _value(event, "value")
        try:
            value = float(raw_value)
        except (TypeError, ValueError):
            replay.errors.append(
                f"OpenSkill event {_value(event, 'event_id')}: invalid value {raw_value}"
            )
            return
        if not math.isfinite(value):
            replay.errors.append(
                f"OpenSkill event {_value(event, 'event_id')}: non-finite value"
            )
            return
        mu, sigma = current[key]
        if event_type == "set_mu":
            mu = value
        elif event_type == "set_sigma":
            sigma = max(0.0, min(rating_system.DEFAULT_SIGMA, value))
        elif event_type == "add_sigma":
            sigma = max(
                0.0,
                min(rating_system.DEFAULT_SIGMA, sigma + value),
            )
        else:
            replay.errors.append(
                f"OpenSkill event {_value(event, 'event_id')}: unknown type {event_type!r}"
            )
            return
        current[key] = (mu, sigma)
        replay.players_touched.add(key)

    def apply_events_through(guild_id: int, through: datetime | None) -> None:
        guild_events = events_by_guild.get(guild_id, [])
        offset = int(event_offsets.get(guild_id, 0) or 0)
        while offset < len(guild_events):
            event = guild_events[offset]
            event_at = _parse_match_datetime(_value(event, "event_at"))
            if through is not None and (event_at is None or event_at > through):
                break
            apply_event(event)
            offset += 1
        event_offsets[guild_id] = offset

    for match in ordered_matches:
        match_id = int(_value(match, "match_id"))
        guild_id = int(_value(match, "guild_id", 0) or 0)
        match_at = _parse_match_datetime(_value(match, "match_date"))
        apply_events_through(guild_id, match_at)
        winning_team = _value(match, "winning_team")
        if winning_team not in (1, 2):
            replay.errors.append(f"Match {match_id}: invalid winning_team {winning_team}")
            continue

        participants = participants_by_match.get(match_id, [])
        team1_participants = [
            participant for participant in participants if _participant_team(participant) == 1
        ]
        team2_participants = [
            participant for participant in participants if _participant_team(participant) == 2
        ]

        if len(team1_participants) == 5 and len(team2_participants) == 5:
            team1_ids = [
                int(_value(participant, "discord_id")) for participant in team1_participants
            ]
            team2_ids = [
                int(_value(participant, "discord_id")) for participant in team2_participants
            ]
        else:
            team1_ids = _parse_team_ids(_value(match, "team1_players"))
            team2_ids = _parse_team_ids(_value(match, "team2_players"))

        if len(team1_ids) != 5 or len(team2_ids) != 5 or len(set(team1_ids + team2_ids)) != 10:
            replay.errors.append(
                f"Match {match_id}: invalid team sizes {len(team1_ids)}/{len(team2_ids)}"
            )
            continue

        before: dict[int, tuple[float, float]] = {}
        for player_id in team1_ids + team2_ids:
            key = (guild_id, player_id)
            mu, sigma = current.get(
                key,
                (rating_system.DEFAULT_MU, rating_system.DEFAULT_SIGMA),
            )
            previous_at = last_match_at.get(key)
            if match_at is not None and previous_at is not None:
                days_since = max(0, (match_at - previous_at).days)
                sigma = rating_system.apply_sigma_decay(sigma, days_since)
            before[player_id] = (mu, sigma)

        participants_by_id = {
            int(_value(participant, "discord_id")): participant for participant in participants
        }
        all_ids = team1_ids + team2_ids
        won_by_player = {
            player_id: team_number == winning_team
            for team_number, player_ids in ((1, team1_ids), (2, team2_ids))
            for player_id in player_ids
        }
        recorded_streak_rate = _streak_rate(match)
        streak_multipliers: dict[int, float] = {}
        for player_id in all_ids:
            key = (guild_id, player_id)
            _streak_length, multiplier = streak_system.calculate_streak_multiplier(
                recent_outcomes.get(key, []),
                won=won_by_player[player_id],
                streak_multiplier_per_game=recorded_streak_rate,
            )
            streak_multipliers[player_id] = multiplier

        complete_fantasy = len(participants_by_id) >= 10 and all(
            player_id in participants_by_id
            and _value(participants_by_id[player_id], "fantasy_points") is not None
            for player_id in all_ids
        )
        raw_probability = rating_system.os_predict_win_probability(
            [before[player_id] for player_id in team1_ids],
            [before[player_id] for player_id in team2_ids],
        )
        replay.prediction_rows.append(
            {
                "match_id": match_id,
                "guild_id": guild_id,
                "openskill_radiant_win_prob": rating_system.calibrate_win_probability(
                    raw_probability
                ),
                "openskill_raw_radiant_win_prob": raw_probability,
                "openskill_algorithm_version": OPENSKILL_ALGORITHM_VERSION,
                "openskill_algorithm_fingerprint": algorithm_fingerprint,
            }
        )

        if complete_fantasy:
            team1_data = [
                (
                    player_id,
                    *before[player_id],
                    float(
                        _value(
                            participants_by_id[player_id],
                            "fantasy_points",
                        )
                    ),
                )
                for player_id in team1_ids
            ]
            team2_data = [
                (
                    player_id,
                    *before[player_id],
                    float(
                        _value(
                            participants_by_id[player_id],
                            "fantasy_points",
                        )
                    ),
                )
                for player_id in team2_ids
            ]
            weighted_results = rating_system.update_ratings_after_match(
                team1_data,
                team2_data,
                int(winning_team),
                streak_multipliers=streak_multipliers,
            )
            results = {
                player_id: (mu, sigma)
                for player_id, (mu, sigma, _factor) in weighted_results.items()
            }
            factors = {
                player_id: factor for player_id, (_mu, _sigma, factor) in weighted_results.items()
            }
            replay.matches_with_fantasy += 1
        else:
            results = rating_system.update_ratings_equal_weight(
                [(player_id, *before[player_id]) for player_id in team1_ids],
                [(player_id, *before[player_id]) for player_id in team2_ids],
                int(winning_team),
                streak_multipliers=streak_multipliers,
            )
            factors = dict.fromkeys(all_ids)
            replay.matches_equal_weight += 1

        for team_number, player_ids in ((1, team1_ids), (2, team2_ids)):
            for player_id in player_ids:
                key = (guild_id, player_id)
                old_mu, old_sigma = before[player_id]
                new_mu, new_sigma = results[player_id]
                current[key] = (new_mu, new_sigma)
                replay.players_touched.add(key)
                if match_at is not None:
                    last_match_at[key] = match_at
                replay.history_rows.append(
                    {
                        "guild_id": guild_id,
                        "match_id": match_id,
                        "match_date": _value(match, "match_date"),
                        "discord_id": player_id,
                        "team_number": team_number,
                        "won": int(team_number == winning_team),
                        "os_mu_before": old_mu,
                        "os_mu_after": new_mu,
                        "os_sigma_before": old_sigma,
                        "os_sigma_after": new_sigma,
                        "fantasy_weight": factors[player_id],
                        "os_algorithm_version": OPENSKILL_ALGORITHM_VERSION,
                        "os_algorithm_fingerprint": algorithm_fingerprint,
                    }
                )
                outcomes = recent_outcomes.setdefault(key, [])
                outcomes.insert(0, won_by_player[player_id])
                del outcomes[_RECENT_OUTCOME_LIMIT:]
        replay.matches_processed += 1

    for guild_id, guild_events in events_by_guild.items():
        offset = int(event_offsets.get(guild_id, 0) or 0)
        for event in guild_events[offset:]:
            apply_event(event)

    return replay
