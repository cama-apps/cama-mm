"""Repository for the Witch's Curse feature."""

from __future__ import annotations

import logging
import time

from repositories.base_repository import BaseRepository
from repositories.interfaces import ICurseRepository

logger = logging.getLogger("cama_bot.repositories.curse")

_SECONDS_PER_DAY = 86400


class CurseRepository(BaseRepository, ICurseRepository):
    """Persists per-target curses with anonymous casters.

    One row per (caster, target, guild) active curse. Multiple rows allowed for
    a single target. Casting again from the same caster extends the existing
    row's expiry rather than inserting a new one.
    """

    def cast_or_extend(
        self,
        guild_id: int | None,
        caster_id: int,
        target_id: int,
        days: int,
    ) -> int:
        normalized_guild = self.normalize_guild_id(guild_id)
        now = int(time.time())
        extension = days * _SECONDS_PER_DAY

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT id, expires_at FROM curses
                WHERE caster_discord_id = ?
                  AND target_discord_id = ?
                  AND guild_id = ?
                  AND expires_at > ?
                ORDER BY expires_at DESC
                LIMIT 1
                """,
                (caster_id, target_id, normalized_guild, now),
            )
            row = cursor.fetchone()

            if row is not None:
                new_expiry = row["expires_at"] + extension
                cursor.execute(
                    "UPDATE curses SET expires_at = ? WHERE id = ?",
                    (new_expiry, row["id"]),
                )
                return new_expiry

            new_expiry = now + extension
            cursor.execute(
                """
                INSERT INTO curses
                    (target_discord_id, guild_id, caster_discord_id, expires_at)
                VALUES (?, ?, ?, ?)
                """,
                (target_id, normalized_guild, caster_id, new_expiry),
            )
            return new_expiry

    def count_active_curses_for_target(
        self,
        target_id: int,
        guild_id: int | None,
        now: int,
    ) -> int:
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT COUNT(*) AS n FROM curses
                WHERE target_discord_id = ?
                  AND guild_id = ?
                  AND expires_at > ?
                """,
                (target_id, normalized_guild, now),
            )
            row = cursor.fetchone()
            return int(row["n"]) if row else 0
