"""
Reaction tracker for monitoring sword emoji reactions on lobby messages.

Tracks timestamps of ⚔️ reactions to help detect recent engagement with the lobby.
Discord API doesn't expose reaction timestamps, so we track them via events.
"""

import logging
from datetime import datetime, timedelta
from threading import Lock

logger = logging.getLogger("cama_bot.utils.reaction_tracker")


class ReactionTracker:
    """
    Thread-safe tracker for reaction timestamps.

    Stores reaction timestamps per message to detect recent lobby engagement.
    Only tracks ⚔️ (sword) emoji reactions on lobby messages.
    """

    def __init__(self):
        """Initialize the reaction tracker."""
        # Structure: {message_id: {user_id: timestamp}}
        self._reactions: dict[int, dict[int, datetime]] = {}
        self._lock = Lock()

    def track_reaction(self, message_id: int, user_id: int) -> None:
        """
        Record a ⚔️ reaction event.

        Args:
            message_id: Discord message ID that was reacted to
            user_id: Discord user ID who reacted
        """
        with self._lock:
            if message_id not in self._reactions:
                self._reactions[message_id] = {}

            self._reactions[message_id][user_id] = datetime.now()
            logger.debug(
                f"Tracked ⚔️ reaction: user {user_id} on message {message_id}"
            )

    def check_recent_reaction(
        self, message_id: int, user_id: int, window_seconds: int
    ) -> tuple[bool, datetime | None]:
        """
        Check if a user reacted to a message within the time window.

        Args:
            message_id: Discord message ID to check
            user_id: Discord user ID to check
            window_seconds: Time window in seconds

        Returns:
            (has_recent_reaction, timestamp_or_none)
        """
        with self._lock:
            message_reactions = self._reactions.get(message_id)
            if not message_reactions:
                return False, None

            reaction_time = message_reactions.get(user_id)
            if not reaction_time:
                return False, None

            elapsed = (datetime.now() - reaction_time).total_seconds()
            if elapsed < window_seconds:
                return True, reaction_time
            else:
                return False, None

    def remove_reaction(self, message_id: int, user_id: int) -> None:
        """
        Remove a tracked reaction (e.g., when user un-reacts).

        Args:
            message_id: Discord message ID
            user_id: Discord user ID
        """
        with self._lock:
            if message_id in self._reactions:
                self._reactions[message_id].pop(user_id, None)
                logger.debug(
                    f"Removed reaction: user {user_id} from message {message_id}"
                )

                # Clean up empty message entries
                if not self._reactions[message_id]:
                    del self._reactions[message_id]

    def cleanup_old_reactions(self, max_age_seconds: int = 300) -> int:
        """
        Remove reaction records older than max_age_seconds.

        Args:
            max_age_seconds: Maximum age before cleanup (default 5 minutes)

        Returns:
            Number of reactions removed
        """
        cutoff = datetime.now() - timedelta(seconds=max_age_seconds)
        removed_count = 0

        with self._lock:
            messages_to_delete = []

            for message_id, user_reactions in self._reactions.items():
                users_to_delete = []

                for user_id, timestamp in user_reactions.items():
                    if timestamp < cutoff:
                        users_to_delete.append(user_id)

                for user_id in users_to_delete:
                    del user_reactions[user_id]
                    removed_count += 1

                # Mark message for deletion if no reactions remain
                if not user_reactions:
                    messages_to_delete.append(message_id)

            # Delete empty message entries
            for message_id in messages_to_delete:
                del self._reactions[message_id]

            if removed_count > 0:
                logger.debug(f"Cleaned up {removed_count} old reaction entries")

            return removed_count

    def clear(self) -> None:
        """Clear all reaction records (useful for testing)."""
        with self._lock:
            self._reactions.clear()
            logger.debug("Cleared all reaction records")

    def get_message_reaction_count(self, message_id: int) -> int:
        """
        Get count of tracked reactions for a message.

        Args:
            message_id: Discord message ID

        Returns:
            Number of users who have reacted
        """
        with self._lock:
            return len(self._reactions.get(message_id, {}))
