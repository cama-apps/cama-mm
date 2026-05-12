"""Repository for time-limited manashop buffs.

Wraps the ``manashop_buffs`` table. Buffs are produced by manashop ultimates
(Counterspell, Aegis, Overgrowth, Sanctuary, Blood Pact, Dark Bargain) and
24h Mid items like Aegis. Each row is a single buff instance with a hard
``expires_at`` epoch second; ``triggered`` flips to 1 when the buff is
consumed (e.g. Aegis absorbing one PvP attack) for its lifetime.
"""

import json
import time
from typing import Any

from repositories.base_repository import BaseRepository, safe_json_loads


class BuffRepository(BaseRepository):
    """Stores time-limited manashop buffs."""

    def grant(
        self,
        discord_id: int,
        guild_id: int | None,
        buff_type: str,
        expires_at: int,
        *,
        target_id: int | None = None,
        data: dict | None = None,
    ) -> int:
        """Insert a new buff row. Returns the new buff id."""
        gid = self.normalize_guild_id(guild_id)
        granted_at = int(time.time())
        data_json = json.dumps(data) if data else None
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO manashop_buffs
                (discord_id, guild_id, buff_type, target_id, granted_at, expires_at, triggered, data)
                VALUES (?, ?, ?, ?, ?, ?, 0, ?)
                """,
                (discord_id, gid, buff_type, target_id, granted_at, expires_at, data_json),
            )
            return int(cursor.lastrowid or 0)

    def active_for(
        self, discord_id: int, guild_id: int | None, buff_type: str
    ) -> list[dict]:
        """Return all non-triggered, non-expired buffs of ``buff_type`` owned
        by ``discord_id``. Most recent first."""
        gid = self.normalize_guild_id(guild_id)
        now = int(time.time())
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT id, discord_id, guild_id, buff_type, target_id,
                       granted_at, expires_at, triggered, data
                FROM manashop_buffs
                WHERE discord_id = ? AND guild_id = ? AND buff_type = ?
                  AND triggered = 0 AND expires_at > ?
                ORDER BY granted_at DESC
                """,
                (discord_id, gid, buff_type, now),
            )
            rows = [dict(row) for row in cursor.fetchall()]
            for row in rows:
                row["data"] = safe_json_loads(row.get("data"), {}, context="manashop_buffs.data")
            return rows

    def has_active(
        self, discord_id: int, guild_id: int | None, buff_type: str
    ) -> bool:
        """Return True if any non-triggered, non-expired buff of ``buff_type``
        exists for the player."""
        return bool(self.active_for(discord_id, guild_id, buff_type))

    def active_targeted_at(
        self, target_id: int, guild_id: int | None, buff_type: str
    ) -> list[dict]:
        """Return all non-triggered, non-expired buffs of ``buff_type`` whose
        ``target_id`` is the given player (e.g. Sanctuary on an ally,
        Blood Pact on a victim)."""
        gid = self.normalize_guild_id(guild_id)
        now = int(time.time())
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT id, discord_id, guild_id, buff_type, target_id,
                       granted_at, expires_at, triggered, data
                FROM manashop_buffs
                WHERE target_id = ? AND guild_id = ? AND buff_type = ?
                  AND triggered = 0 AND expires_at > ?
                ORDER BY granted_at DESC
                """,
                (target_id, gid, buff_type, now),
            )
            rows = [dict(row) for row in cursor.fetchall()]
            for row in rows:
                row["data"] = safe_json_loads(row.get("data"), {}, context="manashop_buffs.data")
            return rows

    def refresh_atomic(
        self,
        discord_id: int,
        guild_id: int | None,
        buff_type: str,
        expires_at: int,
        *,
        target_id: int | None = None,
        data: dict | None = None,
    ) -> int:
        """Atomically expire all active buffs of ``buff_type`` for the player
        and insert a fresh one. Closes the consume-then-grant race so
        concurrent re-purchases cannot leave two active rows.
        """
        gid = self.normalize_guild_id(guild_id)
        granted_at = int(time.time())
        data_json = json.dumps(data) if data else None
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "UPDATE manashop_buffs SET triggered = 1 "
                "WHERE discord_id = ? AND guild_id = ? AND buff_type = ? "
                "AND triggered = 0 AND expires_at > ?",
                (discord_id, gid, buff_type, granted_at),
            )
            cursor.execute(
                """
                INSERT INTO manashop_buffs
                (discord_id, guild_id, buff_type, target_id, granted_at, expires_at, triggered, data)
                VALUES (?, ?, ?, ?, ?, ?, 0, ?)
                """,
                (discord_id, gid, buff_type, target_id, granted_at, expires_at, data_json),
            )
            return int(cursor.lastrowid or 0)

    def consume_atomic(self, buff_id: int) -> bool:
        """Atomically mark a buff as triggered. Returns True if the flag
        flipped (claim succeeded), False if it was already triggered or the
        row doesn't exist."""
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "UPDATE manashop_buffs SET triggered = 1 "
                "WHERE id = ? AND triggered = 0",
                (buff_id,),
            )
            return cursor.rowcount > 0

    def update_data(self, buff_id: int, data: dict[str, Any]) -> None:
        """Overwrite the JSON ``data`` blob for a buff. Used for buffs that
        accumulate state (e.g. Blood Pact's running skim total)."""
        data_json = json.dumps(data)
        with self.connection() as conn:
            conn.execute(
                "UPDATE manashop_buffs SET data = ? WHERE id = ?",
                (data_json, buff_id),
            )

    def cleanup_expired(self, *, before: int | None = None) -> int:
        """Delete expired/triggered buff rows. Returns number of rows pruned.

        Lazy maintenance — call from a daily reset path or periodic cleanup.
        """
        cutoff = before if before is not None else int(time.time())
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "DELETE FROM manashop_buffs "
                "WHERE triggered = 1 OR expires_at <= ?",
                (cutoff,),
            )
            return cursor.rowcount or 0
