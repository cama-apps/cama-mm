"""
Queue system for managing players waiting to play.
"""

from collections import deque


class PlayerQueue:
    """Manages a queue of players waiting to play."""

    def __init__(self):
        """Initialize empty queue."""
        self.queue: deque = deque()
        self.player_ids: set[int] = set()  # Track who's in queue to prevent duplicates

    def add_player(self, discord_id: int) -> bool:
        """
        Add a player to the queue.

        Args:
            discord_id: Discord user ID

        Returns:
            True if added, False if already in queue
        """
        if discord_id in self.player_ids:
            return False

        self.queue.append(discord_id)
        self.player_ids.add(discord_id)
        return True

    def remove_player(self, discord_id: int) -> bool:
        """
        Remove a player from the queue.

        Args:
            discord_id: Discord user ID

        Returns:
            True if removed, False if not in queue
        """
        if discord_id not in self.player_ids:
            return False

        # Remove from queue (may need to rebuild to maintain order)
        self.queue = deque([pid for pid in self.queue if pid != discord_id])
        self.player_ids.remove(discord_id)
        return True

    def get_players(self, count: int = 10) -> list[int]:
        """
        Get players from queue (removes them from queue).

        Args:
            count: Number of players to get (default 10)

        Returns:
            List of Discord IDs
        """
        players = []
        for _ in range(min(count, len(self.queue))):
            player_id = self.queue.popleft()
            self.player_ids.remove(player_id)
            players.append(player_id)

        return players

    def peek(self, count: int = 10) -> list[int]:
        """
        Peek at players in queue without removing them.

        Args:
            count: Number of players to peek at

        Returns:
            List of Discord IDs
        """
        return [self.queue[i] for i in range(min(count, len(self.queue)))]

    def clear(self):
        """Clear the entire queue."""
        self.queue.clear()
        self.player_ids.clear()

    def size(self) -> int:
        """Get queue size."""
        return len(self.queue)

    def is_in_queue(self, discord_id: int) -> bool:
        """Check if player is in queue."""
        return discord_id in self.player_ids

    def get_all(self) -> list[int]:
        """Get all players in queue without removing them."""
        return list(self.queue)
