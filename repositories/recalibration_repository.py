"""
Repository for recalibration state tracking.
"""

from repositories.base_repository import BaseRepository
from repositories.interfaces import IRecalibrationRepository


class RecalibrationRepository(BaseRepository, IRecalibrationRepository):
    """Data access for recalibration state."""

    def get_state(self, discord_id: int, guild_id: int | None = None) -> dict | None:
        """Get recalibration state for a player."""
        normalized_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT discord_id, guild_id, last_recalibration_at, total_recalibrations,
                       rating_at_recalibration
                FROM recalibration_state
                WHERE discord_id = ? AND guild_id = ?
                """,
                (discord_id, normalized_id),
            )
            row = cursor.fetchone()
            if not row:
                return None
            return {
                "discord_id": row["discord_id"],
                "guild_id": row["guild_id"],
                "last_recalibration_at": row["last_recalibration_at"],
                "total_recalibrations": row["total_recalibrations"],
                "rating_at_recalibration": row["rating_at_recalibration"],
            }

    def upsert_state(
        self,
        discord_id: int,
        guild_id: int | None = None,
        last_recalibration_at: int | None = None,
        total_recalibrations: int | None = None,
        rating_at_recalibration: float | None = None,
    ) -> None:
        """Create or update recalibration state."""
        normalized_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO recalibration_state (
                    discord_id, guild_id, last_recalibration_at, total_recalibrations,
                    rating_at_recalibration, updated_at
                )
                VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
                ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                    last_recalibration_at = COALESCE(excluded.last_recalibration_at, recalibration_state.last_recalibration_at),
                    total_recalibrations = COALESCE(excluded.total_recalibrations, recalibration_state.total_recalibrations),
                    rating_at_recalibration = COALESCE(excluded.rating_at_recalibration, recalibration_state.rating_at_recalibration),
                    updated_at = CURRENT_TIMESTAMP
                """,
                (discord_id, normalized_id, last_recalibration_at, total_recalibrations, rating_at_recalibration),
            )

    def execute_recalibration_atomic(
        self,
        discord_id: int,
        guild_id: int,
        now: int,
        cooldown_seconds: int,
        rating: float,
        new_rd: float,
        new_volatility: float,
    ) -> int:
        """Atomically validate cooldown, apply Glicko update, bump state.

        Closes the TOCTOU between the service-level cooldown check and the
        state write: two concurrent /recalibrate calls can no longer both pass
        the check and double-bump ``total_recalibrations``.

        Raises ``ValueError('ON_COOLDOWN:<remaining_seconds>')`` if cooldown
        is still active. Returns the new ``total_recalibrations`` value.
        """
        normalized_guild_id = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            cursor.execute(
                """
                SELECT last_recalibration_at, COALESCE(total_recalibrations, 0) AS total_recalibrations
                FROM recalibration_state
                WHERE discord_id = ? AND guild_id = ?
                """,
                (discord_id, normalized_guild_id),
            )
            row = cursor.fetchone()
            last_at = row["last_recalibration_at"] if row else None
            total = row["total_recalibrations"] if row else 0

            if last_at and (now - last_at) < cooldown_seconds:
                remaining = cooldown_seconds - (now - last_at)
                raise ValueError(f"ON_COOLDOWN:{remaining}")

            cursor.execute(
                """
                UPDATE players
                SET glicko_rating = ?, glicko_rd = ?, glicko_volatility = ?,
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (rating, new_rd, new_volatility, discord_id, normalized_guild_id),
            )

            new_total = total + 1
            cursor.execute(
                """
                INSERT INTO recalibration_state (
                    discord_id, guild_id, last_recalibration_at, total_recalibrations,
                    rating_at_recalibration, updated_at
                )
                VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
                ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                    last_recalibration_at = excluded.last_recalibration_at,
                    total_recalibrations = excluded.total_recalibrations,
                    rating_at_recalibration = excluded.rating_at_recalibration,
                    updated_at = CURRENT_TIMESTAMP
                """,
                (discord_id, normalized_guild_id, now, new_total, rating),
            )

            return new_total

    def reset_cooldown(self, discord_id: int, guild_id: int | None = None) -> None:
        """Reset recalibration cooldown by setting last_recalibration_at to 0."""
        normalized_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE recalibration_state
                SET last_recalibration_at = 0, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (discord_id, normalized_id),
            )
