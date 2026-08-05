"""Repository for guild-scoped low-priority matchmaking state."""

from dataclasses import dataclass

from domain.low_priority_constants import (
    LOW_PRIORITY_MAX_WINS,
    LOW_PRIORITY_MIN_WINS,
    LOW_PRIORITY_REQUIRED_WINS,
)
from repositories.base_repository import BaseRepository
from repositories.interfaces import ILowPriorityRepository


@dataclass(frozen=True)
class LowPriorityState:
    discord_id: int
    guild_id: int
    wins_required: int
    wins_remaining: int
    start_pending_match_id: int | None
    active: bool
    reason: str | None
    set_by: int
    removed_by: int | None
    removed_reason: str | None
    created_at: str
    updated_at: str


class LowPriorityRepository(BaseRepository, ILowPriorityRepository):
    """Persist and query low-priority state without economy side effects."""

    @staticmethod
    def _to_state(row) -> LowPriorityState:
        return LowPriorityState(
            discord_id=row["discord_id"],
            guild_id=row["guild_id"],
            wins_required=row["wins_required"],
            wins_remaining=row["wins_remaining"],
            start_pending_match_id=row["start_pending_match_id"],
            active=bool(row["active"]),
            reason=row["reason"],
            set_by=row["set_by"],
            removed_by=row["removed_by"],
            removed_reason=row["removed_reason"],
            created_at=row["created_at"],
            updated_at=row["updated_at"],
        )

    def get_state(
        self, discord_id: int, guild_id: int | None = None
    ) -> LowPriorityState | None:
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            row = conn.execute(
                """
                SELECT discord_id, guild_id, wins_required, wins_remaining,
                       start_pending_match_id, active, reason, set_by,
                       removed_by, removed_reason, created_at, updated_at
                FROM low_priority_state
                WHERE discord_id = ? AND guild_id = ?
                """,
                (discord_id, normalized_guild),
            ).fetchone()
        return self._to_state(row) if row else None

    def set_low_priority(
        self,
        discord_id: int,
        guild_id: int | None,
        *,
        set_by: int,
        reason: str | None,
        wins_required: int = LOW_PRIORITY_REQUIRED_WINS,
        start_pending_match_id: int | None = None,
    ) -> LowPriorityState:
        if not isinstance(wins_required, int) or isinstance(wins_required, bool):
            raise ValueError("wins_required must be an integer")
        if not LOW_PRIORITY_MIN_WINS <= wins_required <= LOW_PRIORITY_MAX_WINS:
            raise ValueError(
                f"wins_required must be between {LOW_PRIORITY_MIN_WINS} "
                f"and {LOW_PRIORITY_MAX_WINS}"
            )
        if (
            start_pending_match_id is not None
            and (
                not isinstance(start_pending_match_id, int)
                or isinstance(start_pending_match_id, bool)
                or start_pending_match_id < 0
            )
        ):
            raise ValueError("start_pending_match_id must be a non-negative integer or None")

        normalized_guild = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            conn.execute(
                """
                INSERT INTO low_priority_state (
                    discord_id, guild_id, wins_required, wins_remaining,
                    start_pending_match_id, active, reason, set_by, removed_by,
                    removed_reason, created_at, updated_at
                )
                VALUES (?, ?, ?, ?, ?, 1, ?, ?, NULL, NULL,
                        CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
                ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                    wins_required = excluded.wins_required,
                    wins_remaining = excluded.wins_remaining,
                    start_pending_match_id = excluded.start_pending_match_id,
                    active = 1,
                    reason = excluded.reason,
                    set_by = excluded.set_by,
                    removed_by = NULL,
                    removed_reason = NULL,
                    created_at = CURRENT_TIMESTAMP,
                    updated_at = CURRENT_TIMESTAMP
                """,
                (
                    discord_id,
                    normalized_guild,
                    wins_required,
                    wins_required,
                    start_pending_match_id,
                    reason,
                    set_by,
                ),
            )
            row = conn.execute(
                """
                SELECT discord_id, guild_id, wins_required, wins_remaining,
                       start_pending_match_id, active, reason, set_by,
                       removed_by, removed_reason, created_at, updated_at
                FROM low_priority_state
                WHERE discord_id = ? AND guild_id = ?
                """,
                (discord_id, normalized_guild),
            ).fetchone()
        if row is None:
            raise RuntimeError("Low-priority write succeeded but state was not found")
        return self._to_state(row)

    def clear_low_priority(
        self,
        discord_id: int,
        guild_id: int | None,
        *,
        removed_by: int,
        reason: str | None,
    ) -> bool:
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.execute(
                """
                UPDATE low_priority_state
                SET wins_remaining = 0,
                    active = 0,
                    removed_by = ?,
                    removed_reason = ?,
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ? AND active = 1
                """,
                (removed_by, reason, discord_id, normalized_guild),
            )
            return cursor.rowcount > 0

    def get_active_ids(
        self, discord_ids: list[int], guild_id: int | None = None
    ) -> set[int]:
        if not discord_ids:
            return set()
        normalized_guild = self.normalize_guild_id(guild_id)
        placeholders = ",".join("?" * len(discord_ids))
        with self.connection() as conn:
            rows = conn.execute(
                f"""
                SELECT discord_id
                FROM low_priority_state
                WHERE guild_id = ? AND active = 1 AND wins_remaining > 0
                  AND discord_id IN ({placeholders})
                """,
                (normalized_guild, *discord_ids),
            ).fetchall()
        return {row["discord_id"] for row in rows}

    def list_active(self, guild_id: int | None = None) -> list[LowPriorityState]:
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            rows = conn.execute(
                """
                SELECT discord_id, guild_id, wins_required, wins_remaining,
                       start_pending_match_id, active, reason, set_by,
                       removed_by, removed_reason, created_at, updated_at
                FROM low_priority_state
                WHERE guild_id = ? AND active = 1 AND wins_remaining > 0
                ORDER BY discord_id
                """,
                (normalized_guild,),
            ).fetchall()
        return [self._to_state(row) for row in rows]
