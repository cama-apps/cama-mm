"""Repository for Daily Mafia subgame data access."""

import time

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
        payout_per_winner=row["payout_per_winner"] or 0,
        mvp_id=row["mvp_id"],
        mafia_thread_id=row["mafia_thread_id"],
        discussion_thread_id=row["discussion_thread_id"],
        setup_message_id=row["setup_message_id"],
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
    ) -> int:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO mafia_games (
                    guild_id, game_date, phase, started_at, roster_size, twist_event
                ) VALUES (?, ?, ?, ?, ?, ?)
                """,
                (
                    gid,
                    game_date,
                    phase.value,
                    started_at,
                    roster_size,
                    twist_event.value if twist_event else None,
                ),
            )
            return cursor.lastrowid

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

    def set_thread_ids(
        self,
        game_id: int,
        *,
        mafia_thread_id: int | None = None,
        discussion_thread_id: int | None = None,
        setup_message_id: int | None = None,
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
    ) -> None:
        """UPSERT an action and mark the actor as having acted."""
        gid = self.normalize_guild_id(guild_id)
        now = int(time.time())
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO mafia_actions (
                    game_id, guild_id, actor_id, target_id, action_type, phase, created_at, result
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(game_id, actor_id, action_type, phase) DO UPDATE SET
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
    ) -> list[dict]:
        clauses = ["game_id = ?"]
        params: list = [game_id]
        if action_type is not None:
            clauses.append("action_type = ?")
            params.append(action_type.value)
        if phase is not None:
            clauses.append("phase = ?")
            params.append(phase.value)
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
    ) -> dict | None:
        """Find the most recent action of a given type by an actor across any phase."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT * FROM mafia_actions
                WHERE game_id = ? AND actor_id = ? AND action_type = ?
                ORDER BY created_at DESC LIMIT 1
                """,
                (game_id, actor_id, action_type.value),
            )
            row = cursor.fetchone()
            return dict(row) if row else None

    # ── Eligibility & opt-out ───────────────────────────────────────────────

    def get_eligible_player_ids(self, guild_id: int | None, since: int) -> list[int]:
        """Distinct discord_ids active in /gamba or /dig within the last `since` window.

        `since` is a unix timestamp lower-bound. Excludes opted-out players.
        """
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT DISTINCT discord_id FROM (
                    SELECT discord_id FROM wheel_spins
                      WHERE guild_id = ? AND spin_time >= ?
                    UNION
                    SELECT actor_id AS discord_id FROM dig_actions
                      WHERE guild_id = ? AND action_type = 'dig' AND created_at >= ?
                ) AS recent
                WHERE discord_id NOT IN (
                    SELECT discord_id FROM mafia_optout WHERE guild_id = ?
                )
                """,
                (gid, since, gid, since, gid),
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
