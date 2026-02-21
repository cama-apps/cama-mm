"""
Repository for neon degen terminal event persistence.
"""

import logging
import time

from repositories.base_repository import BaseRepository

logger = logging.getLogger("cama_bot.repositories.neon_event")


class NeonEventRepository(BaseRepository):
    """Data access for neon terminal one-time event tracking."""

    def load_one_time_events(self) -> list[tuple[int, int, str]]:
        """
        Load all one-time events from the database.

        Returns:
            List of (discord_id, guild_id, event_type) tuples.
        """
        try:
            with self.connection() as conn:
                cursor = conn.cursor()
                cursor.execute(
                    "SELECT discord_id, guild_id, event_type FROM neon_events WHERE one_time = 1"
                )
                return [(row["discord_id"], row["guild_id"], row["event_type"]) for row in cursor.fetchall()]
        except Exception as e:
            logger.debug(f"Failed to load one-time events: {e}")
            return []

    def check_one_time_event(self, discord_id: int, guild_id: int, event_type: str) -> bool:
        """
        Check if a one-time event exists in the database.

        Returns:
            True if the event has already been triggered.
        """
        try:
            with self.connection() as conn:
                cursor = conn.cursor()
                cursor.execute(
                    "SELECT 1 FROM neon_events WHERE discord_id = ? AND guild_id = ? "
                    "AND event_type = ? AND one_time = 1 LIMIT 1",
                    (discord_id, guild_id, event_type),
                )
                return cursor.fetchone() is not None
        except Exception as e:
            logger.debug(f"Failed to check one-time event: {e}")
            return False

    def persist_one_time_event(self, discord_id: int, guild_id: int, event_type: str, layer: int) -> None:
        """
        Persist a one-time event to the database.
        """
        try:
            with self.connection() as conn:
                cursor = conn.cursor()
                cursor.execute(
                    "INSERT OR IGNORE INTO neon_events "
                    "(discord_id, guild_id, event_type, layer, one_time, fired_at) "
                    "VALUES (?, ?, ?, ?, 1, ?)",
                    (discord_id, guild_id, event_type, layer, int(time.time())),
                )
        except Exception as e:
            logger.debug(f"Failed to persist one-time event: {e}")
