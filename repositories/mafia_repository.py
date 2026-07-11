"""Repository for Daily Mafia subgame data access."""

import time
from typing import Any

from domain.models.mafia import (
    MafiaActionType,
    MafiaGame,
    MafiaPhase,
    MafiaPlayer,
    MafiaRole,
    MafiaTwist,
    MafiaWinner,
)
from repositories.base_repository import BaseRepository


def _row_get(row, key, default=None):
    """Safe column access for rows that may predate a migration."""
    try:
        keys = row.keys()
    except AttributeError:
        return row[key]
    return row[key] if key in keys else default


def _row_to_game(row) -> MafiaGame:
    return MafiaGame(
        game_id=row["game_id"],
        guild_id=row["guild_id"],
        game_date=row["game_date"],
        phase=MafiaPhase(row["phase"]),
        started_at=row["started_at"],
        roster_size=row["roster_size"],
        twist_event=MafiaTwist(row["twist_event"]) if row["twist_event"] else None,
        night_ended_at=row["night_ended_at"],
        day_ended_at=row["day_ended_at"],
        winner=MafiaWinner(row["winner"]) if row["winner"] else None,
        entry_fee=row["entry_fee"] or 0,
        payout_per_winner=row["payout_per_winner"] or 0,
        mvp_id=row["mvp_id"],
        mafia_thread_id=row["mafia_thread_id"],
        discussion_thread_id=row["discussion_thread_id"],
        setup_message_id=row["setup_message_id"],
        day_number=_row_get(row, "day_number", 1) or 1,
        phase_started_at=_row_get(row, "phase_started_at"),
        standings_message_id=_row_get(row, "standings_message_id"),
        graveyard_thread_id=_row_get(row, "graveyard_thread_id"),
        status=_row_get(row, "status", "ACTIVE") or "ACTIVE",
    )


def _row_to_player(row) -> MafiaPlayer:
    return MafiaPlayer(
        game_id=row["game_id"],
        discord_id=row["discord_id"],
        guild_id=row["guild_id"],
        role=MafiaRole(row["role"]),
        is_godfather=bool(row["is_godfather"]),
        hero_name=row["hero_name"],
        is_alive=bool(row["is_alive"]),
        eliminated_phase=MafiaPhase(row["eliminated_phase"]) if row["eliminated_phase"] else None,
        eliminated_at=row["eliminated_at"],
        acted=bool(row["acted"]),
    )


class MafiaRepository(BaseRepository):
    """Data access for Daily Mafia games, players, actions, and opt-outs."""

    # ── Game lifecycle ──────────────────────────────────────────────────────

    def get_active_game(self, guild_id: int | None) -> MafiaGame | None:
        """Most recent non-RESOLVED game for the guild."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT * FROM mafia_games
                WHERE guild_id = ? AND phase != ?
                ORDER BY game_id DESC LIMIT 1
                """,
                (gid, MafiaPhase.RESOLVED.value),
            )
            row = cursor.fetchone()
            return _row_to_game(row) if row else None

    def get_game_for_date(self, guild_id: int | None, game_date: str) -> MafiaGame | None:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT * FROM mafia_games WHERE guild_id = ? AND game_date = ?",
                (gid, game_date),
            )
            row = cursor.fetchone()
            return _row_to_game(row) if row else None

    def get_game_by_id(self, game_id: int) -> MafiaGame | None:
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT * FROM mafia_games WHERE game_id = ?", (game_id,))
            row = cursor.fetchone()
            return _row_to_game(row) if row else None

    def create_game(
        self,
        guild_id: int | None,
        game_date: str,
        phase: MafiaPhase,
        started_at: int,
        roster_size: int,
        twist_event: MafiaTwist | None,
        entry_fee: int = 0,
        phase_started_at: int | None = None,
    ) -> int:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO mafia_games (
                    guild_id, game_date, phase, started_at, roster_size,
                    twist_event, entry_fee, phase_started_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    gid,
                    game_date,
                    phase.value,
                    started_at,
                    roster_size,
                    twist_event.value if twist_event else None,
                    entry_fee,
                    phase_started_at if phase_started_at is not None else started_at,
                ),
            )
            return cursor.lastrowid

    def create_game_with_players_and_entry_fees(
        self,
        *,
        guild_id: int | None,
        game_date: str,
        phase: MafiaPhase,
        started_at: int,
        roster_size: int,
        twist_event: MafiaTwist | None,
        entry_fee: int,
        players: list[MafiaPlayer],
        max_debt: int,
    ) -> int:
        """Create a game, roster players, and collect entry fees atomically."""
        if not players:
            raise ValueError("no_players")

        gid = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO mafia_games (
                    guild_id, game_date, phase, started_at, roster_size,
                    twist_event, entry_fee, phase_started_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    gid,
                    game_date,
                    phase.value,
                    started_at,
                    roster_size,
                    twist_event.value if twist_event else None,
                    entry_fee,
                    started_at,
                ),
            )
            game_id = cursor.lastrowid

            player_rows = [
                (
                    game_id,
                    p.discord_id,
                    gid,
                    p.role.value,
                    1 if p.is_godfather else 0,
                    p.hero_name,
                    1 if p.is_alive else 0,
                    p.eliminated_phase.value if p.eliminated_phase else None,
                    p.eliminated_at,
                    1 if p.acted else 0,
                )
                for p in players
            ]
            cursor.executemany(
                """
                INSERT INTO mafia_players (
                    game_id, discord_id, guild_id, role, is_godfather, hero_name,
                    is_alive, eliminated_phase, eliminated_at, acted
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                """,
                player_rows,
            )

            self._set_economy_ledger_context(
                cursor,
                source="mafia_entry_fee",
                related_type="mafia_game",
                related_id=game_id,
                reason="mafia entry fee",
                metadata={
                    "game_date": game_date,
                    "entry_fee": entry_fee,
                    "roster_size": roster_size,
                },
            )
            try:
                for player in players:
                    cursor.execute(
                        """
                        UPDATE players
                        SET jopacoin_balance = COALESCE(jopacoin_balance, 0) - ?,
                            updated_at = CURRENT_TIMESTAMP
                        WHERE discord_id = ? AND guild_id = ?
                          AND COALESCE(jopacoin_balance, 0) - ? >= ?
                        """,
                        (entry_fee, player.discord_id, gid, entry_fee, -max_debt),
                    )
                    if cursor.rowcount != 1:
                        raise ValueError("entry_fee_debit_failed")
                player_ids = [p.discord_id for p in players]
                placeholders = ",".join("?" * len(player_ids))
                cursor.execute(
                    f"""
                    UPDATE players
                    SET lowest_balance_ever = jopacoin_balance
                    WHERE discord_id IN ({placeholders}) AND guild_id = ?
                      AND (lowest_balance_ever IS NULL OR jopacoin_balance < lowest_balance_ever)
                    """,
                    player_ids + [gid],
                )
            finally:
                self._clear_economy_ledger_context(cursor)

            return game_id

    def set_phase(
        self,
        game_id: int,
        phase: MafiaPhase,
        *,
        night_ended_at: int | None = None,
        day_ended_at: int | None = None,
    ) -> None:
        sets = ["phase = ?"]
        params: list = [phase.value]
        if night_ended_at is not None:
            sets.append("night_ended_at = ?")
            params.append(night_ended_at)
        if day_ended_at is not None:
            sets.append("day_ended_at = ?")
            params.append(day_ended_at)
        params.append(game_id)
        with self.connection() as conn:
            conn.cursor().execute(
                f"UPDATE mafia_games SET {', '.join(sets)} WHERE game_id = ?",
                params,
            )

    def set_twist_event(self, game_id: int, twist: MafiaTwist | None) -> None:
        with self.connection() as conn:
            conn.cursor().execute(
                "UPDATE mafia_games SET twist_event = ? WHERE game_id = ?",
                (twist.value if twist else None, game_id),
            )

    def set_thread_ids(
        self,
        game_id: int,
        *,
        mafia_thread_id: int | None = None,
        discussion_thread_id: int | None = None,
        setup_message_id: int | None = None,
        graveyard_thread_id: int | None = None,
        standings_message_id: int | None = None,
    ) -> None:
        sets: list[str] = []
        params: list = []
        if mafia_thread_id is not None:
            sets.append("mafia_thread_id = ?")
            params.append(mafia_thread_id)
        if discussion_thread_id is not None:
            sets.append("discussion_thread_id = ?")
            params.append(discussion_thread_id)
        if setup_message_id is not None:
            sets.append("setup_message_id = ?")
            params.append(setup_message_id)
        if graveyard_thread_id is not None:
            sets.append("graveyard_thread_id = ?")
            params.append(graveyard_thread_id)
        if standings_message_id is not None:
            sets.append("standings_message_id = ?")
            params.append(standings_message_id)
        if not sets:
            return
        params.append(game_id)
        with self.connection() as conn:
            conn.cursor().execute(
                f"UPDATE mafia_games SET {', '.join(sets)} WHERE game_id = ?",
                params,
            )

    def finalize_game(
        self,
        game_id: int,
        winner: MafiaWinner,
        payout_per_winner: int,
        mvp_id: int | None,
    ) -> None:
        with self.connection() as conn:
            conn.cursor().execute(
                """
                UPDATE mafia_games
                SET phase = ?, winner = ?, payout_per_winner = ?, mvp_id = ?, day_ended_at = ?
                WHERE game_id = ?
                """,
                (
                    MafiaPhase.RESOLVED.value,
                    winner.value,
                    payout_per_winner,
                    mvp_id,
                    int(time.time()),
                    game_id,
                ),
            )

    def apply_night_resolution(
        self, game_id: int, killed_ids: list[int], *, ended_at: int | None = None
    ) -> bool:
        """Atomically apply night deaths and advance NIGHT -> DAY once.

        Resets ``phase_started_at`` to ``ended_at`` so the day clock starts from
        the moment the night resolved.
        """
        ended = ended_at if ended_at is not None else int(time.time())
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE mafia_games
                SET phase = ?, night_ended_at = ?, phase_started_at = ?
                WHERE game_id = ? AND phase = ?
                """,
                (MafiaPhase.DAY.value, ended, ended, game_id, MafiaPhase.NIGHT.value),
            )
            if cursor.rowcount != 1:
                return False

            for discord_id in killed_ids:
                cursor.execute(
                    """
                    UPDATE mafia_players
                    SET is_alive = 0, eliminated_phase = ?, eliminated_at = ?
                    WHERE game_id = ? AND discord_id = ? AND is_alive = 1
                    """,
                    (MafiaPhase.NIGHT.value, ended, game_id, discord_id),
                )
            return True

    def advance_to_next_cycle(
        self, game_id: int, *, ended_at: int | None = None
    ) -> bool:
        """Atomically advance DAY -> next NIGHT (undecided game continues).

        Increments day_number, resets phase_started_at, records day_ended_at.
        Returns False if the game wasn't in DAY (already advanced/resolved).
        """
        ended = ended_at if ended_at is not None else int(time.time())
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE mafia_games
                SET phase = ?, day_ended_at = ?, phase_started_at = ?,
                    day_number = day_number + 1
                WHERE game_id = ? AND phase = ?
                """,
                (MafiaPhase.NIGHT.value, ended, ended, game_id, MafiaPhase.DAY.value),
            )
            return cursor.rowcount == 1

    def revive_player(self, game_id: int, discord_id: int) -> None:
        """Bring a dead player back (Resurrection event)."""
        with self.connection() as conn:
            conn.cursor().execute(
                """
                UPDATE mafia_players
                SET is_alive = 1, eliminated_phase = NULL, eliminated_at = NULL
                WHERE game_id = ? AND discord_id = ?
                """,
                (game_id, discord_id),
            )

    def cancel_game(
        self, game_id: int, *, refund: bool = True
    ) -> dict[str, Any]:
        """Abort an in-flight game: optionally refund entry fees, mark CANCELLED.

        Returns {"applied": bool, "refunded": {discord_id: amount}}.
        """
        refunded: dict[int, int] = {}
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT guild_id, entry_fee, phase FROM mafia_games WHERE game_id = ?",
                (game_id,),
            )
            row = cursor.fetchone()
            if row is None:
                return {"applied": False, "reason": "game_not_found", "refunded": {}}
            if row["phase"] == MafiaPhase.RESOLVED.value:
                return {"applied": False, "reason": "already_resolved", "refunded": {}}
            gid = row["guild_id"]
            entry_fee = row["entry_fee"] or 0

            if refund and entry_fee > 0:
                cursor.execute(
                    "SELECT discord_id FROM mafia_players WHERE game_id = ?",
                    (game_id,),
                )
                player_ids = [r["discord_id"] for r in cursor.fetchall()]
                self._set_economy_ledger_context(
                    cursor,
                    source="mafia_abort_refund",
                    related_type="mafia_game",
                    related_id=game_id,
                    reason="mafia game aborted — entry fee refunded",
                    metadata={"entry_fee": entry_fee},
                )
                try:
                    for discord_id in player_ids:
                        cursor.execute(
                            """
                            UPDATE players
                            SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?,
                                updated_at = CURRENT_TIMESTAMP
                            WHERE discord_id = ? AND guild_id = ?
                            """,
                            (entry_fee, discord_id, gid),
                        )
                        if cursor.rowcount == 1:
                            refunded[discord_id] = entry_fee
                finally:
                    self._clear_economy_ledger_context(cursor)

            cursor.execute(
                """
                UPDATE mafia_games
                SET phase = ?, status = 'CANCELLED', winner = ?, day_ended_at = ?
                WHERE game_id = ?
                """,
                (MafiaPhase.RESOLVED.value, MafiaWinner.NONE.value, int(time.time()), game_id),
            )
            return {"applied": True, "refunded": refunded}

    def finalize_day_resolution(
        self,
        *,
        game_id: int,
        winner: MafiaWinner,
        payout_per_winner: int,
        mvp_id: int | None,
        lynched_id: int | None,
        payout_deltas: dict[int, int],
        entry_fee: int,
        bankruptcy_penalty_rate: float | None = None,
        nonprofit_overflow: int = 0,
        ended_at: int | None = None,
    ) -> dict[str, Any]:
        """Atomically apply lynch, payouts, bankruptcy sinks, and DAY -> RESOLVED."""
        ended = ended_at if ended_at is not None else int(time.time())
        penalties: dict[int, int] = {}
        gid: int | None = None

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT guild_id FROM mafia_games WHERE game_id = ?", (game_id,))
            row = cursor.fetchone()
            if row is None:
                return {"applied": False, "reason": "game_not_found"}
            gid = row["guild_id"]

            cursor.execute(
                """
                UPDATE mafia_games
                SET phase = ?, winner = ?, payout_per_winner = ?, mvp_id = ?, day_ended_at = ?
                WHERE game_id = ? AND phase = ?
                """,
                (
                    MafiaPhase.RESOLVED.value,
                    winner.value,
                    payout_per_winner,
                    mvp_id,
                    ended,
                    game_id,
                    MafiaPhase.DAY.value,
                ),
            )
            if cursor.rowcount != 1:
                return {"applied": False, "reason": "already_resolved"}

            if lynched_id is not None:
                cursor.execute(
                    """
                    UPDATE mafia_players
                    SET is_alive = 0, eliminated_phase = ?, eliminated_at = ?
                    WHERE game_id = ? AND discord_id = ? AND is_alive = 1
                    """,
                    (MafiaPhase.DAY.value, ended, game_id, lynched_id),
                )

            if payout_deltas:
                self._set_economy_ledger_context(
                    cursor,
                    source="mafia_payout",
                    related_type="mafia_game",
                    related_id=game_id,
                    reason="mafia pot payout",
                    metadata={
                        "winner": winner.value,
                        "entry_fee": entry_fee,
                        "payout_per_winner": payout_per_winner,
                    },
                )
                try:
                    for discord_id, amount in payout_deltas.items():
                        if amount <= 0:
                            continue
                        cursor.execute(
                            """
                            UPDATE players
                            SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?,
                                updated_at = CURRENT_TIMESTAMP
                            WHERE discord_id = ? AND guild_id = ?
                            """,
                            (amount, discord_id, gid),
                        )
                        if cursor.rowcount != 1:
                            raise ValueError("payout_player_missing")
                finally:
                    self._clear_economy_ledger_context(cursor)

            # Capped-payout overflow flows into the nonprofit fund, in the same
            # transaction as the payouts so the pot is conserved atomically.
            if nonprofit_overflow > 0:
                cursor.execute(
                    """
                    INSERT INTO nonprofit_fund (guild_id, total_collected, updated_at)
                    VALUES (?, ?, CURRENT_TIMESTAMP)
                    ON CONFLICT(guild_id) DO UPDATE SET
                        total_collected = total_collected + excluded.total_collected,
                        updated_at = CURRENT_TIMESTAMP
                    """,
                    (gid, nonprofit_overflow),
                )

            if bankruptcy_penalty_rate is not None:
                kept_rate = max(0.0, min(1.0, bankruptcy_penalty_rate))
                self._set_economy_ledger_context(
                    cursor,
                    source="mafia_bankruptcy_penalty",
                    related_type="mafia_game",
                    related_id=game_id,
                    reason="mafia bankruptcy penalty",
                    metadata={"winner": winner.value, "entry_fee": entry_fee},
                )
                try:
                    for discord_id, amount in payout_deltas.items():
                        profit = max(0, int(amount) - entry_fee)
                        if profit <= 0:
                            continue
                        cursor.execute(
                            """
                            SELECT COALESCE(penalty_games_remaining, 0) AS penalty_games
                            FROM bankruptcy_state
                            WHERE discord_id = ? AND guild_id = ?
                            """,
                            (discord_id, gid),
                        )
                        state = cursor.fetchone()
                        if state is None or int(state["penalty_games"]) <= 0:
                            continue
                        penalty = int(profit * (1 - kept_rate))
                        if penalty <= 0:
                            continue
                        cursor.execute(
                            """
                            UPDATE players
                            SET jopacoin_balance = COALESCE(jopacoin_balance, 0) - ?,
                                updated_at = CURRENT_TIMESTAMP
                            WHERE discord_id = ? AND guild_id = ?
                            """,
                            (penalty, discord_id, gid),
                        )
                        if cursor.rowcount != 1:
                            raise ValueError("penalty_player_missing")
                        penalties[discord_id] = penalty
                finally:
                    self._clear_economy_ledger_context(cursor)

            return {"applied": True, "bankruptcy_penalties": penalties, "guild_id": gid}

    # ── Players ─────────────────────────────────────────────────────────────

    def add_players(self, game_id: int, players: list[MafiaPlayer]) -> None:
        if not players:
            return
        rows = [
            (
                game_id,
                p.discord_id,
                p.guild_id,
                p.role.value,
                1 if p.is_godfather else 0,
                p.hero_name,
                1 if p.is_alive else 0,
                p.eliminated_phase.value if p.eliminated_phase else None,
                p.eliminated_at,
                1 if p.acted else 0,
            )
            for p in players
        ]
        with self.connection() as conn:
            conn.cursor().executemany(
                """
                INSERT INTO mafia_players (
                    game_id, discord_id, guild_id, role, is_godfather, hero_name,
                    is_alive, eliminated_phase, eliminated_at, acted
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                """,
                rows,
            )

    def get_players(self, game_id: int) -> list[MafiaPlayer]:
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT * FROM mafia_players WHERE game_id = ? ORDER BY discord_id",
                (game_id,),
            )
            return [_row_to_player(row) for row in cursor.fetchall()]

    def get_player(self, game_id: int, discord_id: int) -> MafiaPlayer | None:
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT * FROM mafia_players WHERE game_id = ? AND discord_id = ?",
                (game_id, discord_id),
            )
            row = cursor.fetchone()
            return _row_to_player(row) if row else None

    def get_alive_players(
        self, game_id: int, role: MafiaRole | None = None
    ) -> list[MafiaPlayer]:
        with self.connection() as conn:
            cursor = conn.cursor()
            if role is None:
                cursor.execute(
                    "SELECT * FROM mafia_players WHERE game_id = ? AND is_alive = 1",
                    (game_id,),
                )
            else:
                cursor.execute(
                    "SELECT * FROM mafia_players WHERE game_id = ? AND is_alive = 1 AND role = ?",
                    (game_id, role.value),
                )
            return [_row_to_player(row) for row in cursor.fetchall()]

    def set_player_alive(
        self,
        game_id: int,
        discord_id: int,
        alive: bool,
        *,
        eliminated_phase: MafiaPhase | None = None,
    ) -> None:
        with self.connection() as conn:
            conn.cursor().execute(
                """
                UPDATE mafia_players
                SET is_alive = ?, eliminated_phase = ?, eliminated_at = ?
                WHERE game_id = ? AND discord_id = ?
                """,
                (
                    1 if alive else 0,
                    eliminated_phase.value if eliminated_phase else None,
                    None if alive else int(time.time()),
                    game_id,
                    discord_id,
                ),
            )

    # ── Actions ─────────────────────────────────────────────────────────────

    def record_action(
        self,
        game_id: int,
        guild_id: int | None,
        actor_id: int,
        target_id: int | None,
        action_type: MafiaActionType,
        phase: MafiaPhase,
        result: str | None = None,
        day_number: int = 1,
    ) -> None:
        """UPSERT an action (scoped to the cycle) and mark the actor as acted."""
        gid = self.normalize_guild_id(guild_id)
        now = int(time.time())
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO mafia_actions (
                    game_id, guild_id, actor_id, target_id, action_type, phase,
                    day_number, created_at, result
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(game_id, actor_id, action_type, phase, day_number) DO UPDATE SET
                    target_id = excluded.target_id,
                    created_at = excluded.created_at,
                    result = excluded.result
                """,
                (
                    game_id,
                    gid,
                    actor_id,
                    target_id,
                    action_type.value,
                    phase.value,
                    day_number,
                    now,
                    result,
                ),
            )
            cursor.execute(
                "UPDATE mafia_players SET acted = 1 WHERE game_id = ? AND discord_id = ?",
                (game_id, actor_id),
            )

    def get_actions(
        self,
        game_id: int,
        action_type: MafiaActionType | None = None,
        phase: MafiaPhase | None = None,
        day_number: int | None = None,
    ) -> list[dict]:
        clauses = ["game_id = ?"]
        params: list = [game_id]
        if action_type is not None:
            clauses.append("action_type = ?")
            params.append(action_type.value)
        if phase is not None:
            clauses.append("phase = ?")
            params.append(phase.value)
        if day_number is not None:
            clauses.append("day_number = ?")
            params.append(day_number)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"SELECT * FROM mafia_actions WHERE {' AND '.join(clauses)} ORDER BY created_at",
                params,
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_action_for_actor(
        self,
        game_id: int,
        actor_id: int,
        action_type: MafiaActionType,
        day_number: int | None = None,
    ) -> dict | None:
        """Find the most recent action of a given type by an actor.

        Scoped to ``day_number`` when provided (one-read-per-night detective);
        otherwise spans all cycles (vigilante one-shot per game).
        """
        clauses = ["game_id = ?", "actor_id = ?", "action_type = ?"]
        params: list = [game_id, actor_id, action_type.value]
        if day_number is not None:
            clauses.append("day_number = ?")
            params.append(day_number)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"""
                SELECT * FROM mafia_actions
                WHERE {' AND '.join(clauses)}
                ORDER BY created_at DESC LIMIT 1
                """,
                params,
            )
            row = cursor.fetchone()
            return dict(row) if row else None

    # ── Town Bounty ─────────────────────────────────────────────────────────

    def add_bounty(
        self,
        *,
        game_id: int,
        guild_id: int | None,
        day_number: int,
        target_id: int,
        contributor_id: int,
        max_debt: int,
    ) -> dict[str, Any]:
        """Stake 1 JC on a suspect for the current day.

        The PRIMARY KEY enforces ≤1 stake per contributor/target/day. The stake
        is parked in the nonprofit fund: on a successful bounty it is paid back
        out (capped at N); on failure it stays forfeited. Atomic.
        """
        gid = self.normalize_guild_id(guild_id)
        now = int(time.time())
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT 1 FROM mafia_bounties
                WHERE game_id = ? AND day_number = ? AND target_id = ? AND contributor_id = ?
                """,
                (game_id, day_number, target_id, contributor_id),
            )
            if cursor.fetchone() is not None:
                return {"ok": False, "error": "already_staked"}

            # Debit 1 JC from the contributor (respecting the debt floor).
            self._set_economy_ledger_context(
                cursor,
                source="mafia_bounty_stake",
                related_type="mafia_game",
                related_id=game_id,
                reason="mafia town bounty stake",
                metadata={"day_number": day_number, "target_id": target_id},
            )
            try:
                cursor.execute(
                    """
                    UPDATE players
                    SET jopacoin_balance = COALESCE(jopacoin_balance, 0) - 1,
                        updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                      AND COALESCE(jopacoin_balance, 0) - 1 >= ?
                    """,
                    (contributor_id, gid, -max_debt),
                )
                if cursor.rowcount != 1:
                    return {"ok": False, "error": "insufficient_funds"}
            finally:
                self._clear_economy_ledger_context(cursor)

            # Park the stake in the nonprofit fund.
            cursor.execute(
                """
                INSERT INTO nonprofit_fund (guild_id, total_collected, updated_at)
                VALUES (?, 1, CURRENT_TIMESTAMP)
                ON CONFLICT(guild_id) DO UPDATE SET
                    total_collected = total_collected + 1,
                    updated_at = CURRENT_TIMESTAMP
                """,
                (gid,),
            )
            cursor.execute(
                """
                INSERT INTO mafia_bounties
                    (game_id, guild_id, day_number, target_id, contributor_id, amount, created_at)
                VALUES (?, ?, ?, ?, ?, 1, ?)
                """,
                (game_id, gid, day_number, target_id, contributor_id, now),
            )
            return {"ok": True}

    def get_bounties_for_day(self, game_id: int, day_number: int) -> list[dict]:
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT * FROM mafia_bounties WHERE game_id = ? AND day_number = ?",
                (game_id, day_number),
            )
            return [dict(row) for row in cursor.fetchall()]

    def resolve_day_bounties(
        self,
        *,
        game_id: int,
        guild_id: int | None,
        day_number: int,
        lynched_id: int | None,
        lynched_was_mafia: bool,
        alive_count: int,
    ) -> dict[str, Any]:
        """Pay out a successful bounty, drawn from the nonprofit fund where the
        stakes were parked. The reward is capped at the coins actually staked on
        the winning target (and additionally at N = alive_count and the fund
        balance), so it can never pull unrelated nonprofit-fund revenue. Failed/
        other stakes stay forfeited. Atomic.
        Returns {"paid": {contributor_id: amount}, "reward": int}.
        """
        gid = self.normalize_guild_id(guild_id)
        paid: dict[int, int] = {}
        reward_total = 0
        if lynched_id is None or not lynched_was_mafia:
            return {"paid": paid, "reward": 0}

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT contributor_id, amount FROM mafia_bounties
                WHERE game_id = ? AND day_number = ? AND target_id = ?
                """,
                (game_id, day_number, lynched_id),
            )
            bounty_rows = cursor.fetchall()
            winners = [r["contributor_id"] for r in bounty_rows]
            if not winners:
                return {"paid": paid, "reward": 0}

            # The reward can only be funded by the coins players actually staked
            # on THIS target — never by unrelated nonprofit-fund revenue (tax
            # fines, capped-payout overflow, loans, disbursements). Cap by the
            # parked stake sum, then by alive_count, then by what the fund holds.
            parked_stake_sum = sum(r["amount"] for r in bounty_rows)
            cursor.execute(
                "SELECT total_collected FROM nonprofit_fund WHERE guild_id = ?",
                (gid,),
            )
            row = cursor.fetchone()
            available = row["total_collected"] if row else 0
            reward_total = max(0, min(alive_count, parked_stake_sum, available))
            if reward_total <= 0:
                return {"paid": paid, "reward": 0}

            # Draw the reward out of the nonprofit fund and split it.
            cursor.execute(
                """
                UPDATE nonprofit_fund
                SET total_collected = total_collected - ?, updated_at = CURRENT_TIMESTAMP
                WHERE guild_id = ?
                """,
                (reward_total, gid),
            )
            base = reward_total // len(winners)
            dust = reward_total - base * len(winners)
            self._set_economy_ledger_context(
                cursor,
                source="mafia_bounty_payout",
                related_type="mafia_game",
                related_id=game_id,
                reason="mafia town bounty payout",
                metadata={"day_number": day_number, "target_id": lynched_id},
            )
            try:
                for i, cid in enumerate(winners):
                    amount = base + (dust if i == 0 else 0)
                    if amount <= 0:
                        continue
                    cursor.execute(
                        """
                        UPDATE players
                        SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?,
                            updated_at = CURRENT_TIMESTAMP
                        WHERE discord_id = ? AND guild_id = ?
                        """,
                        (amount, cid, gid),
                    )
                    if cursor.rowcount == 1:
                        paid[cid] = amount
            finally:
                self._clear_economy_ledger_context(cursor)
            return {"paid": paid, "reward": reward_total}

    # ── Eligibility & opt-out ───────────────────────────────────────────────

    def get_eligible_player_ids(
        self,
        guild_id: int | None,
        since: int,
        *,
        entry_fee: int = 0,
        max_debt: int | None = None,
    ) -> list[int]:
        """Distinct discord_ids active in /gamba or /dig within the last `since` window.

        `since` is a unix timestamp lower-bound. Excludes opted-out players,
        unregistered players, and players who cannot pay the entry fee without
        exceeding the configured debt floor.
        """
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            debt_clause = ""
            params: list = [gid, since, gid, since, gid, gid]
            if max_debt is not None:
                debt_clause = "AND COALESCE(p.jopacoin_balance, 0) - ? >= ?"
                params.extend([entry_fee, -max_debt])
            cursor.execute(
                f"""
                SELECT DISTINCT recent.discord_id FROM (
                    SELECT discord_id FROM wheel_spins
                      WHERE guild_id = ? AND spin_time >= ?
                    UNION
                    SELECT actor_id AS discord_id FROM dig_actions
                      WHERE guild_id = ? AND action_type = 'dig' AND created_at >= ?
                ) AS recent
                JOIN players p
                  ON p.discord_id = recent.discord_id
                 AND p.guild_id = ?
                WHERE recent.discord_id NOT IN (
                    SELECT discord_id FROM mafia_optout WHERE guild_id = ?
                )
                {debt_clause}
                ORDER BY recent.discord_id
                """,
                params,
            )
            return [row["discord_id"] for row in cursor.fetchall()]

    def get_recent_player_participation(
        self, discord_id: int, guild_id: int | None, limit: int = 3
    ) -> list[bool]:
        """Most recent `limit` mafia_players.acted values, newest first."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT acted FROM mafia_players
                WHERE discord_id = ? AND guild_id = ?
                ORDER BY game_id DESC LIMIT ?
                """,
                (discord_id, gid, limit),
            )
            return [bool(row["acted"]) for row in cursor.fetchall()]

    def set_optout(self, guild_id: int | None, discord_id: int, opted_out: bool) -> None:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            if opted_out:
                cursor.execute(
                    "INSERT OR IGNORE INTO mafia_optout (discord_id, guild_id) VALUES (?, ?)",
                    (discord_id, gid),
                )
            else:
                cursor.execute(
                    "DELETE FROM mafia_optout WHERE discord_id = ? AND guild_id = ?",
                    (discord_id, gid),
                )

    def is_opted_out(self, guild_id: int | None, discord_id: int) -> bool:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT 1 FROM mafia_optout WHERE discord_id = ? AND guild_id = ?",
                (discord_id, gid),
            )
            return cursor.fetchone() is not None

    # Signups are a single pending bucket per guild (no calendar), consumed and
    # cleared when the next game starts. The week_start column holds a sentinel.
    _SIGNUP_BUCKET = "pending"

    def add_signup(self, guild_id: int | None, discord_id: int) -> None:
        """Opt-in roster priority for the next game (/mafia join)."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            conn.cursor().execute(
                """
                INSERT OR IGNORE INTO mafia_signups (guild_id, week_start, discord_id, created_at)
                VALUES (?, ?, ?, ?)
                """,
                (gid, self._SIGNUP_BUCKET, discord_id, int(time.time())),
            )

    def get_signups(self, guild_id: int | None) -> list[int]:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT discord_id FROM mafia_signups WHERE guild_id = ? AND week_start = ?",
                (gid, self._SIGNUP_BUCKET),
            )
            return [row["discord_id"] for row in cursor.fetchall()]

    def clear_signups(self, guild_id: int | None) -> None:
        """Consume the pending signup bucket once a game has started."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            conn.cursor().execute(
                "DELETE FROM mafia_signups WHERE guild_id = ? AND week_start = ?",
                (gid, self._SIGNUP_BUCKET),
            )

    # ── History, leaderboard, stats ─────────────────────────────────────────

    def get_player_history(
        self, discord_id: int, guild_id: int | None, limit: int = 20
    ) -> list[dict]:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT g.game_id, g.game_date, g.winner, g.twist_event,
                       g.payout_per_winner, g.mvp_id,
                       p.role, p.is_godfather, p.hero_name, p.is_alive,
                       p.eliminated_phase, p.acted
                FROM mafia_players p
                JOIN mafia_games g ON g.game_id = p.game_id
                WHERE p.discord_id = ? AND p.guild_id = ?
                  AND g.phase = ?
                ORDER BY g.game_id DESC LIMIT ?
                """,
                (discord_id, gid, MafiaPhase.RESOLVED.value, limit),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_leaderboard(self, guild_id: int | None, limit: int = 20) -> list[dict]:
        """Aggregate per-player stats across all resolved games for a guild."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    p.discord_id,
                    COUNT(*) AS games_played,
                    SUM(CASE
                        WHEN (g.winner = 'TOWN' AND p.role IN ('TOWNIE','DOCTOR','DETECTIVE','VIGILANTE'))
                          OR (g.winner = 'MAFIA' AND p.role = 'MAFIA')
                          OR (g.winner = 'JESTER' AND p.role = 'JESTER')
                        THEN 1 ELSE 0
                    END) AS wins,
                    SUM(CASE WHEN g.winner = 'MAFIA' AND p.role = 'MAFIA' THEN 1 ELSE 0 END) AS mafia_wins,
                    SUM(CASE WHEN g.winner = 'TOWN' AND p.role IN ('TOWNIE','DOCTOR','DETECTIVE','VIGILANTE') THEN 1 ELSE 0 END) AS town_wins,
                    SUM(CASE WHEN g.winner = 'JESTER' AND p.role = 'JESTER' THEN 1 ELSE 0 END) AS jester_wins,
                    SUM(CASE WHEN g.mvp_id = p.discord_id THEN 1 ELSE 0 END) AS mvp_count
                FROM mafia_players p
                JOIN mafia_games g ON g.game_id = p.game_id
                WHERE p.guild_id = ? AND g.phase = ?
                GROUP BY p.discord_id
                ORDER BY wins DESC, games_played DESC
                LIMIT ?
                """,
                (gid, MafiaPhase.RESOLVED.value, limit),
            )
            return [dict(row) for row in cursor.fetchall()]

    def compute_player_stats(self, guild_id: int | None, discord_id: int) -> dict:
        """Aggregate stats used to derive titles."""
        gid = self.normalize_guild_id(guild_id)
        stats = {
            "games_played": 0,
            "wins": 0,
            "mafia_wins": 0,
            "town_wins": 0,
            "jester_wins": 0,
            "mafia_kills": 0,
            "doctor_saves": 0,
            "correct_reads": 0,
        }
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    COUNT(*) AS games_played,
                    SUM(CASE
                        WHEN (g.winner = 'TOWN' AND p.role IN ('TOWNIE','DOCTOR','DETECTIVE','VIGILANTE'))
                          OR (g.winner = 'MAFIA' AND p.role = 'MAFIA')
                          OR (g.winner = 'JESTER' AND p.role = 'JESTER')
                        THEN 1 ELSE 0
                    END) AS wins,
                    SUM(CASE WHEN g.winner = 'MAFIA' AND p.role = 'MAFIA' THEN 1 ELSE 0 END) AS mafia_wins,
                    SUM(CASE WHEN g.winner = 'TOWN' AND p.role IN ('TOWNIE','DOCTOR','DETECTIVE','VIGILANTE') THEN 1 ELSE 0 END) AS town_wins,
                    SUM(CASE WHEN g.winner = 'JESTER' AND p.role = 'JESTER' THEN 1 ELSE 0 END) AS jester_wins
                FROM mafia_players p
                JOIN mafia_games g ON g.game_id = p.game_id
                WHERE p.discord_id = ? AND p.guild_id = ? AND g.phase = ?
                """,
                (discord_id, gid, MafiaPhase.RESOLVED.value),
            )
            row = cursor.fetchone()
            if row:
                for key in ("games_played", "wins", "mafia_wins", "town_wins", "jester_wins"):
                    stats[key] = row[key] or 0

            # Mafia kills: KILL actions by this player whose target died at NIGHT in the same game.
            cursor.execute(
                """
                SELECT COUNT(*) FROM mafia_actions a
                JOIN mafia_players victim
                  ON victim.game_id = a.game_id AND victim.discord_id = a.target_id
                JOIN mafia_games g ON g.game_id = a.game_id
                WHERE a.actor_id = ? AND g.guild_id = ?
                  AND a.action_type = 'KILL'
                  AND victim.eliminated_phase = 'NIGHT'
                  AND g.phase = ?
                """,
                (discord_id, gid, MafiaPhase.RESOLVED.value),
            )
            stats["mafia_kills"] = cursor.fetchone()[0] or 0

            # Doctor saves: SAVE actions where the protected target was the actual mafia kill target
            # AND target ended the night still alive. Approximation: save row whose target stayed alive
            # in a game where mafia submitted a KILL on that same target.
            cursor.execute(
                """
                SELECT COUNT(*) FROM mafia_actions s
                JOIN mafia_players target
                  ON target.game_id = s.game_id AND target.discord_id = s.target_id
                JOIN mafia_games g ON g.game_id = s.game_id
                WHERE s.actor_id = ? AND g.guild_id = ?
                  AND s.action_type = 'SAVE'
                  AND target.eliminated_phase IS NULL
                  AND EXISTS (
                      SELECT 1 FROM mafia_actions k
                      WHERE k.game_id = s.game_id
                        AND k.action_type = 'KILL'
                        AND k.target_id = s.target_id
                  )
                  AND g.phase = ?
                """,
                (discord_id, gid, MafiaPhase.RESOLVED.value),
            )
            stats["doctor_saves"] = cursor.fetchone()[0] or 0

            # Correct reads: INVESTIGATE actions where target was a non-Godfather mafia
            # AND that target was eventually lynched in the same game.
            cursor.execute(
                """
                SELECT COUNT(*) FROM mafia_actions i
                JOIN mafia_players target
                  ON target.game_id = i.game_id AND target.discord_id = i.target_id
                JOIN mafia_games g ON g.game_id = i.game_id
                WHERE i.actor_id = ? AND g.guild_id = ?
                  AND i.action_type = 'INVESTIGATE'
                  AND target.role = 'MAFIA' AND target.is_godfather = 0
                  AND target.eliminated_phase = 'DAY'
                  AND g.phase = ?
                """,
                (discord_id, gid, MafiaPhase.RESOLVED.value),
            )
            stats["correct_reads"] = cursor.fetchone()[0] or 0

        return stats
