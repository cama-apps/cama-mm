"""
Typing tracker for monitoring recent typing activity in Discord channels.

Tracks when users are typing in threads/channels to help detect AFK players.
"""

import logging
from datetime import datetime, timedelta
from threading import Lock

logger = logging.getLogger("cama_bot.utils.typing_tracker")


class TypingTracker:
    """
    Thread-safe tracker for recent typing events.

    Stores the last typing timestamp for each (guild_id, user_id) pair.
    Used to detect if a player has recently shown typing activity.
    """

    def __init__(self, default_window_seconds: int = 10):
        """
        Initialize the typing tracker.

        Args:
            default_window_seconds: Default time window for considering typing "recent"
        """
        self.default_window_seconds = default_window_seconds
        self._typing_cache: dict[tuple[int, int], datetime] = {}
        self._lock = Lock()

    def mark_typing(self, guild_id: int, user_id: int) -> None:
        """
        Record a typing event for a user in a guild.

        Args:
            guild_id: Discord guild ID
            user_id: Discord user ID
        """
        with self._lock:
            self._typing_cache[(guild_id, user_id)] = datetime.now()
            logger.debug(f"Marked typing for user {user_id} in guild {guild_id}")

    def is_typing_recently(
        self, guild_id: int, user_id: int, window_seconds: int | None = None
    ) -> bool:
        """
        Check if a user has typed recently in a guild.

        Args:
            guild_id: Discord guild ID
            user_id: Discord user ID
            window_seconds: Time window in seconds (uses default if None)

        Returns:
            True if user typed within the time window
        """
        if window_seconds is None:
            window_seconds = self.default_window_seconds

        with self._lock:
            last_typing = self._typing_cache.get((guild_id, user_id))
            if not last_typing:
                return False

            elapsed = (datetime.now() - last_typing).total_seconds()
            return elapsed < window_seconds

    def cleanup_old_entries(self, max_age_seconds: int = 300) -> int:
        """
        Remove typing records older than max_age_seconds.

        Args:
            max_age_seconds: Maximum age before cleanup (default 5 minutes)

        Returns:
            Number of entries removed
        """
        cutoff = datetime.now() - timedelta(seconds=max_age_seconds)
        with self._lock:
            old_count = len(self._typing_cache)
            self._typing_cache = {
                key: timestamp
                for key, timestamp in self._typing_cache.items()
                if timestamp >= cutoff
            }
            removed = old_count - len(self._typing_cache)
            if removed > 0:
                logger.debug(f"Cleaned up {removed} old typing entries")
            return removed

    def clear(self) -> None:
        """Clear all typing records (useful for testing)."""
        with self._lock:
            self._typing_cache.clear()
            logger.debug("Cleared all typing records")
