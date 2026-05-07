"""Repository for guild-wide dig modifiers with expiry.

One row per ``(guild_id, modifier_id)``. On re-trigger the row is upserted
and ``expires_at`` extended. Queries filter ``WHERE expires_at > now``;
expired rows are pruned lazily by ``clear_expired``.
"""

from __future__ import annotations

import json
import logging
import time

from repositories.base_repository import BaseRepository
from repositories.interfaces import IDigGuildModifierRepository

logger = logging.getLogger("cama_bot.repositories.dig_guild_modifier")


class DigGuildModifierRepository(BaseRepository, IDigGuildModifierRepository):
    def set_modifier(
        self,
        guild_id: int | None,
        modifier_id: str,
        duration_seconds: int,
        payload: dict | None = None,
    ) -> int:
        gid = self.normalize_guild_id(guild_id)
        now = int(time.time())
        new_expiry = now + max(0, int(duration_seconds))
        payload_json = json.dumps(payload or {})

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT expires_at FROM dig_guild_modifiers
                WHERE guild_id = ? AND modifier_id = ?
                """,
                (gid, modifier_id),
            )
            row = cursor.fetchone()
            if row is not None:
                # Extend if the existing row is still active; otherwise reset
                # the expiry from now.
                base = max(int(row["expires_at"]), now)
                new_expiry = base + max(0, int(duration_seconds))
                cursor.execute(
                    """
                    UPDATE dig_guild_modifiers
                       SET expires_at = ?, payload_json = ?
                     WHERE guild_id = ? AND modifier_id = ?
                    """,
                    (new_expiry, payload_json, gid, modifier_id),
                )
                return new_expiry

            cursor.execute(
                """
                INSERT INTO dig_guild_modifiers
                    (guild_id, modifier_id, expires_at, payload_json, created_at)
                VALUES (?, ?, ?, ?, ?)
                """,
                (gid, modifier_id, new_expiry, payload_json, now),
            )
            return new_expiry

    def get_active(self, guild_id: int | None, now: int | None = None) -> list[dict]:
        gid = self.normalize_guild_id(guild_id)
        ts = int(now if now is not None else time.time())
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT modifier_id, expires_at, payload_json
                  FROM dig_guild_modifiers
                 WHERE guild_id = ? AND expires_at > ?
                """,
                (gid, ts),
            )
            rows = cursor.fetchall()
        out: list[dict] = []
        for row in rows:
            try:
                payload = json.loads(row["payload_json"]) if row["payload_json"] else {}
            except (json.JSONDecodeError, TypeError):
                payload = {}
            out.append({
                "modifier_id": row["modifier_id"],
                "expires_at": int(row["expires_at"]),
                "payload": payload,
            })
        return out

    def is_active(self, guild_id: int | None, modifier_id: str, now: int | None = None) -> bool:
        gid = self.normalize_guild_id(guild_id)
        ts = int(now if now is not None else time.time())
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT 1 FROM dig_guild_modifiers
                 WHERE guild_id = ? AND modifier_id = ? AND expires_at > ?
                 LIMIT 1
                """,
                (gid, modifier_id, ts),
            )
            return cursor.fetchone() is not None

    def clear_expired(self, now: int | None = None) -> int:
        ts = int(now if now is not None else time.time())
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "DELETE FROM dig_guild_modifiers WHERE expires_at <= ?",
                (ts,),
            )
            return cursor.rowcount or 0
