"""Persistence and read-model access for player-history trivia."""

from __future__ import annotations

import json
import time
from typing import Any

from repositories.base_repository import BaseRepository


class PlayerTriviaRepository(BaseRepository):
    """Store immutable trivia rounds and expose a safe guild data snapshot."""

    _FINISH_STATUSES = frozenset({"completed", "timed_out", "error"})

    def load_snapshot(self, guild_id: int | None) -> dict[str, list[dict]]:
        """Load deterministic, guild-scoped rows used to generate trivia.

        The snapshot deliberately omits private/free-text fields such as match
        notes, miner backstories, action details, and enrichment JSON. Resolved
        prediction questions are included so trivia choices can identify their
        market without exposing creator IDs. Filtering to current Discord guild
        members is a service-layer concern because repositories do not have
        Discord state.
        """
        guild_id = self.normalize_guild_id(guild_id)

        queries: list[tuple[str, str]] = [
            (
                "players",
                """
                SELECT
                    guild_id,
                    discord_id,
                    discord_username AS username,
                    COALESCE(wins, 0) AS wins,
                    COALESCE(losses, 0) AS losses,
                    COALESCE(jopacoin_balance, 0) AS balance,
                    glicko_rating,
                    glicko_rd,
                    glicko_volatility,
                    os_mu,
                    os_sigma,
                    preferred_roles,
                    main_role,
                    lowest_balance_ever,
                    first_calibrated_at,
                    created_at,
                    last_match_date,
                    COALESCE(personal_best_win_streak, 0) AS personal_best_win_streak,
                    COALESCE(total_bets_placed, 0) AS total_bets_placed,
                    COALESCE(dota_streak_days, 0) AS dota_streak_days
                FROM players
                WHERE guild_id = ?
                ORDER BY discord_id ASC
                """,
            ),
            (
                "participants",
                """
                SELECT
                    mp.guild_id,
                    mp.match_id,
                    mp.discord_id,
                    mp.team_number,
                    mp.won,
                    mp.side,
                    mp.hero_id,
                    mp.kills,
                    mp.deaths,
                    mp.assists,
                    mp.gpm,
                    mp.xpm,
                    mp.hero_damage,
                    mp.tower_damage,
                    mp.last_hits,
                    mp.denies,
                    mp.net_worth,
                    mp.hero_healing,
                    mp.lane_role,
                    mp.lane_efficiency,
                    mp.towers_killed,
                    mp.roshans_killed,
                    mp.teamfight_participation,
                    mp.obs_placed,
                    mp.sen_placed,
                    mp.camps_stacked,
                    mp.rune_pickups,
                    mp.firstblood_claimed,
                    mp.stuns,
                    mp.fantasy_points,
                    mp.bonus_jc,
                    m.match_date,
                    m.lobby_type,
                    m.winning_team,
                    m.betting_mode,
                    m.balancing_rating_system
                FROM match_participants mp
                JOIN matches m
                  ON m.match_id = mp.match_id
                 AND m.guild_id = mp.guild_id
                WHERE mp.guild_id = ?
                ORDER BY m.match_date ASC, mp.match_id ASC, mp.discord_id ASC
                """,
            ),
            (
                "ratings",
                """
                SELECT
                    rh.guild_id,
                    rh.id,
                    rh.discord_id,
                    rh.match_id,
                    rh.rating,
                    rh.rating_before,
                    rh.rd_before,
                    rh.rd_after,
                    rh.volatility_before,
                    rh.volatility_after,
                    rh.expected_team_win_prob,
                    rh.team_number,
                    rh.won,
                    rh.timestamp,
                    rh.os_mu_before,
                    rh.os_mu_after,
                    rh.os_sigma_before,
                    rh.os_sigma_after,
                    rh.fantasy_weight,
                    rh.streak_length,
                    rh.streak_multiplier,
                    m.match_date,
                    m.lobby_type
                FROM rating_history rh
                JOIN matches m
                  ON m.match_id = rh.match_id
                 AND m.guild_id = rh.guild_id
                WHERE rh.guild_id = ?
                ORDER BY m.match_date ASC, rh.match_id ASC, rh.id ASC
                """,
            ),
            (
                "pairings",
                """
                SELECT
                    guild_id,
                    player1_id,
                    player2_id,
                    COALESCE(games_together, 0) AS games_together,
                    COALESCE(wins_together, 0) AS wins_together,
                    COALESCE(games_against, 0) AS games_against,
                    COALESCE(player1_wins_against, 0) AS player1_wins_against,
                    last_match_id
                FROM player_pairings
                WHERE guild_id = ?
                ORDER BY player1_id ASC, player2_id ASC
                """,
            ),
            (
                "bankruptcies",
                """
                SELECT
                    guild_id,
                    discord_id,
                    last_bankruptcy_at,
                    COALESCE(penalty_games_remaining, 0) AS penalty_games_remaining,
                    COALESCE(bankruptcy_count, 0) AS bankruptcy_count
                FROM bankruptcy_state
                WHERE guild_id = ?
                ORDER BY discord_id ASC
                """,
            ),
            (
                "wheel_spins",
                """
                SELECT
                    guild_id,
                    spin_id,
                    discord_id,
                    result,
                    spin_time,
                    COALESCE(is_bankrupt, 0) AS is_bankrupt,
                    COALESCE(is_golden, 0) AS is_golden,
                    outcome_code,
                    COALESCE(is_bonus, 0) AS is_bonus,
                    event_id,
                    outcome_metadata
                FROM wheel_spins
                WHERE guild_id = ?
                ORDER BY spin_time ASC, spin_id ASC
                """,
            ),
            (
                "double_spins",
                """
                SELECT
                    guild_id,
                    spin_id,
                    discord_id,
                    cost,
                    balance_before,
                    balance_after,
                    won,
                    spin_time
                FROM double_or_nothing_spins
                WHERE guild_id = ?
                ORDER BY spin_time ASC, spin_id ASC
                """,
            ),
            (
                "tips",
                """
                SELECT
                    guild_id,
                    id,
                    sender_id,
                    recipient_id,
                    amount,
                    fee,
                    timestamp
                FROM tip_transactions
                WHERE guild_id = ?
                ORDER BY timestamp ASC, id ASC
                """,
            ),
            (
                "bets",
                """
                SELECT
                    b.guild_id,
                    b.bet_id,
                    b.discord_id,
                    b.match_id,
                    b.team_bet_on,
                    b.amount,
                    COALESCE(b.leverage, 1) AS leverage,
                    b.amount * COALESCE(b.leverage, 1) AS effective_bet,
                    b.bet_time,
                    b.payout,
                    COALESCE(b.is_blind, 0) AS is_blind,
                    b.odds_at_placement,
                    m.match_date,
                    m.winning_team,
                    m.betting_mode,
                    m.lobby_type
                FROM bets b
                JOIN matches m
                  ON m.match_id = b.match_id
                 AND m.guild_id = b.guild_id
                WHERE b.guild_id = ?
                  AND m.winning_team IN (1, 2)
                ORDER BY b.bet_time ASC, b.bet_id ASC
                """,
            ),
            (
                "prediction_positions",
                """
                SELECT
                    p.guild_id,
                    pp.prediction_id,
                    CASE WHEN p.status = 'resolved' THEN p.question END AS question,
                    pp.discord_id,
                    COALESCE(pp.yes_contracts, 0) AS yes_contracts,
                    COALESCE(pp.yes_cost_basis_total, 0) AS yes_cost_basis_total,
                    COALESCE(pp.no_contracts, 0) AS no_contracts,
                    COALESCE(pp.no_cost_basis_total, 0) AS no_cost_basis_total,
                    COALESCE(pp.bankruptcy_penalty, 0) AS bankruptcy_penalty,
                    p.status,
                    p.outcome,
                    p.created_at,
                    p.resolved_at,
                    p.current_price,
                    p.initial_fair
                FROM prediction_positions pp
                JOIN predictions p ON p.prediction_id = pp.prediction_id
                WHERE p.guild_id = ?
                ORDER BY pp.prediction_id ASC, pp.discord_id ASC
                """,
            ),
            (
                "prediction_trades",
                """
                SELECT
                    p.guild_id,
                    pt.trade_id,
                    pt.prediction_id,
                    pt.discord_id,
                    pt.action,
                    pt.contracts,
                    pt.jopacoins,
                    pt.vwap_x100,
                    pt.last_fill_price,
                    pt.trade_time,
                    p.status,
                    p.outcome,
                    p.resolved_at
                FROM prediction_trades pt
                JOIN predictions p ON p.prediction_id = pt.prediction_id
                WHERE p.guild_id = ?
                ORDER BY pt.trade_time ASC, pt.trade_id ASC
                """,
            ),
            (
                "tunnels",
                """
                SELECT
                    guild_id,
                    discord_id,
                    depth,
                    max_depth,
                    total_digs,
                    total_jc_earned,
                    last_dig_at,
                    streak_days,
                    pickaxe_tier,
                    prestige_level,
                    luminosity,
                    best_run_score,
                    current_run_jc,
                    current_run_artifacts,
                    current_run_events,
                    total_prestige_score,
                    stat_strength,
                    stat_smarts,
                    stat_stamina,
                    stat_points,
                    cavein_free_streak,
                    last_cheer_at
                FROM tunnels
                WHERE guild_id = ?
                ORDER BY discord_id ASC
                """,
            ),
            (
                "dig_artifacts",
                """
                SELECT
                    guild_id,
                    id,
                    discord_id,
                    artifact_id,
                    found_at,
                    is_relic,
                    equipped
                FROM dig_artifacts
                WHERE guild_id = ?
                ORDER BY found_at ASC, id ASC
                """,
            ),
            (
                "dig_achievements",
                """
                SELECT guild_id, discord_id, achievement_id, unlocked_at
                FROM dig_achievements
                WHERE guild_id = ?
                ORDER BY unlocked_at ASC, discord_id ASC, achievement_id ASC
                """,
            ),
            (
                "dig_actions",
                """
                SELECT
                    guild_id,
                    id,
                    actor_id,
                    target_id,
                    action_type,
                    depth_before,
                    depth_after,
                    jc_delta,
                    created_at
                FROM dig_actions
                WHERE guild_id = ?
                ORDER BY created_at ASC, id ASC
                """,
            ),
            (
                "mafia_games",
                """
                SELECT
                    guild_id,
                    game_id,
                    game_date,
                    phase,
                    started_at,
                    night_ended_at,
                    day_ended_at,
                    winner,
                    entry_fee,
                    payout_per_winner,
                    mvp_id,
                    roster_size,
                    twist_event,
                    day_number,
                    status
                FROM mafia_games
                WHERE guild_id = ?
                ORDER BY started_at ASC, game_id ASC
                """,
            ),
            (
                "mafia_players",
                """
                SELECT
                    mp.guild_id,
                    mp.game_id,
                    mp.discord_id,
                    mp.role,
                    mp.is_godfather,
                    mp.hero_name,
                    mp.is_alive,
                    mp.eliminated_phase,
                    mp.eliminated_at,
                    mp.acted
                FROM mafia_players mp
                JOIN mafia_games mg
                  ON mg.game_id = mp.game_id
                 AND mg.guild_id = mp.guild_id
                WHERE mp.guild_id = ? AND mg.phase = 'RESOLVED'
                ORDER BY mp.game_id ASC, mp.discord_id ASC
                """,
            ),
            (
                "mafia_actions",
                """
                SELECT
                    ma.guild_id,
                    ma.action_id,
                    ma.game_id,
                    ma.actor_id,
                    ma.target_id,
                    ma.action_type,
                    ma.phase,
                    ma.day_number,
                    ma.created_at
                FROM mafia_actions ma
                JOIN mafia_games mg
                  ON mg.game_id = ma.game_id
                 AND mg.guild_id = ma.guild_id
                WHERE ma.guild_id = ? AND mg.phase = 'RESOLVED'
                ORDER BY ma.created_at ASC, ma.action_id ASC
                """,
            ),
            (
                "recalibrations",
                """
                SELECT
                    guild_id,
                    discord_id,
                    last_recalibration_at,
                    COALESCE(total_recalibrations, 0) AS total_recalibrations,
                    rating_at_recalibration
                FROM recalibration_state
                WHERE guild_id = ?
                ORDER BY discord_id ASC
                """,
            ),
            (
                "trivia_sessions",
                """
                SELECT
                    guild_id,
                    id,
                    discord_id,
                    streak,
                    jc_earned,
                    played_at
                FROM trivia_sessions
                WHERE guild_id = ?
                ORDER BY played_at ASC, id ASC
                """,
            ),
            (
                "disburse_vote_history",
                """
                SELECT
                    guild_id,
                    proposal_id,
                    discord_id,
                    vote_method,
                    voted_at,
                    proposal_outcome,
                    finalized_at
                FROM disburse_vote_history
                WHERE guild_id = ?
                ORDER BY voted_at ASC, proposal_id ASC, discord_id ASC
                """,
            ),
            (
                "protected_heroes",
                """
                SELECT
                    guild_id,
                    purchase_id,
                    pending_match_id,
                    match_id,
                    discord_id,
                    team_side,
                    hero_id,
                    cost,
                    status,
                    purchased_at,
                    resolved_at
                FROM protected_hero_purchases
                WHERE guild_id = ?
                ORDER BY purchased_at ASC, purchase_id ASC
                """,
            ),
        ]

        snapshot: dict[str, list[dict]] = {}
        with self.connection() as conn:
            conn.execute("BEGIN")
            cursor = conn.cursor()
            for name, query in queries:
                cursor.execute(query, (guild_id,))
                snapshot[name] = [dict(row) for row in cursor.fetchall()]
        return snapshot

    @staticmethod
    def _question_snapshot(question: dict[str, Any], question_number: int) -> tuple:
        question_key = question.get("question_key", question.get("key"))
        category = question.get("category")
        question_text = question.get("question_text", question.get("text"))
        correct_index = question.get("correct_index")

        raw_options = question.get("options")
        if raw_options is None and question.get("options_json") is not None:
            encoded = question["options_json"]
            raw_options = json.loads(encoded) if isinstance(encoded, str) else encoded

        if not isinstance(question_key, str) or not question_key:
            raise ValueError("Each trivia question requires a non-empty question_key.")
        if not isinstance(category, str) or not category:
            raise ValueError("Each trivia question requires a non-empty category.")
        if not isinstance(question_text, str) or not question_text:
            raise ValueError("Each trivia question requires non-empty question_text.")
        if not isinstance(raw_options, list) or len(raw_options) != 4:
            raise ValueError("Each trivia question requires exactly four options.")
        if not all(isinstance(option, str) for option in raw_options):
            raise ValueError("Trivia question options must be strings.")
        if len({option.casefold() for option in raw_options}) != 4:
            raise ValueError("Trivia question options must be distinct.")
        if not isinstance(correct_index, int) or not 0 <= correct_index < len(raw_options):
            raise ValueError("correct_index must identify one of the stored options.")

        return (
            question_number,
            question_key,
            category,
            1 if question.get("spicy") else 0,
            question_text,
            json.dumps(raw_options, ensure_ascii=False, separators=(",", ":")),
            correct_index,
            question.get("explanation"),
        )

    def try_start_session(
        self,
        discord_id: int,
        guild_id: int | None,
        questions: list[dict],
        now: int,
        cooldown_seconds: int,
        bypass: bool = False,
    ) -> int | None:
        """Atomically claim a cooldown and persist immutable question data."""
        if not questions:
            raise ValueError("A player-trivia session requires at least one question.")
        if cooldown_seconds < 0:
            raise ValueError("cooldown_seconds must be non-negative.")

        guild_id = self.normalize_guild_id(guild_id)
        question_rows = [
            self._question_snapshot(question, number)
            for number, question in enumerate(questions, start=1)
        ]

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            if not bypass:
                cursor.execute(
                    """
                    SELECT 1
                    FROM player_trivia_sessions
                    WHERE guild_id = ?
                      AND discord_id = ?
                      AND status != 'cancelled'
                      AND started_at > ?
                    LIMIT 1
                    """,
                    (guild_id, discord_id, int(now) - int(cooldown_seconds)),
                )
                if cursor.fetchone() is not None:
                    return None

            cursor.execute(
                """
                INSERT INTO player_trivia_sessions (
                    guild_id, discord_id, started_at, status, question_count
                ) VALUES (?, ?, ?, 'active', ?)
                """,
                (guild_id, discord_id, int(now), len(question_rows)),
            )
            session_id = int(cursor.lastrowid)
            cursor.executemany(
                """
                INSERT INTO player_trivia_questions (
                    session_id, question_number, question_key, category,
                    spicy, question_text, options_json, correct_index, explanation
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                """,
                [(session_id, *row) for row in question_rows],
            )
            return session_id

    def get_last_session_started(self, discord_id: int, guild_id: int | None) -> int | None:
        """Return the latest non-cancelled session start time."""
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            row = conn.execute(
                """
                SELECT started_at
                FROM player_trivia_sessions
                WHERE guild_id = ? AND discord_id = ? AND status != 'cancelled'
                ORDER BY started_at DESC, session_id DESC
                LIMIT 1
                """,
                (guild_id, discord_id),
            ).fetchone()
            return int(row["started_at"]) if row is not None else None

    def get_recent_question_keys(
        self, discord_id: int, guild_id: int | None, since: int
    ) -> set[str]:
        """Return question keys from recent, non-cancelled sessions."""
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            rows = conn.execute(
                """
                SELECT DISTINCT q.question_key
                FROM player_trivia_questions q
                JOIN player_trivia_sessions s ON s.session_id = q.session_id
                WHERE s.guild_id = ?
                  AND s.discord_id = ?
                  AND s.started_at >= ?
                  AND s.status != 'cancelled'
                ORDER BY q.question_key ASC
                """,
                (guild_id, discord_id, int(since)),
            ).fetchall()
            return {str(row["question_key"]) for row in rows}

    def settle_answer(
        self,
        session_id: int,
        question_number: int,
        selected_index: int,
        reward_if_correct: int,
        answered_at: int,
    ) -> dict | None:
        """Settle the first answer to a stored question and credit its reward.

        Correctness is always computed from the persisted ``correct_index``.
        A repeated click, unknown round, or terminal session returns ``None``.
        """
        if reward_if_correct < 0:
            raise ValueError("reward_if_correct must be non-negative.")

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    s.guild_id,
                    s.discord_id,
                    s.status,
                    s.question_count,
                    s.score,
                    s.jc_earned,
                    q.question_key,
                    q.category,
                    q.options_json,
                    q.correct_index,
                    q.selected_index
                FROM player_trivia_sessions s
                JOIN player_trivia_questions q ON q.session_id = s.session_id
                WHERE s.session_id = ? AND q.question_number = ?
                """,
                (session_id, question_number),
            )
            row = cursor.fetchone()
            if row is None or row["status"] != "active" or row["selected_index"] is not None:
                return None

            cursor.execute(
                """
                SELECT MIN(question_number) AS next_question
                FROM player_trivia_questions
                WHERE session_id = ? AND selected_index IS NULL
                """,
                (session_id,),
            )
            next_question = cursor.fetchone()["next_question"]
            if next_question is None or int(next_question) != question_number:
                return None

            options = json.loads(row["options_json"])
            if not isinstance(selected_index, int) or not 0 <= selected_index < len(options):
                raise ValueError("selected_index must identify one of the stored options.")

            is_correct = selected_index == int(row["correct_index"])
            reward = int(reward_if_correct) if is_correct else 0

            cursor.execute(
                """
                UPDATE player_trivia_questions
                SET selected_index = ?, is_correct = ?, reward = ?, answered_at = ?
                WHERE session_id = ?
                  AND question_number = ?
                  AND selected_index IS NULL
                """,
                (
                    selected_index,
                    1 if is_correct else 0,
                    reward,
                    int(answered_at),
                    session_id,
                    question_number,
                ),
            )
            if cursor.rowcount != 1:
                return None

            cursor.execute(
                """
                SELECT COALESCE(jopacoin_balance, 0) AS balance
                FROM players
                WHERE guild_id = ? AND discord_id = ?
                """,
                (row["guild_id"], row["discord_id"]),
            )
            player_row = cursor.fetchone()
            if player_row is None:
                raise ValueError("Player for trivia session no longer exists.")
            new_balance = int(player_row["balance"])

            if reward:
                self._set_economy_ledger_context(
                    cursor,
                    source="player_trivia",
                    actor_id=int(row["discord_id"]),
                    related_type="player_trivia_session",
                    related_id=session_id,
                    reason="player trivia correct answer",
                    metadata={
                        "session_id": session_id,
                        "question_number": question_number,
                        "question_key": row["question_key"],
                        "category": row["category"],
                        "selected_index": selected_index,
                        "correct_index": int(row["correct_index"]),
                        "reward": reward,
                        "answered_at": int(answered_at),
                    },
                )
                try:
                    cursor.execute(
                        """
                        UPDATE players
                        SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?,
                            updated_at = CURRENT_TIMESTAMP
                        WHERE guild_id = ? AND discord_id = ?
                        """,
                        (reward, row["guild_id"], row["discord_id"]),
                    )
                finally:
                    self._clear_economy_ledger_context(cursor)
                new_balance += reward

            score = int(row["score"] or 0) + (1 if is_correct else 0)
            jc_earned = int(row["jc_earned"] or 0) + reward
            cursor.execute(
                """
                SELECT COUNT(*) AS answered
                FROM player_trivia_questions
                WHERE session_id = ? AND selected_index IS NOT NULL
                """,
                (session_id,),
            )
            answered = int(cursor.fetchone()["answered"])
            completed = answered >= int(row["question_count"])
            cursor.execute(
                """
                UPDATE player_trivia_sessions
                SET score = ?,
                    jc_earned = ?,
                    status = CASE WHEN ? THEN 'completed' ELSE status END,
                    completed_at = CASE WHEN ? THEN ? ELSE completed_at END
                WHERE session_id = ?
                """,
                (
                    score,
                    jc_earned,
                    1 if completed else 0,
                    1 if completed else 0,
                    int(answered_at),
                    session_id,
                ),
            )

            return {
                "accepted": True,
                "is_correct": is_correct,
                "reward": reward,
                "score": score,
                "jc_earned": jc_earned,
                "complete": completed,
                "completed": completed,
                "new_balance": new_balance,
            }

    def finish_session(self, session_id: int, status: str, now: int) -> None:
        """Finish an active session with an explicit terminal status."""
        if status not in self._FINISH_STATUSES:
            raise ValueError(f"Unsupported player-trivia finish status: {status}")
        with self.connection() as conn:
            conn.execute(
                """
                UPDATE player_trivia_sessions
                SET status = ?, completed_at = ?
                WHERE session_id = ? AND status = 'active'
                """,
                (status, int(now), session_id),
            )

    def cancel_session_if_unanswered(self, session_id: int, now: int | None = None) -> bool:
        """Cancel an untouched active session so it no longer consumes cooldown."""
        if now is None:
            now = int(time.time())
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE player_trivia_sessions
                SET status = 'cancelled', completed_at = ?
                WHERE session_id = ?
                  AND status = 'active'
                  AND NOT EXISTS (
                      SELECT 1
                      FROM player_trivia_questions
                      WHERE session_id = ? AND selected_index IS NOT NULL
                  )
                """,
                (int(now), session_id, session_id),
            )
            return cursor.rowcount == 1

    def get_session(self, session_id: int) -> dict | None:
        """Return one stored player-trivia session."""
        with self.connection() as conn:
            row = conn.execute(
                """
                SELECT session_id, guild_id, discord_id, started_at, completed_at,
                       status, question_count, score, jc_earned
                FROM player_trivia_sessions
                WHERE session_id = ?
                """,
                (session_id,),
            ).fetchone()
            return dict(row) if row is not None else None

    def get_questions(self, session_id: int) -> list[dict]:
        """Return stored rounds in question order with decoded options."""
        with self.connection() as conn:
            rows = conn.execute(
                """
                SELECT session_id, question_number, question_key, category, spicy,
                       question_text, options_json, correct_index, explanation,
                       selected_index, is_correct, reward, answered_at
                FROM player_trivia_questions
                WHERE session_id = ?
                ORDER BY question_number ASC
                """,
                (session_id,),
            ).fetchall()

        questions = []
        for row in rows:
            question = dict(row)
            question["options"] = json.loads(question["options_json"])
            question["spicy"] = bool(question["spicy"])
            if question["is_correct"] is not None:
                question["is_correct"] = bool(question["is_correct"])
            questions.append(question)
        return questions
