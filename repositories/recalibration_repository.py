"""
Repository for recalibration state tracking.
"""

from repositories.base_repository import BaseRepository
from repositories.interfaces import IRecalibrationRepository


class RecalibrationRepository(BaseRepository, IRecalibrationRepository):
    """Data access for recalibration state."""

    def get_state(self, discord_id: int, guild_id: int) -> dict | None:
        """Get recalibration state for a player."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT discord_id, guild_id, last_recalibration_at, total_recalibrations,
                       rating_at_recalibration
                FROM recalibration_state
                WHERE discord_id = ? AND guild_id = ?
                """,
                (discord_id, guild_id),
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
        guild_id: int,
        last_recalibration_at: int | None = None,
        total_recalibrations: int | None = None,
        rating_at_recalibration: float | None = None,
    ) -> None:
        """Create or update recalibration state."""
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
                (discord_id, guild_id, last_recalibration_at, total_recalibrations, rating_at_recalibration),
            )

    def reset_cooldown(self, discord_id: int, guild_id: int) -> None:
        """Reset recalibration cooldown by setting last_recalibration_at to 0."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE recalibration_state
                SET last_recalibration_at = 0, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (discord_id, guild_id),
            )
