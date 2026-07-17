"""Dynamic, guild-scoped trivia questions built from persisted player activity."""

from __future__ import annotations

import logging
import math
import random
import re
import time
from collections import Counter, defaultdict
from collections.abc import Callable, Iterable, Mapping, Sequence
from dataclasses import dataclass
from datetime import UTC, datetime
from typing import Any

from config import CALIBRATION_RD_THRESHOLD, PREDICTION_CONTRACT_VALUE
from domain.rating_constants import OPENSKILL_DISPLAY_SCALE, OPENSKILL_MIN_MU

logger = logging.getLogger("cama_bot.services.player_trivia")

_LANE_NAMES = {
    0: "Roaming",
    1: "Safe Lane",
    2: "Mid Lane",
    3: "Off Lane",
    4: "Jungle",
}
_TOWN_MAFIA_ROLES = {"TOWNIE", "DOCTOR", "DETECTIVE", "VIGILANTE"}
_MARKDOWN_RE = re.compile(r"[\\*_`~|>\[\]()]")


@dataclass(frozen=True)
class PlayerTriviaQuestion:
    """One immutable, fully rendered multiple-choice question."""

    key: str
    category: str
    text: str
    options: tuple[str, str, str, str]
    correct_index: int
    explanation: str
    spicy: bool = False

    def __post_init__(self) -> None:
        if len(self.options) != 4:
            raise ValueError("Player-trivia questions require exactly four options.")
        if len({option.casefold() for option in self.options}) != 4:
            raise ValueError("Player-trivia options must be distinct.")
        if not 0 <= self.correct_index < 4:
            raise ValueError("correct_index must identify one of the four options.")

    def to_record(self) -> dict[str, Any]:
        """Return the JSON-friendly representation stored with a session."""
        return {
            "question_key": self.key,
            "category": self.category,
            "question_text": self.text,
            "options": list(self.options),
            "correct_index": self.correct_index,
            "explanation": self.explanation,
            "spicy": self.spicy,
        }


@dataclass(frozen=True)
class _Candidate:
    question: PlayerTriviaQuestion
    identity: str


@dataclass
class _Context:
    snapshot: dict[str, list[dict[str, Any]]]
    names: dict[int, str]
    player_rows: dict[int, dict[str, Any]]
    candidates: list[_Candidate]


def _safe_name(value: Any) -> str:
    """Render a stored display name as inert, single-line plain text."""
    text = " ".join(str(value or "").replace("@", "＠").split())
    text = _MARKDOWN_RE.sub("", text).strip()
    return text[:72]


def _as_int(value: Any, default: int = 0) -> int:
    try:
        return int(value)
    except (TypeError, ValueError, OverflowError):
        return default


def _as_float(value: Any, default: float | None = None) -> float | None:
    try:
        number = float(value)
    except (TypeError, ValueError, OverflowError):
        return default
    return number if math.isfinite(number) else default


def _truthy(value: Any) -> bool:
    if isinstance(value, str):
        return value.strip().lower() in {"1", "true", "yes", "win", "won"}
    return bool(value)


def _date_key(value: Any) -> float:
    if value is None:
        return 0.0
    if isinstance(value, (int, float)):
        return float(value)
    raw = str(value).strip()
    if not raw:
        return 0.0
    try:
        return float(raw)
    except ValueError:
        pass
    try:
        parsed = datetime.fromisoformat(raw.replace("Z", "+00:00"))
        if parsed.tzinfo is None:
            parsed = parsed.replace(tzinfo=UTC)
        return parsed.timestamp()
    except ValueError:
        return 0.0


def _humanize_code(value: Any) -> str:
    text = str(value or "").strip().replace("_", " ").replace("-", " ")
    return " ".join(part.capitalize() for part in text.split())


class PlayerTriviaService:
    """Build a varied question bank from a repository snapshot."""

    def __init__(self, repo: Any, rng: random.Random | None = None):
        self.repo = repo
        self.rng = rng or random.Random()

    # ------------------------------------------------------------------
    # Session persistence delegates
    # ------------------------------------------------------------------

    def try_start_session(
        self,
        discord_id: int,
        guild_id: int,
        questions: Sequence[PlayerTriviaQuestion | Mapping[str, Any]],
        now: int,
        cooldown_seconds: int,
        bypass: bool = False,
    ) -> Any:
        records = [
            question.to_record() if isinstance(question, PlayerTriviaQuestion) else dict(question)
            for question in questions
        ]
        return self.repo.try_start_session(
            discord_id,
            guild_id,
            records,
            now,
            cooldown_seconds,
            bypass=bypass,
        )

    def get_last_session_started(self, discord_id: int, guild_id: int) -> Any:
        return self.repo.get_last_session_started(discord_id, guild_id)

    def settle_answer(
        self,
        session_id: int,
        question_number: int,
        selected_index: int,
        reward: int,
        answered_at: int,
    ) -> Any:
        return self.repo.settle_answer(
            session_id,
            question_number,
            selected_index,
            reward,
            answered_at,
        )

    def finish_session(self, session_id: int, status: str, completed_at: int) -> Any:
        return self.repo.finish_session(session_id, status, completed_at)

    def cancel_session_if_unanswered(self, session_id: int) -> Any:
        return self.repo.cancel_session_if_unanswered(session_id)

    def get_session(self, session_id: int) -> Any:
        return self.repo.get_session(session_id)

    def get_questions(self, session_id: int) -> Any:
        return self.repo.get_questions(session_id)

    # ------------------------------------------------------------------
    # Public generation API
    # ------------------------------------------------------------------

    def generate_questions(
        self,
        user_id: int,
        guild_id: int,
        current_member_ids: Iterable[int] | None = None,
        count: int = 10,
        include_spicy: bool = False,
        recent_days: int = 30,
    ) -> list[PlayerTriviaQuestion]:
        if count <= 0:
            return []

        raw_snapshot = self.repo.load_snapshot(guild_id) or {}
        snapshot = {
            str(key): [dict(row) for row in (rows or [])] for key, rows in raw_snapshot.items()
        }
        allowed = (
            {_as_int(member_id) for member_id in current_member_ids}
            if current_member_ids is not None
            else None
        )
        names, player_rows = self._player_index(snapshot.get("players", []), allowed)
        if len(names) < 4:
            return []

        context = _Context(
            snapshot=snapshot,
            names=names,
            player_rows=player_rows,
            candidates=[],
        )
        builders = (
            self._build_match_questions,
            self._build_rating_questions,
            self._build_hero_lane_performance_questions,
            self._build_pairing_questions,
            self._build_economy_and_tip_questions,
            self._build_betting_questions,
            self._build_wheel_questions,
            self._build_double_questions,
            self._build_prediction_questions,
            self._build_dig_questions,
            self._build_mafia_questions,
            self._build_trivia_questions,
            self._build_disbursement_questions,
            self._build_protected_hero_questions,
        )
        for builder in builders:
            try:
                builder(context)
            except Exception:
                # One malformed optional dataset must not make the entire game
                # unavailable. Log the category and keep the other candidates.
                logger.exception("Failed to build player-trivia candidates in %s", builder.__name__)

        since = int(time.time()) - max(0, recent_days) * 86400
        recent_raw = self.repo.get_recent_question_keys(user_id, guild_id, since) or []
        recent_keys = {
            str(item.get("key") or item.get("question_key"))
            if isinstance(item, Mapping)
            else str(item)
            for item in recent_raw
        }
        eligible = [
            candidate
            for candidate in context.candidates
            if candidate.question.key not in recent_keys
            and (include_spicy or not candidate.question.spicy)
        ]
        self.rng.shuffle(eligible)

        selected: list[PlayerTriviaQuestion] = []
        category_counts: Counter[str] = Counter()
        identity_counts: Counter[str] = Counter()
        seen_keys: set[str] = set()
        for candidate in eligible:
            question = candidate.question
            if question.key in seen_keys:
                continue
            if category_counts[question.category] >= 2:
                continue
            if identity_counts[candidate.identity] >= 2:
                continue
            selected.append(question)
            seen_keys.add(question.key)
            category_counts[question.category] += 1
            identity_counts[candidate.identity] += 1
            if len(selected) >= count:
                break
        return selected

    # ------------------------------------------------------------------
    # Generic candidate helpers
    # ------------------------------------------------------------------

    def _player_index(
        self,
        rows: Sequence[dict[str, Any]],
        allowed: set[int] | None,
    ) -> tuple[dict[int, str], dict[int, dict[str, Any]]]:
        pending: list[tuple[int, str, dict[str, Any]]] = []
        name_counts: Counter[str] = Counter()
        for row in sorted(rows, key=lambda item: _as_int(item.get("discord_id"))):
            discord_id = _as_int(row.get("discord_id"))
            if discord_id <= 0 or (allowed is not None and discord_id not in allowed):
                continue
            name = _safe_name(
                row.get("username")
                or row.get("discord_username")
                or row.get("display_name")
                or row.get("name")
            )
            if not name:
                continue
            pending.append((discord_id, name, row))
            name_counts[name.casefold()] += 1

        names: dict[int, str] = {}
        player_rows: dict[int, dict[str, Any]] = {}
        for discord_id, name, row in pending:
            # Ambiguous button labels cannot have a unique correct answer.
            if name_counts[name.casefold()] != 1:
                continue
            names[discord_id] = name
            player_rows[discord_id] = row
        return names, player_rows

    def _four_options(
        self, correct: str, distractors: Iterable[str]
    ) -> tuple[tuple[str, str, str, str], int] | None:
        correct = _safe_name(correct)
        if not correct:
            return None
        unique: dict[str, str] = {}
        for value in distractors:
            safe = _safe_name(value)
            if safe and safe.casefold() != correct.casefold():
                unique.setdefault(safe.casefold(), safe)
        if len(unique) < 3:
            return None
        choices = self.rng.sample(sorted(unique.values(), key=str.casefold), 3)
        choices.append(correct)
        self.rng.shuffle(choices)
        options = tuple(choices)
        return options, options.index(correct)  # type: ignore[return-value]

    def _add_player_leader(
        self,
        context: _Context,
        *,
        key: str,
        category: str,
        text: str,
        values: Mapping[int, float],
        explanation: Callable[[str, float], str],
        minimum: float | None = None,
        lowest: bool = False,
        displayed: Callable[[float], Any] | None = None,
        spicy: bool = False,
    ) -> None:
        usable = [
            (player_id, float(value))
            for player_id, value in values.items()
            if player_id in context.names and math.isfinite(float(value))
        ]
        if len(usable) < 4:
            return
        usable.sort(key=lambda item: (item[1], item[0]), reverse=not lowest)
        winner_id, winner_value = usable[0]
        if minimum is not None:
            if lowest and winner_value > minimum:
                return
            if not lowest and winner_value < minimum:
                return
        render = displayed or (lambda value: value)
        if winner_value == usable[1][1] or render(winner_value) == render(usable[1][1]):
            return
        built = self._four_options(
            context.names[winner_id],
            (context.names[player_id] for player_id, _ in usable[1:]),
        )
        if built is None:
            return
        options, correct_index = built
        name = context.names[winner_id]
        context.candidates.append(
            _Candidate(
                PlayerTriviaQuestion(
                    key=key,
                    category=category,
                    text=text,
                    options=options,
                    correct_index=correct_index,
                    explanation=explanation(name, winner_value),
                    spicy=spicy,
                ),
                identity=f"player:{winner_id}",
            )
        )

    def _add_value_question(
        self,
        context: _Context,
        *,
        key: str,
        category: str,
        text: str,
        correct: str,
        distractors: Iterable[str],
        explanation: str,
        identity: str,
        spicy: bool = False,
    ) -> None:
        built = self._four_options(correct, distractors)
        if built is None:
            return
        options, correct_index = built
        context.candidates.append(
            _Candidate(
                PlayerTriviaQuestion(
                    key=key,
                    category=category,
                    text=text,
                    options=options,
                    correct_index=correct_index,
                    explanation=explanation,
                    spicy=spicy,
                ),
                identity=identity,
            )
        )

    @staticmethod
    def _rows_for_players(
        rows: Iterable[dict[str, Any]], names: Mapping[int, str], field: str = "discord_id"
    ) -> list[dict[str, Any]]:
        return [row for row in rows if _as_int(row.get(field)) in names]

    @staticmethod
    def _hero_name(hero_id: int) -> str:
        try:
            from utils.hero_lookup import get_hero_name

            name = get_hero_name(hero_id)
        except Exception:
            name = None
        return _safe_name(name or f"Hero {hero_id}")

    # ------------------------------------------------------------------
    # Match, rating, hero, lane, and performance candidates
    # ------------------------------------------------------------------

    def _match_aggregates(
        self, context: _Context
    ) -> tuple[dict[int, dict[str, int]], dict[int, list[dict[str, Any]]]]:
        by_player: dict[int, list[dict[str, Any]]] = defaultdict(list)
        for row in self._rows_for_players(context.snapshot.get("participants", []), context.names):
            by_player[_as_int(row.get("discord_id"))].append(row)

        stats: dict[int, dict[str, int]] = {}
        for player_id, player_row in context.player_rows.items():
            rows = by_player.get(player_id, [])
            wins = _as_int(player_row.get("wins"), -1)
            losses = _as_int(player_row.get("losses"), -1)
            if wins < 0 or losses < 0:
                wins = sum(_truthy(row.get("won")) for row in rows)
                losses = len(rows) - wins

            ordered = sorted(
                rows,
                key=lambda row: (
                    _date_key(row.get("match_date")),
                    _as_int(row.get("match_id")),
                ),
            )
            streak = 0
            if ordered:
                latest_won = _truthy(ordered[-1].get("won"))
                for row in reversed(ordered):
                    if _truthy(row.get("won")) != latest_won:
                        break
                    streak += 1
                if not latest_won:
                    streak = -streak
            stats[player_id] = {
                "wins": max(0, wins),
                "losses": max(0, losses),
                "games": max(0, wins) + max(0, losses),
                "streak": streak,
            }
        return stats, by_player

    def _build_match_questions(self, context: _Context) -> None:
        stats, _ = self._match_aggregates(context)
        games = {player_id: row["games"] for player_id, row in stats.items()}
        wins = {player_id: row["wins"] for player_id, row in stats.items()}
        losses = {player_id: row["losses"] for player_id, row in stats.items()}
        rates = {
            player_id: row["wins"] / row["games"]
            for player_id, row in stats.items()
            if row["games"] >= 10
        }
        win_streaks = {
            player_id: row["streak"] for player_id, row in stats.items() if row["streak"] > 0
        }
        self._add_player_leader(
            context,
            key="matches:most_games",
            category="matches",
            text="Who has played the most recorded inhouse matches?",
            values=games,
            minimum=3,
            explanation=lambda name,
            value: f"As of game start, {name} had played {int(value)} recorded matches.",
        )
        self._add_player_leader(
            context,
            key="matches:most_wins",
            category="matches",
            text="Who has the most recorded match wins?",
            values=wins,
            minimum=2,
            explanation=lambda name,
            value: f"As of game start, {name} had {int(value)} recorded wins.",
        )
        self._add_player_leader(
            context,
            key="matches:highest_win_rate",
            category="matches",
            text="Among players with at least 10 games, who has the highest match win rate?",
            values=rates,
            explanation=lambda name, value: f"As of game start, {name}'s win rate was {value:.1%}.",
            displayed=lambda value: round(value * 100, 1),
        )
        self._add_player_leader(
            context,
            key="matches:longest_current_win_streak",
            category="matches",
            text="Who is riding the longest current recorded-match win streak?",
            values=win_streaks,
            minimum=2,
            explanation=lambda name,
            value: f"As of game start, {name} was on a {int(value)}-match win streak.",
        )
        self._add_player_leader(
            context,
            key="matches:lowest_win_rate",
            category="matches",
            text="Among players with at least 10 games, who has the lowest match win rate?",
            values=rates,
            lowest=True,
            explanation=lambda name, value: f"As of game start, {name}'s win rate was {value:.1%}.",
            displayed=lambda value: round(value * 100, 1),
            spicy=True,
        )
        self._add_player_leader(
            context,
            key="matches:most_losses",
            category="matches",
            text="Who has the most recorded match losses?",
            values=losses,
            minimum=2,
            explanation=lambda name,
            value: f"As of game start, {name} had {int(value)} recorded losses.",
            spicy=True,
        )

        records = {
            player_id: f"{row['wins']}W–{row['losses']}L"
            for player_id, row in stats.items()
            if row["games"] > 0
        }
        for player_id in sorted(records):
            self._add_value_question(
                context,
                key=f"matches:record:{player_id}",
                category="matches",
                text=f"What is {context.names[player_id]}'s recorded match record?",
                correct=records[player_id],
                distractors=(value for other_id, value in records.items() if other_id != player_id),
                explanation=f"As of game start, {context.names[player_id]} was {records[player_id]}.",
                identity=f"player:{player_id}",
            )

    def _rating_is_active(self, player_id: int, context: _Context, games: int) -> bool:
        if games < 10:
            return False
        raw = context.player_rows[player_id].get("last_match_date")
        when = _date_key(raw)
        return not (when and when < time.time() - 180 * 86400)

    def _build_rating_questions(self, context: _Context) -> None:
        match_stats, _ = self._match_aggregates(context)
        glicko: dict[int, int] = {}
        openskill: dict[int, int] = {}
        for player_id, row in context.player_rows.items():
            if not self._rating_is_active(player_id, context, match_stats[player_id]["games"]):
                continue
            rating = _as_float(row.get("glicko_rating"))
            rd = _as_float(row.get("glicko_rd"))
            mu = _as_float(row.get("os_mu"))
            sigma = _as_float(row.get("os_sigma"))
            if rating is None or rd is None or rd > CALIBRATION_RD_THRESHOLD:
                continue
            if mu is None or sigma is None or sigma > 4.0:
                continue
            glicko[player_id] = max(0, min(3000, int(round(rating))))
            os_display = (mu - OPENSKILL_MIN_MU) * OPENSKILL_DISPLAY_SCALE
            openskill[player_id] = max(0, min(3000, int(round(os_display))))
        if len(glicko) < 4:
            return

        self._add_player_leader(
            context,
            key="ratings:glicko_leader",
            category="ratings",
            text="Among active calibrated players, who has the highest Glicko rating?",
            values=glicko,
            explanation=lambda name,
            value: f"As of game start, {name}'s Glicko display rating was {int(value)}.",
            displayed=round,
        )
        self._add_player_leader(
            context,
            key="ratings:openskill_leader",
            category="ratings",
            text="Among active calibrated players, who has the highest OpenSkill display rating?",
            values=openskill,
            explanation=lambda name,
            value: f"As of game start, {name}'s OpenSkill display rating was {int(value)}.",
            displayed=round,
        )
        display_gap = {
            player_id: abs(glicko[player_id] - openskill[player_id]) for player_id in glicko
        }
        self._add_player_leader(
            context,
            key="ratings:largest_display_gap",
            category="ratings",
            text="Whose Glicko and OpenSkill display ratings disagree by the most points?",
            values=display_gap,
            minimum=1,
            explanation=lambda name,
            value: f"As of game start, {name}'s two 0–3000 ratings were {int(value)} points apart.",
            displayed=round,
        )
        self._add_player_leader(
            context,
            key="ratings:smallest_display_gap",
            category="ratings",
            text="Whose Glicko and OpenSkill display ratings are closest together?",
            values=display_gap,
            lowest=True,
            explanation=lambda name,
            value: f"As of game start, {name}'s two 0–3000 ratings were only {int(value)} points apart.",
            displayed=round,
        )

        glicko_order = sorted(glicko, key=lambda player_id: (-glicko[player_id], player_id))
        os_order = sorted(openskill, key=lambda player_id: (-openskill[player_id], player_id))
        glicko_rank = {player_id: rank for rank, player_id in enumerate(glicko_order, 1)}
        os_rank = {player_id: rank for rank, player_id in enumerate(os_order, 1)}
        rank_gap = {
            player_id: abs(glicko_rank[player_id] - os_rank[player_id]) for player_id in glicko
        }
        self._add_player_leader(
            context,
            key="ratings:largest_rank_gap",
            category="ratings",
            text="Whose Glicko rank and OpenSkill rank disagree by the most places?",
            values=rank_gap,
            minimum=1,
            explanation=lambda name,
            value: f"As of game start, {name}'s two rating ranks were {int(value)} places apart.",
        )
        self._add_player_leader(
            context,
            key="ratings:smallest_rank_gap",
            category="ratings",
            text="Whose Glicko rank and OpenSkill rank are closest?",
            values=rank_gap,
            lowest=True,
            explanation=lambda name,
            value: f"As of game start, {name}'s two rating ranks were {int(value)} places apart.",
        )

        rank_options = [f"#{rank}" for rank in range(1, len(glicko) + 1)]
        for player_id in glicko_order:
            correct = f"#{glicko_rank[player_id]}"
            self._add_value_question(
                context,
                key=f"ratings:glicko_rank:{player_id}",
                category="ratings",
                text=f"What is {context.names[player_id]}'s Glicko rank among active calibrated players?",
                correct=correct,
                distractors=(value for value in rank_options if value != correct),
                explanation=f"As of game start, {context.names[player_id]} ranked {correct} by Glicko.",
                identity=f"player:{player_id}",
            )

        recalibrations = {
            _as_int(row.get("discord_id")): _as_int(row.get("total_recalibrations"))
            for row in context.snapshot.get("recalibrations", [])
            if _as_int(row.get("discord_id")) in context.names
        }
        self._add_player_leader(
            context,
            key="ratings:most_recalibrations",
            category="ratings",
            text="Who has completed the most recorded rating recalibrations?",
            values=recalibrations,
            minimum=1,
            explanation=lambda name, value: (
                f"As of game start, {name} had completed {int(value)} recalibrations."
            ),
        )

        largest_gain: dict[int, float] = {}
        for row in context.snapshot.get("ratings", []):
            player_id = _as_int(row.get("discord_id"))
            after = _as_float(row.get("rating"))
            before = _as_float(row.get("rating_before"))
            if player_id not in context.names or after is None or before is None:
                continue
            gain = after - before
            largest_gain[player_id] = max(largest_gain.get(player_id, gain), gain)
        self._add_player_leader(
            context,
            key="ratings:largest_single_gain",
            category="ratings",
            text="Who has the largest single-match Glicko rating gain on record?",
            values=largest_gain,
            minimum=0.1,
            explanation=lambda name, value: (
                f"As of game start, {name}'s largest one-match Glicko gain was {value:.1f} points."
            ),
            displayed=lambda value: round(value, 1),
        )

    def _build_hero_lane_performance_questions(self, context: _Context) -> None:
        _, by_player = self._match_aggregates(context)
        enriched = {
            player_id: [row for row in rows if _as_int(row.get("hero_id")) > 0]
            for player_id, rows in by_player.items()
        }
        eligible = {player_id: rows for player_id, rows in enriched.items() if len(rows) >= 3}

        distinct_played = {
            player_id: len({_as_int(row.get("hero_id")) for row in rows})
            for player_id, rows in eligible.items()
        }
        distinct_won = {
            player_id: len({_as_int(row.get("hero_id")) for row in rows if _truthy(row.get("won"))})
            for player_id, rows in eligible.items()
        }
        distinct_lost = {
            player_id: len(
                {_as_int(row.get("hero_id")) for row in rows if not _truthy(row.get("won"))}
            )
            for player_id, rows in eligible.items()
        }
        self._add_player_leader(
            context,
            key="heroes:most_distinct_played",
            category="heroes",
            text="Who has played the widest variety of heroes in enriched matches?",
            values=distinct_played,
            minimum=2,
            explanation=lambda name,
            value: f"As of game start, {name} had played {int(value)} distinct heroes in enriched matches.",
        )
        self._add_player_leader(
            context,
            key="heroes:most_distinct_won",
            category="heroes",
            text="Who has recorded wins with the most distinct heroes?",
            values=distinct_won,
            minimum=2,
            explanation=lambda name,
            value: f"As of game start, {name} had won with {int(value)} distinct heroes.",
        )
        self._add_player_leader(
            context,
            key="heroes:most_distinct_lost",
            category="heroes",
            text="Who has recorded losses with the most distinct heroes?",
            values=distinct_lost,
            minimum=2,
            explanation=lambda name,
            value: f"As of game start, {name} had lost with {int(value)} distinct heroes.",
        )

        all_hero_names = {
            self._hero_name(_as_int(row.get("hero_id")))
            for rows in eligible.values()
            for row in rows
        }
        for player_id, rows in sorted(eligible.items()):
            counts = Counter(_as_int(row.get("hero_id")) for row in rows)
            ordered_counts = counts.most_common()
            if (
                ordered_counts
                and (len(ordered_counts) == 1 or ordered_counts[0][1] > ordered_counts[1][1])
                and ordered_counts[0][1] >= 3
            ):
                hero_id, hero_games = ordered_counts[0]
                hero_name = self._hero_name(hero_id)
                self._add_value_question(
                    context,
                    key=f"heroes:most_played:{player_id}",
                    category="heroes",
                    text=f"Which hero has {context.names[player_id]} played most often?",
                    correct=hero_name,
                    distractors=(name for name in all_hero_names if name != hero_name),
                    explanation=f"As of game start, {context.names[player_id]} had {hero_games} enriched games on {hero_name}.",
                    identity=f"player:{player_id}",
                )

            ordered_rows = sorted(
                rows,
                key=lambda row: (
                    _date_key(row.get("match_date")),
                    _as_int(row.get("match_id")),
                ),
            )
            latest_hero = self._hero_name(_as_int(ordered_rows[-1].get("hero_id")))
            self._add_value_question(
                context,
                key=f"heroes:most_recent:{player_id}",
                category="heroes",
                text=f"Which hero did {context.names[player_id]} play most recently?",
                correct=latest_hero,
                distractors=(name for name in all_hero_names if name != latest_hero),
                explanation=f"Ordering enriched matches by match date and match ID, {context.names[player_id]}'s latest hero was {latest_hero}.",
                identity=f"player:{player_id}",
            )

            wins_by_hero = Counter(
                _as_int(row.get("hero_id")) for row in rows if _truthy(row.get("won"))
            )
            win_order = wins_by_hero.most_common()
            if win_order and (len(win_order) == 1 or win_order[0][1] > win_order[1][1]):
                hero_id, hero_wins = win_order[0]
                if hero_wins >= 2:
                    hero_name = self._hero_name(hero_id)
                    self._add_value_question(
                        context,
                        key=f"heroes:most_wins:{player_id}",
                        category="heroes",
                        text=f"Which hero has {context.names[player_id]} won with most often?",
                        correct=hero_name,
                        distractors=(name for name in all_hero_names if name != hero_name),
                        explanation=f"As of game start, {context.names[player_id]} had {hero_wins} wins on {hero_name}.",
                        identity=f"player:{player_id}",
                    )

            if len(rows) >= 8:
                hero_rows: dict[int, list[dict[str, Any]]] = defaultdict(list)
                for row in rows:
                    hero_rows[_as_int(row.get("hero_id"))].append(row)
                rates = [
                    (
                        hero_id,
                        sum(_truthy(row.get("won")) for row in hero_games) / len(hero_games),
                        len(hero_games),
                    )
                    for hero_id, hero_games in hero_rows.items()
                    if len(hero_games) >= 3
                ]
                rates.sort(key=lambda item: (item[1], item[0]))
                if len(rates) >= 2 and round(rates[0][1] * 100, 1) != round(rates[1][1] * 100, 1):
                    hero_id, rate, hero_games = rates[0]
                    hero_name = self._hero_name(hero_id)
                    self._add_value_question(
                        context,
                        key=f"heroes:worst_win_rate:{player_id}",
                        category="heroes",
                        text=f"Which qualifying hero gives {context.names[player_id]} their lowest win rate?",
                        correct=hero_name,
                        distractors=(self._hero_name(other_id) for other_id, _, _ in rates[1:]),
                        explanation=f"As of game start, {context.names[player_id]} was {rate:.1%} across {hero_games} games on {hero_name}.",
                        identity=f"player:{player_id}",
                        spicy=True,
                    )

            lane_counts = Counter(
                _as_int(row.get("lane_role"), -1)
                for row in rows
                if row.get("lane_role") is not None
                and _as_int(row.get("lane_role"), -1) in _LANE_NAMES
            )
            lane_order = lane_counts.most_common()
            if (
                sum(lane_counts.values()) >= 5
                and lane_order
                and (len(lane_order) == 1 or lane_order[0][1] > lane_order[1][1])
            ):
                lane_id, lane_games = lane_order[0]
                lane_name = _LANE_NAMES[lane_id]
                self._add_value_question(
                    context,
                    key=f"lanes:most_common:{player_id}",
                    category="lanes",
                    text=f"Which lane has {context.names[player_id]} played most often?",
                    correct=lane_name,
                    distractors=(name for lane, name in _LANE_NAMES.items() if lane != lane_id),
                    explanation=f"As of game start, {context.names[player_id]} had {lane_games} enriched games in the {lane_name}.",
                    identity=f"player:{player_id}",
                )

        performance_fields = (
            ("kills", "average kills", 1),
            ("assists", "average assists", 1),
            ("gpm", "average GPM", 0),
            ("hero_damage", "average hero damage", 0),
            ("tower_damage", "average tower damage", 0),
            ("fantasy_points", "average fantasy points", 1),
        )
        for field, label, digits in performance_fields:
            values: dict[int, float] = {}
            for player_id, rows in enriched.items():
                measured = [
                    float(row[field])
                    for row in rows
                    if row.get(field) is not None and _as_float(row.get(field)) is not None
                ]
                if len(measured) >= 10:
                    values[player_id] = sum(measured) / len(measured)
            self._add_player_leader(
                context,
                key=f"performance:highest_{field}_average",
                category="performance",
                text=f"Among players with at least 10 enriched games, who has the highest {label}?",
                values=values,
                explanation=lambda name,
                value,
                label=label,
                digits=digits: f"As of game start, {name}'s {label} was {value:.{digits}f}.",
                displayed=lambda value, digits=digits: round(value, digits),
            )

        record_fields = (
            ("kills", "single-match kill record"),
            ("gpm", "single-match GPM record"),
            ("hero_damage", "single-match hero-damage record"),
        )
        for field, label in record_fields:
            values = {
                player_id: max(float(row[field]) for row in rows if row.get(field) is not None)
                for player_id, rows in eligible.items()
                if any(row.get(field) is not None for row in rows)
            }
            self._add_player_leader(
                context,
                key=f"performance:record_{field}",
                category="performance",
                text=f"Who holds the highest {label}?",
                values=values,
                explanation=lambda name,
                value,
                label=label: f"As of game start, {name}'s {label} was {int(value):,}.",
                displayed=round,
            )

    # ------------------------------------------------------------------
    # Pairings and economy candidates
    # ------------------------------------------------------------------

    def _build_pairing_questions(self, context: _Context) -> None:
        together: dict[int, dict[int, tuple[int, int]]] = defaultdict(dict)
        against: dict[int, dict[int, tuple[int, int]]] = defaultdict(dict)
        for row in context.snapshot.get("pairings", []):
            player1 = _as_int(row.get("player1_id"))
            player2 = _as_int(row.get("player2_id"))
            if player1 not in context.names or player2 not in context.names or player1 == player2:
                continue
            games_together = max(0, _as_int(row.get("games_together")))
            wins_together = max(0, min(games_together, _as_int(row.get("wins_together"))))
            games_against = max(0, _as_int(row.get("games_against")))
            player1_wins = max(
                0,
                min(games_against, _as_int(row.get("player1_wins_against"))),
            )
            if games_together:
                together[player1][player2] = (games_together, wins_together)
                together[player2][player1] = (games_together, wins_together)
            if games_against:
                against[player1][player2] = (games_against, player1_wins)
                against[player2][player1] = (
                    games_against,
                    games_against - player1_wins,
                )

        for player_id in sorted(context.names):
            teammate_rows = together.get(player_id, {})
            if len(teammate_rows) >= 4:
                by_games = sorted(
                    teammate_rows.items(),
                    key=lambda item: (-item[1][0], item[0]),
                )
                if by_games[0][1][0] >= 5 and by_games[0][1][0] > by_games[1][1][0]:
                    teammate_id, (games, _wins) = by_games[0]
                    self._add_value_question(
                        context,
                        key=f"pairings:most_common_teammate:{player_id}",
                        category="pairings",
                        text=f"Who has played alongside {context.names[player_id]} most often?",
                        correct=context.names[teammate_id],
                        distractors=(context.names[other_id] for other_id, _ in by_games[1:]),
                        explanation=f"As of game start, {context.names[player_id]} and {context.names[teammate_id]} had shared {games} matches.",
                        identity=f"player:{teammate_id}",
                    )

                qualifying = [
                    (other_id, games, wins, wins / games)
                    for other_id, (games, wins) in teammate_rows.items()
                    if games >= 5
                ]
                best = sorted(qualifying, key=lambda item: (-item[3], item[0]))
                if len(best) >= 4 and round(best[0][3] * 100, 1) != round(best[1][3] * 100, 1):
                    teammate_id, games, wins, rate = best[0]
                    self._add_value_question(
                        context,
                        key=f"pairings:best_teammate:{player_id}",
                        category="pairings",
                        text=f"Among qualifying teammates, who has the highest win rate with {context.names[player_id]}?",
                        correct=context.names[teammate_id],
                        distractors=(context.names[item[0]] for item in best[1:]),
                        explanation=f"As of game start, they were {wins}W–{games - wins}L together ({rate:.1%}).",
                        identity=f"player:{teammate_id}",
                    )
                worst = sorted(qualifying, key=lambda item: (item[3], item[0]))
                if len(worst) >= 4 and round(worst[0][3] * 100, 1) != round(worst[1][3] * 100, 1):
                    teammate_id, games, wins, rate = worst[0]
                    self._add_value_question(
                        context,
                        key=f"pairings:worst_teammate:{player_id}",
                        category="pairings",
                        text=f"Among qualifying teammates, who has the lowest win rate with {context.names[player_id]}?",
                        correct=context.names[teammate_id],
                        distractors=(context.names[item[0]] for item in worst[1:]),
                        explanation=f"As of game start, they were {wins}W–{games - wins}L together ({rate:.1%}).",
                        identity=f"player:{teammate_id}",
                        spicy=True,
                    )

            opponent_rows = against.get(player_id, {})
            if len(opponent_rows) >= 4:
                by_games = sorted(
                    opponent_rows.items(),
                    key=lambda item: (-item[1][0], item[0]),
                )
                if by_games[0][1][0] >= 5 and by_games[0][1][0] > by_games[1][1][0]:
                    opponent_id, (games, _wins) = by_games[0]
                    self._add_value_question(
                        context,
                        key=f"pairings:most_common_opponent:{player_id}",
                        category="pairings",
                        text=f"Who has faced {context.names[player_id]} as an opponent most often?",
                        correct=context.names[opponent_id],
                        distractors=(context.names[other_id] for other_id, _ in by_games[1:]),
                        explanation=f"As of game start, {context.names[player_id]} had faced {context.names[opponent_id]} {games} times.",
                        identity=f"player:{opponent_id}",
                    )
                qualifying = [
                    (other_id, games, wins, wins / games)
                    for other_id, (games, wins) in opponent_rows.items()
                    if games >= 5
                ]
                best = sorted(qualifying, key=lambda item: (-item[3], item[0]))
                if len(best) >= 4 and round(best[0][3] * 100, 1) != round(best[1][3] * 100, 1):
                    opponent_id, games, wins, rate = best[0]
                    self._add_value_question(
                        context,
                        key=f"pairings:best_opponent:{player_id}",
                        category="pairings",
                        text=f"Against which qualifying opponent does {context.names[player_id]} have the best win rate?",
                        correct=context.names[opponent_id],
                        distractors=(context.names[item[0]] for item in best[1:]),
                        explanation=f"As of game start, {context.names[player_id]} was {wins}W–{games - wins}L against {context.names[opponent_id]} ({rate:.1%}).",
                        identity=f"player:{opponent_id}",
                    )

    def _build_economy_and_tip_questions(self, context: _Context) -> None:
        balances = {
            player_id: _as_int(row.get("balance", row.get("jopacoin_balance", 0)))
            for player_id, row in context.player_rows.items()
        }
        self._add_player_leader(
            context,
            key="economy:highest_balance",
            category="economy",
            text="Who has the highest current Jopacoin balance?",
            values=balances,
            explanation=lambda name, value: f"As of game start, {name} had {int(value):,} JC.",
        )
        self._add_player_leader(
            context,
            key="economy:lowest_balance",
            category="economy",
            text="Who has the lowest current Jopacoin balance?",
            values=balances,
            lowest=True,
            explanation=lambda name, value: f"As of game start, {name} had {int(value):,} JC.",
            spicy=True,
        )
        lowest_ever = {
            player_id: _as_int(row.get("lowest_balance_ever", row.get("balance", 0)))
            for player_id, row in context.player_rows.items()
        }
        self._add_player_leader(
            context,
            key="economy:deepest_historical_debt",
            category="economy",
            text="Who has recorded the deepest all-time Jopacoin debt?",
            values=lowest_ever,
            lowest=True,
            explanation=lambda name, value: (
                f"As of game start, {name}'s lowest recorded balance was {int(value):,} JC."
            ),
            spicy=True,
        )
        balance_options = {player_id: f"{balance:,} JC" for player_id, balance in balances.items()}
        for player_id in sorted(balance_options):
            self._add_value_question(
                context,
                key=f"economy:balance:{player_id}",
                category="economy",
                text=f"What was {context.names[player_id]}'s Jopacoin balance when this game began?",
                correct=balance_options[player_id],
                distractors=(
                    value for other_id, value in balance_options.items() if other_id != player_id
                ),
                explanation=f"At the question snapshot, {context.names[player_id]} had {balance_options[player_id]}.",
                identity=f"player:{player_id}",
            )

        bankruptcies = {
            _as_int(row.get("discord_id")): _as_int(row.get("bankruptcy_count"))
            for row in context.snapshot.get("bankruptcies", [])
            if _as_int(row.get("discord_id")) in context.names
        }
        self._add_player_leader(
            context,
            key="economy:most_bankruptcies",
            category="economy",
            text="Who has the most recorded bankruptcy declarations?",
            values=bankruptcies,
            minimum=1,
            explanation=lambda name,
            value: f"As of game start, {name} had declared bankruptcy {int(value)} times.",
        )

        sent_amount: Counter[int] = Counter()
        sent_count: Counter[int] = Counter()
        received_amount: Counter[int] = Counter()
        received_count: Counter[int] = Counter()
        for row in context.snapshot.get("tips", []):
            sender = _as_int(row.get("sender_id"))
            recipient = _as_int(row.get("recipient_id"))
            amount = max(0, _as_int(row.get("amount")))
            if sender in context.names:
                sent_amount[sender] += amount
                sent_count[sender] += 1
            if recipient in context.names:
                received_amount[recipient] += amount
                received_count[recipient] += 1
        self._add_player_leader(
            context,
            key="tips:most_jc_sent",
            category="tips",
            text="Who has sent the most total JC through tips?",
            values=sent_amount,
            minimum=1,
            explanation=lambda name,
            value: f"As of game start, {name} had sent {int(value):,} JC in tips.",
        )
        self._add_player_leader(
            context,
            key="tips:most_transactions_sent",
            category="tips",
            text="Who has sent the most individual tip transactions?",
            values=sent_count,
            minimum=2,
            explanation=lambda name, value: f"As of game start, {name} had sent {int(value)} tips.",
        )
        self._add_player_leader(
            context,
            key="tips:most_transactions_received",
            category="tips",
            text="Who has received the most individual tip transactions?",
            values=received_count,
            minimum=2,
            explanation=lambda name,
            value: f"As of game start, {name} had received {int(value)} tips.",
        )
        self._add_player_leader(
            context,
            key="tips:most_jc_received",
            category="tips",
            text="Who has received the most total JC through tips?",
            values=received_amount,
            minimum=1,
            explanation=lambda name,
            value: f"As of game start, {name} had received {int(value):,} JC in tips.",
        )

    # ------------------------------------------------------------------
    # Gambling and prediction candidates
    # ------------------------------------------------------------------

    def _build_betting_questions(self, context: _Context) -> None:
        stats: dict[int, dict[str, int]] = defaultdict(
            lambda: {"bets": 0, "wins": 0, "losses": 0, "wagered": 0, "pnl": 0}
        )
        for row in context.snapshot.get("bets", []):
            player_id = _as_int(row.get("discord_id"))
            winning_team = _as_int(row.get("winning_team"))
            if player_id not in context.names or winning_team not in (1, 2):
                continue
            amount = max(0, _as_int(row.get("amount")))
            leverage = max(1, _as_int(row.get("leverage"), 1))
            effective = max(0, _as_int(row.get("effective_bet"), amount * leverage))
            side = str(row.get("team_bet_on") or "").lower()
            won = (side == "radiant" and winning_team == 1) or (
                side == "dire" and winning_team == 2
            )
            payout = _as_int(row.get("payout"), 0)
            player = stats[player_id]
            player["bets"] += 1
            player["wins"] += int(won)
            player["losses"] += int(not won)
            player["wagered"] += effective
            player["pnl"] += payout - effective
        bet_counts = {player_id: row["bets"] for player_id, row in stats.items()}
        wagered = {
            player_id: row["wagered"] for player_id, row in stats.items() if row["bets"] >= 5
        }
        rates = {
            player_id: row["wins"] / row["bets"]
            for player_id, row in stats.items()
            if row["bets"] >= 5
        }
        losses = {
            player_id: max(0, -row["pnl"]) for player_id, row in stats.items() if row["bets"] >= 5
        }
        self._add_player_leader(
            context,
            key="betting:most_bets",
            category="betting",
            text="Who has placed the most settled match bets?",
            values=bet_counts,
            minimum=5,
            explanation=lambda name,
            value: f"As of game start, {name} had {int(value)} settled bets.",
        )
        self._add_player_leader(
            context,
            key="betting:most_wagered",
            category="betting",
            text="Who has wagered the most total leveraged JC on settled matches?",
            values=wagered,
            minimum=1,
            explanation=lambda name,
            value: f"As of game start, {name} had wagered {int(value):,} leveraged JC.",
        )
        self._add_player_leader(
            context,
            key="betting:highest_win_rate",
            category="betting",
            text="Among players with at least five settled bets, who has the best betting win rate?",
            values=rates,
            explanation=lambda name,
            value: f"As of game start, {name}'s settled-bet win rate was {value:.1%}.",
            displayed=lambda value: round(value * 100, 1),
        )
        self._add_player_leader(
            context,
            key="betting:largest_loss",
            category="betting",
            text="Who has the largest cumulative loss on settled match bets?",
            values=losses,
            minimum=1,
            explanation=lambda name,
            value: f"As of game start, {name} was down {int(value):,} JC on settled bets.",
            spicy=True,
        )

    def _build_wheel_questions(self, context: _Context) -> None:
        rows = [
            row
            for row in context.snapshot.get("wheel_spins", [])
            if _as_int(row.get("discord_id")) in context.names
        ]
        counts = Counter(_as_int(row.get("discord_id")) for row in rows)
        self._add_player_leader(
            context,
            key="wheel:most_spins",
            category="wheel",
            text="Who has the most recorded Wheel of Fortune spins?",
            values=counts,
            minimum=2,
            explanation=lambda name,
            value: f"As of game start, {name} had {int(value)} recorded wheel spins.",
        )

        coded_rows = [row for row in rows if str(row.get("outcome_code") or "").strip()]
        all_codes = {_humanize_code(row.get("outcome_code")) for row in coded_rows}
        by_player: dict[int, Counter[str]] = defaultdict(Counter)
        by_code: dict[str, list[dict[str, Any]]] = defaultdict(list)
        for row in coded_rows:
            player_id = _as_int(row.get("discord_id"))
            code = _humanize_code(row.get("outcome_code"))
            by_player[player_id][code] += 1
            by_code[code].append(row)

        for player_id, outcomes in sorted(by_player.items()):
            ordered = outcomes.most_common()
            if sum(outcomes.values()) < 3 or not ordered:
                continue
            if len(ordered) > 1 and ordered[0][1] == ordered[1][1]:
                continue
            code, times = ordered[0]
            self._add_value_question(
                context,
                key=f"wheel:most_common_outcome:{player_id}",
                category="wheel",
                text=f"Which exact wheel outcome has {context.names[player_id]} recorded most often?",
                correct=code,
                distractors=(other for other in all_codes if other != code),
                explanation=f"As of game start, {context.names[player_id]} had landed {code} {times} times.",
                identity=f"player:{player_id}",
            )

        for code, outcome_rows in sorted(by_code.items()):
            code_key = re.sub(r"[^a-z0-9]+", "-", code.casefold()).strip("-")[:48]
            exact_counts = {
                player_id: sum(_as_int(row.get("discord_id")) == player_id for row in outcome_rows)
                for player_id in context.names
            }
            self._add_player_leader(
                context,
                key=f"wheel:exact_outcome_leader:{code_key}",
                category="wheel",
                text=f"Who has recorded the exact wheel outcome {code} most often?",
                values=exact_counts,
                minimum=2,
                explanation=lambda name, value, code=code: (
                    f"As of game start, {name} had recorded {code} {int(value)} times."
                ),
            )
            rendered_counts = {
                player_id: f"{count:,} time" if count == 1 else f"{count:,} times"
                for player_id, count in exact_counts.items()
            }
            for player_id, count in sorted(exact_counts.items()):
                if count < 2:
                    continue
                correct = rendered_counts[player_id]
                distractor_counts = {
                    other_count
                    for other_id, other_count in exact_counts.items()
                    if other_id != player_id and other_count != count
                }
                # Rare outcomes may have one frequent winner and a field of
                # zeroes. Multiple-choice counts can still use nearby false
                # values while the immutable explanation preserves the actual
                # snapshot count.
                for plausible in (
                    count - 1,
                    count + 1,
                    count // 2,
                    count * 2,
                    count - max(2, count // 10),
                    count + max(2, count // 10),
                    0,
                ):
                    if plausible >= 0 and plausible != count:
                        distractor_counts.add(plausible)
                    if len(distractor_counts) >= 3:
                        break
                self._add_value_question(
                    context,
                    key=f"wheel:exact_outcome_count:{code_key}:{player_id}",
                    category="wheel",
                    text=(
                        f"How many times has {context.names[player_id]} recorded "
                        f"the exact wheel outcome {code}?"
                    ),
                    correct=correct,
                    distractors=(
                        f"{value:,} time" if value == 1 else f"{value:,} times"
                        for value in distractor_counts
                    ),
                    explanation=(
                        f"At the question snapshot, {context.names[player_id]} "
                        f"had recorded {code} {correct}."
                    ),
                    identity=f"player:{player_id}",
                )

        for code, outcome_rows in sorted(by_code.items()):
            ordered = sorted(
                outcome_rows,
                key=lambda row: (
                    _as_int(row.get("spin_time")),
                    _as_int(row.get("spin_id")),
                ),
                reverse=True,
            )
            latest = ordered[0]
            if len(ordered) > 1 and (
                _as_int(latest.get("spin_time")),
                _as_int(latest.get("spin_id")),
            ) == (
                _as_int(ordered[1].get("spin_time")),
                _as_int(ordered[1].get("spin_id")),
            ):
                continue
            player_id = _as_int(latest.get("discord_id"))
            self._add_value_question(
                context,
                key=f"wheel:latest_exact_outcome:{str(latest.get('outcome_code')).lower()}",
                category="wheel",
                text=f"Who most recently recorded the exact wheel outcome {code}?",
                correct=context.names[player_id],
                distractors=(
                    name for other_id, name in context.names.items() if other_id != player_id
                ),
                explanation=f"The latest recorded {code} outcome belonged to {context.names[player_id]}.",
                identity=f"player:{player_id}",
            )

    def _build_double_questions(self, context: _Context) -> None:
        attempts: Counter[int] = Counter()
        wins: Counter[int] = Counter()
        for row in context.snapshot.get("double_spins", []):
            player_id = _as_int(row.get("discord_id"))
            if player_id not in context.names:
                continue
            attempts[player_id] += 1
            wins[player_id] += int(_truthy(row.get("won")))
        self._add_player_leader(
            context,
            key="double:most_attempts",
            category="double-or-nothing",
            text="Who has attempted Double or Nothing most often?",
            values=attempts,
            minimum=2,
            explanation=lambda name,
            value: f"As of game start, {name} had {int(value)} Double-or-Nothing attempts.",
        )
        self._add_player_leader(
            context,
            key="double:most_wins",
            category="double-or-nothing",
            text="Who has the most recorded Double-or-Nothing wins?",
            values=wins,
            minimum=1,
            explanation=lambda name,
            value: f"As of game start, {name} had won Double or Nothing {int(value)} times.",
        )
        rates = {
            player_id: wins[player_id] / total
            for player_id, total in attempts.items()
            if total >= 5
        }
        self._add_player_leader(
            context,
            key="double:highest_win_rate",
            category="double-or-nothing",
            text="Among players with five attempts, who has the best Double-or-Nothing win rate?",
            values=rates,
            explanation=lambda name,
            value: f"As of game start, {name}'s Double-or-Nothing win rate was {value:.1%}.",
            displayed=lambda value: round(value * 100, 1),
        )

    def _build_prediction_questions(self, context: _Context) -> None:
        positions: list[dict[str, Any]] = []
        market_names: dict[int, str] = {}
        market_status: dict[int, str] = {}
        for row in context.snapshot.get("prediction_positions", []):
            player_id = _as_int(row.get("discord_id"))
            prediction_id = _as_int(row.get("prediction_id"))
            status = str(row.get("status") or "").lower()
            if player_id not in context.names or prediction_id <= 0 or status != "resolved":
                continue
            outcome = str(row.get("outcome") or "").lower()
            if outcome not in {"yes", "no"}:
                continue
            winning_contracts = _as_int(
                row.get("yes_contracts") if outcome == "yes" else row.get("no_contracts")
            )
            cost = _as_int(row.get("yes_cost_basis_total")) + _as_int(
                row.get("no_cost_basis_total")
            )
            penalty = _as_int(row.get("bankruptcy_penalty"))
            pnl = winning_contracts * PREDICTION_CONTRACT_VALUE - cost - penalty
            # Prediction prompts are user-authored free text and intentionally
            # absent from the safe read model. Stable numeric labels let us ask
            # about a resolved market without exposing its creator or wording.
            name = f"Market #{prediction_id}"
            enriched = dict(row)
            enriched["_pnl"] = pnl
            enriched["_market_name"] = name
            positions.append(enriched)
            market_names[prediction_id] = name
            market_status[prediction_id] = status

        by_player: dict[int, list[dict[str, Any]]] = defaultdict(list)
        for row in positions:
            by_player[_as_int(row.get("discord_id"))].append(row)
        wins = {
            player_id: sum(row["_pnl"] > 0 for row in rows)
            for player_id, rows in by_player.items()
            if len(rows) >= 3
        }
        losses = {
            player_id: sum(row["_pnl"] < 0 for row in rows)
            for player_id, rows in by_player.items()
            if len(rows) >= 3
        }
        total_pnl = {
            player_id: sum(row["_pnl"] for row in rows)
            for player_id, rows in by_player.items()
            if len(rows) >= 3
        }
        self._add_player_leader(
            context,
            key="predictions:most_wins",
            category="predictions",
            text="Among players with three resolved positions, who has the most profitable prediction markets?",
            values=wins,
            minimum=1,
            explanation=lambda name,
            value: f"As of game start, {name} had finished profitable on {int(value)} resolved markets.",
        )
        self._add_player_leader(
            context,
            key="predictions:best_total_pnl",
            category="predictions",
            text="Among players with three resolved positions, who has the highest total prediction P&L?",
            values=total_pnl,
            explanation=lambda name,
            value: f"As of game start, {name}'s resolved prediction P&L was {int(value):+,} JC.",
        )
        self._add_player_leader(
            context,
            key="predictions:most_losses",
            category="predictions",
            text="Who has lost JC on the most resolved prediction markets?",
            values=losses,
            minimum=1,
            explanation=lambda name,
            value: f"As of game start, {name} had finished negative on {int(value)} resolved markets.",
            spicy=True,
        )

        all_markets = set(market_names.values())
        for player_id, rows in sorted(by_player.items()):
            if len(rows) < 3:
                continue
            losing_names = {row["_market_name"] for row in rows if row["_pnl"] < 0}
            winning_names = {row["_market_name"] for row in rows if row["_pnl"] > 0}
            non_losses = all_markets - losing_names
            non_wins = all_markets - winning_names
            for row in sorted(rows, key=lambda item: _as_int(item.get("prediction_id"))):
                market_name = row["_market_name"]
                prediction_id = _as_int(row.get("prediction_id"))
                if row["_pnl"] < 0:
                    self._add_value_question(
                        context,
                        key=f"predictions:loss_market:{player_id}:{prediction_id}",
                        category="predictions",
                        text=f"Which resolved market did {context.names[player_id]} finish with a loss on?",
                        correct=market_name,
                        distractors=non_losses,
                        explanation=f"As of game start, {context.names[player_id]} had a {int(row['_pnl']):+,} JC result on {market_name}.",
                        identity=f"player:{player_id}",
                    )
                elif row["_pnl"] > 0:
                    self._add_value_question(
                        context,
                        key=f"predictions:win_market:{player_id}:{prediction_id}",
                        category="predictions",
                        text=f"Which resolved market did {context.names[player_id]} finish with a profit on?",
                        correct=market_name,
                        distractors=non_wins,
                        explanation=f"As of game start, {context.names[player_id]} had a +{int(row['_pnl']):,} JC result on {market_name}.",
                        identity=f"player:{player_id}",
                    )

        resolved_ids = set(market_names)
        trade_count: Counter[int] = Counter()
        trade_volume: Counter[int] = Counter()
        for row in context.snapshot.get("prediction_trades", []):
            player_id = _as_int(row.get("discord_id"))
            prediction_id = _as_int(row.get("prediction_id"))
            status = str(row.get("status") or market_status.get(prediction_id, "")).lower()
            if len(by_player.get(player_id, [])) < 3 or (
                prediction_id not in resolved_ids and status != "resolved"
            ):
                continue
            trade_count[player_id] += 1
            trade_volume[player_id] += max(0, _as_int(row.get("contracts")))
        self._add_player_leader(
            context,
            key="predictions:most_trades",
            category="predictions",
            text="Who has made the most trades in resolved prediction markets?",
            values=trade_count,
            minimum=3,
            explanation=lambda name,
            value: f"As of game start, {name} had made {int(value)} trades in resolved markets.",
        )
        self._add_player_leader(
            context,
            key="predictions:most_contract_volume",
            category="predictions",
            text="Who has traded the most contracts in resolved prediction markets?",
            values=trade_volume,
            minimum=1,
            explanation=lambda name,
            value: f"As of game start, {name} had traded {int(value):,} resolved-market contracts.",
        )

    # ------------------------------------------------------------------
    # Dig, Mafia, legacy trivia, and other public-profile candidates
    # ------------------------------------------------------------------

    def _build_dig_questions(self, context: _Context) -> None:
        tunnels = {
            _as_int(row.get("discord_id")): row
            for row in context.snapshot.get("tunnels", [])
            if _as_int(row.get("discord_id")) in context.names
        }
        if not tunnels:
            return

        leader_fields = (
            (
                "total_digs",
                "most_digs",
                "Who has made the most recorded digs?",
                "recorded digs",
                5,
            ),
            (
                "max_depth",
                "deepest_tunnel",
                "Who has reached the greatest all-time tunnel depth?",
                "maximum depth",
                1,
            ),
            (
                "total_jc_earned",
                "most_jc_earned",
                "Who has earned the most total JC from digging?",
                "JC earned from digging",
                1,
            ),
            (
                "prestige_level",
                "highest_prestige",
                "Who has the highest Dig prestige level?",
                "Dig prestige level",
                1,
            ),
            (
                "best_run_score",
                "best_run_score",
                "Who has the highest recorded Dig run score?",
                "best Dig run score",
                1,
            ),
        )
        for field, suffix, text, label, minimum in leader_fields:
            values = {player_id: _as_int(row.get(field)) for player_id, row in tunnels.items()}
            self._add_player_leader(
                context,
                key=f"dig:{suffix}",
                category="dig",
                text=text,
                values=values,
                minimum=minimum,
                explanation=lambda name, value, label=label: (
                    f"As of game start, {name}'s {label} was {int(value):,}."
                ),
            )

        profile_fields = (
            ("total_digs", "total recorded dig count", "digs", "{:,}"),
            ("max_depth", "all-time maximum tunnel depth", "blocks", "{:,}"),
            ("prestige_level", "Dig prestige level", "levels", "{:,}"),
            ("pickaxe_tier", "current pickaxe tier", "tiers", "{:,}"),
            ("total_jc_earned", "all-time JC earned from Dig", "JC", "{:,}"),
            ("best_run_score", "best Dig run score", "points", "{:,}"),
            ("stat_strength", "Dig Strength stat", "points", "{:,}"),
            ("stat_smarts", "Dig Smarts stat", "points", "{:,}"),
            ("stat_stamina", "Dig Stamina stat", "points", "{:,}"),
        )
        for player_id, row in sorted(tunnels.items()):
            for field, label, unit, template in profile_fields:
                value = _as_int(row.get(field))
                correct = f"{template.format(value)} {unit}"
                alternatives = {
                    f"{template.format(_as_int(other.get(field)))} {unit}"
                    for other_id, other in tunnels.items()
                    if other_id != player_id
                }
                self._add_value_question(
                    context,
                    key=f"dig:profile:{field}:{player_id}",
                    category="dig",
                    text=f"What was {context.names[player_id]}'s {label} when this game began?",
                    correct=correct,
                    distractors=alternatives,
                    explanation=(
                        f"At the question snapshot, {context.names[player_id]}'s "
                        f"{label} was {correct}."
                    ),
                    identity=f"player:{player_id}",
                )

        artifact_counts: Counter[int] = Counter()
        relic_counts: Counter[int] = Counter()
        for row in context.snapshot.get("dig_artifacts", []):
            player_id = _as_int(row.get("discord_id"))
            if player_id not in context.names:
                continue
            artifact_counts[player_id] += 1
            relic_counts[player_id] += int(_truthy(row.get("is_relic")))
        achievement_counts = Counter(
            _as_int(row.get("discord_id"))
            for row in context.snapshot.get("dig_achievements", [])
            if _as_int(row.get("discord_id")) in context.names
        )
        self._add_player_leader(
            context,
            key="dig:most_artifacts",
            category="dig",
            text="Who has found the most recorded Dig artifacts?",
            values=artifact_counts,
            minimum=1,
            explanation=lambda name, value: (
                f"As of game start, {name} had found {int(value)} Dig artifacts."
            ),
        )
        self._add_player_leader(
            context,
            key="dig:most_relics",
            category="dig",
            text="Who has found the most recorded Dig relics?",
            values=relic_counts,
            minimum=1,
            explanation=lambda name, value: (
                f"As of game start, {name} had found {int(value)} Dig relics."
            ),
        )
        self._add_player_leader(
            context,
            key="dig:most_achievements",
            category="dig",
            text="Who has unlocked the most Dig achievements?",
            values=achievement_counts,
            minimum=1,
            explanation=lambda name, value: (
                f"As of game start, {name} had unlocked {int(value)} Dig achievements."
            ),
        )

        # The read model intentionally omits private action detail. A negative
        # depth delta is the public, structured signal that a logged dig/event
        # involved a cave-in or equivalent collapse.
        cave_ins: Counter[int] = Counter()
        for row in context.snapshot.get("dig_actions", []):
            player_id = _as_int(row.get("actor_id"))
            action_type = str(row.get("action_type") or "").lower()
            before = _as_int(row.get("depth_before"))
            after = _as_int(row.get("depth_after"))
            explicit_cave_in = action_type in {"cave_in", "cave-in", "collapse"}
            inferred_cave_in = action_type in {"dig", "dig_action", "event"} and after < before
            if player_id in context.names and (explicit_cave_in or inferred_cave_in):
                cave_ins[player_id] += 1
        self._add_player_leader(
            context,
            key="dig:most_cave_ins",
            category="dig",
            text="Who has the most recorded Dig cave-ins or depth-losing collapses?",
            values=cave_ins,
            minimum=1,
            explanation=lambda name, value: (
                f"As of game start, {name} had {int(value)} logged cave-ins or "
                "Dig actions whose depth decreased."
            ),
            spicy=True,
        )

    @staticmethod
    def _mafia_role_won(role: str, winner: str) -> bool:
        if winner == "TOWN":
            return role in _TOWN_MAFIA_ROLES
        if winner == "MAFIA":
            return role == "MAFIA"
        if winner == "JESTER":
            return role == "JESTER"
        return False

    def _build_mafia_questions(self, context: _Context) -> None:
        games: dict[int, dict[str, Any]] = {}
        for row in context.snapshot.get("mafia_games", []):
            game_id = _as_int(row.get("game_id"))
            phase = str(row.get("phase") or "").upper()
            status = str(row.get("status") or "").upper()
            winner = str(row.get("winner") or "").upper()
            if (
                game_id > 0
                and phase == "RESOLVED"
                and status != "CANCELLED"
                and winner in {"TOWN", "MAFIA", "JESTER"}
            ):
                games[game_id] = row
        if not games:
            return

        games_played: Counter[int] = Counter()
        wins: Counter[int] = Counter()
        role_counts: dict[int, Counter[str]] = defaultdict(Counter)
        for row in context.snapshot.get("mafia_players", []):
            player_id = _as_int(row.get("discord_id"))
            game_id = _as_int(row.get("game_id"))
            if player_id not in context.names or game_id not in games:
                continue
            role = str(row.get("role") or "").upper()
            winner = str(games[game_id].get("winner") or "").upper()
            games_played[player_id] += 1
            wins[player_id] += int(self._mafia_role_won(role, winner))
            if role:
                role_counts[player_id][role] += 1

        mvp_counts = Counter(
            _as_int(row.get("mvp_id"))
            for row in games.values()
            if games_played[_as_int(row.get("mvp_id"))] >= 3
        )
        self._add_player_leader(
            context,
            key="mafia:most_games",
            category="mafia",
            text="Who has played the most resolved, non-cancelled Mafia games?",
            values=games_played,
            minimum=3,
            explanation=lambda name, value: (
                f"As of game start, {name} had played {int(value)} resolved Mafia games."
            ),
        )
        qualifying_wins = {
            player_id: wins[player_id] for player_id, total in games_played.items() if total >= 3
        }
        self._add_player_leader(
            context,
            key="mafia:most_wins",
            category="mafia",
            text="Among players with three resolved Mafia games, who has the most wins?",
            values=qualifying_wins,
            minimum=1,
            explanation=lambda name, value: (
                f"As of game start, {name} had won {int(value)} resolved Mafia games."
            ),
        )
        self._add_player_leader(
            context,
            key="mafia:most_mvp_awards",
            category="mafia",
            text="Who has earned the most Mafia MVP awards?",
            values=mvp_counts,
            minimum=1,
            explanation=lambda name, value: (
                f"As of game start, {name} had earned {int(value)} Mafia MVP awards."
            ),
        )

        all_roles = {_humanize_code(role) for counts in role_counts.values() for role in counts}
        for player_id, counts in sorted(role_counts.items()):
            if games_played[player_id] < 3:
                continue
            ordered = counts.most_common()
            if not ordered or (len(ordered) > 1 and ordered[0][1] == ordered[1][1]):
                continue
            role, appearances = ordered[0]
            role_name = _humanize_code(role)
            self._add_value_question(
                context,
                key=f"mafia:most_common_role:{player_id}",
                category="mafia",
                text=f"Which Mafia role has {context.names[player_id]} received most often?",
                correct=role_name,
                distractors=(other for other in all_roles if other != role_name),
                explanation=(
                    f"Across resolved, non-cancelled games, {context.names[player_id]} "
                    f"had the {role_name} role {appearances} times."
                ),
                identity=f"player:{player_id}",
            )

        action_labels = {
            "KILL": "Mafia kill choices",
            "VIG_KILL": "Vigilante kill choices",
            "SAVE": "Doctor save choices",
            "INVESTIGATE": "Detective investigations",
            "VOTE": "Mafia votes",
        }
        action_counts: dict[str, Counter[int]] = defaultdict(Counter)
        for row in context.snapshot.get("mafia_actions", []):
            player_id = _as_int(row.get("actor_id"))
            game_id = _as_int(row.get("game_id"))
            action_type = str(row.get("action_type") or "").upper()
            if (
                player_id in context.names
                and game_id in games
                and action_type in action_labels
                and games_played[player_id] >= 3
            ):
                action_counts[action_type][player_id] += 1
        for action_type, label in action_labels.items():
            self._add_player_leader(
                context,
                key=f"mafia:most_{action_type.lower()}_actions",
                category="mafia",
                text=f"Who has made the most recorded {label} in resolved Mafia games?",
                values=action_counts[action_type],
                minimum=1,
                explanation=lambda name, value, label=label: (
                    f"As of game start, {name} had made {int(value)} recorded {label}."
                ),
            )

    def _build_trivia_questions(self, context: _Context) -> None:
        sessions: Counter[int] = Counter()
        total_earned: Counter[int] = Counter()
        best_streak: dict[int, int] = defaultdict(int)
        for row in context.snapshot.get("trivia_sessions", []):
            player_id = _as_int(row.get("discord_id"))
            if player_id not in context.names:
                continue
            sessions[player_id] += 1
            total_earned[player_id] += _as_int(row.get("jc_earned"))
            best_streak[player_id] = max(best_streak[player_id], max(0, _as_int(row.get("streak"))))
        self._add_player_leader(
            context,
            key="trivia:most_sessions",
            category="trivia",
            text="Who has played the most recorded trivia sessions?",
            values=sessions,
            minimum=2,
            explanation=lambda name, value: (
                f"As of game start, {name} had played {int(value)} trivia sessions."
            ),
        )
        self._add_player_leader(
            context,
            key="trivia:most_jc_earned",
            category="trivia",
            text="Who has earned the most total JC from recorded trivia sessions?",
            values=total_earned,
            minimum=1,
            explanation=lambda name, value: (
                f"As of game start, {name} had earned {int(value):,} JC from trivia."
            ),
        )
        self._add_player_leader(
            context,
            key="trivia:best_streak",
            category="trivia",
            text="Who has the longest recorded trivia answer streak?",
            values=best_streak,
            minimum=2,
            explanation=lambda name, value: (
                f"As of game start, {name}'s best recorded trivia streak was {int(value)}."
            ),
        )

    def _build_disbursement_questions(self, context: _Context) -> None:
        # A non-null finalized timestamp makes this immutable history, rather
        # than a live ballot. The structured method/outcome labels are public;
        # no proposal text is present in the read model.
        rows = [
            row
            for row in context.snapshot.get("disburse_vote_history", [])
            if _as_int(row.get("discord_id")) in context.names
            and _date_key(row.get("finalized_at")) > 0
            and str(row.get("proposal_outcome") or "").strip()
        ]
        ballot_counts = Counter(_as_int(row.get("discord_id")) for row in rows)
        self._add_player_leader(
            context,
            key="disburse:most_finalized_ballots",
            category="disbursement",
            text="Who has cast the most ballots in finalized nonprofit disbursements?",
            values=ballot_counts,
            minimum=2,
            explanation=lambda name, value: (
                f"As of game start, {name} had cast {int(value)} finalized disbursement ballots."
            ),
        )

        methods_by_player: dict[int, Counter[str]] = defaultdict(Counter)
        all_methods: set[str] = set()
        for row in rows:
            method = _humanize_code(row.get("vote_method"))
            if not method:
                continue
            player_id = _as_int(row.get("discord_id"))
            methods_by_player[player_id][method] += 1
            all_methods.add(method)
        for player_id, methods in sorted(methods_by_player.items()):
            ordered = methods.most_common()
            if len(methods) == 0 or sum(methods.values()) < 3:
                continue
            if len(ordered) > 1 and ordered[0][1] == ordered[1][1]:
                continue
            method, votes = ordered[0]
            self._add_value_question(
                context,
                key=f"disburse:most_common_vote:{player_id}",
                category="disbursement",
                text=f"Which finalized disbursement method has {context.names[player_id]} voted for most often?",
                correct=method,
                distractors=(other for other in all_methods if other != method),
                explanation=(
                    f"As of game start, {context.names[player_id]} had cast {votes} "
                    f"finalized ballots for {method}."
                ),
                identity=f"player:{player_id}",
            )

        outcomes = Counter(_humanize_code(row.get("proposal_outcome")) for row in rows)
        if len(outcomes) >= 4:
            ordered = outcomes.most_common()
            if len(ordered) == 1 or ordered[0][1] > ordered[1][1]:
                outcome, proposals = ordered[0]
                self._add_value_question(
                    context,
                    key="disburse:most_common_final_outcome",
                    category="disbursement",
                    text="Which outcome has finalized most often in recorded nonprofit disbursements?",
                    correct=outcome,
                    distractors=(other for other in outcomes if other != outcome),
                    explanation=(
                        f"As of game start, {outcome} had finalized most often, "
                        f"appearing on {proposals} recorded ballots."
                    ),
                    identity=f"disburse:{outcome.casefold()}",
                )

    def _build_protected_hero_questions(self, context: _Context) -> None:
        rows = [
            row
            for row in context.snapshot.get("protected_heroes", [])
            if _as_int(row.get("discord_id")) in context.names
            and str(row.get("status") or "").lower() == "recorded"
            and _as_int(row.get("hero_id")) > 0
        ]
        purchase_counts = Counter(_as_int(row.get("discord_id")) for row in rows)
        self._add_player_leader(
            context,
            key="protected-heroes:most_purchases",
            category="protected heroes",
            text="Who has bought hero protection for the most recorded matches?",
            values=purchase_counts,
            minimum=1,
            explanation=lambda name, value: (
                f"As of game start, {name} had {int(value)} recorded protect-hero purchases."
            ),
        )

        heroes_by_player: dict[int, Counter[int]] = defaultdict(Counter)
        all_heroes: set[str] = set()
        for row in rows:
            player_id = _as_int(row.get("discord_id"))
            hero_id = _as_int(row.get("hero_id"))
            heroes_by_player[player_id][hero_id] += 1
            all_heroes.add(self._hero_name(hero_id))
        for player_id, heroes in sorted(heroes_by_player.items()):
            ordered = heroes.most_common()
            if not ordered or (len(ordered) > 1 and ordered[0][1] == ordered[1][1]):
                continue
            hero_id, purchases = ordered[0]
            hero_name = self._hero_name(hero_id)
            self._add_value_question(
                context,
                key=f"protected-heroes:most_common:{player_id}",
                category="protected heroes",
                text=f"Which hero has {context.names[player_id]} protected most often?",
                correct=hero_name,
                distractors=(other for other in all_heroes if other != hero_name),
                explanation=(
                    f"As of game start, {context.names[player_id]} had protected "
                    f"{hero_name} {purchases} times in recorded matches."
                ),
                identity=f"player:{player_id}",
            )
