"""
Repository for bankruptcy state data access.
"""

from repositories.base_repository import BaseRepository
from repositories.interfaces import IBankruptcyRepository


class BankruptcyRepository(BaseRepository, IBankruptcyRepository):
    """Data access for bankruptcy state."""

    def get_state(self, discord_id: int, guild_id: int | None = None) -> dict | None:
        """Get bankruptcy state for a player."""
        normalized_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT discord_id, guild_id, last_bankruptcy_at, penalty_games_remaining,
                       COALESCE(bankruptcy_count, 0) as bankruptcy_count
                FROM bankruptcy_state
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
                "last_bankruptcy_at": row["last_bankruptcy_at"],
                "penalty_games_remaining": row["penalty_games_remaining"],
                "bankruptcy_count": row["bankruptcy_count"],
            }

    def get_bulk_states(self, discord_ids: list[int], guild_id: int | None = None) -> dict[int, dict]:
        """Get bankruptcy states for multiple players in one query.

        Args:
            discord_ids: List of Discord IDs to fetch states for.
            guild_id: Guild ID for multi-guild support.

        Returns:
            Dict mapping discord_id to state dict. Only includes users
            who have bankruptcy records in the database (excludes users
            who have never declared bankruptcy). Empty input returns empty dict.
        """
        if not discord_ids:
            return {}
        normalized_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            placeholders = ",".join("?" * len(discord_ids))
            cursor.execute(
                f"""
                SELECT discord_id, guild_id, last_bankruptcy_at, penalty_games_remaining,
                       COALESCE(bankruptcy_count, 0) as bankruptcy_count
                FROM bankruptcy_state
                WHERE discord_id IN ({placeholders}) AND guild_id = ?
                """,
                (*discord_ids, normalized_id),
            )
            return {row["discord_id"]: dict(row) for row in cursor.fetchall()}

    def upsert_state(
        self, discord_id: int, guild_id: int | None, last_bankruptcy_at: int, penalty_games_remaining: int
    ) -> None:
        """Create or update bankruptcy state, incrementing bankruptcy_count."""
        normalized_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO bankruptcy_state (discord_id, guild_id, last_bankruptcy_at, penalty_games_remaining, bankruptcy_count, updated_at)
                VALUES (?, ?, ?, ?, 1, CURRENT_TIMESTAMP)
                ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                    last_bankruptcy_at = excluded.last_bankruptcy_at,
                    penalty_games_remaining = excluded.penalty_games_remaining,
                    bankruptcy_count = COALESCE(bankruptcy_state.bankruptcy_count, 0) + 1,
                    updated_at = CURRENT_TIMESTAMP
                """,
                (discord_id, normalized_id, last_bankruptcy_at, penalty_games_remaining),
            )

    def reset_cooldown_only(
        self, discord_id: int, guild_id: int | None, last_bankruptcy_at: int, penalty_games_remaining: int
    ) -> None:
        """Reset cooldown and penalty without incrementing bankruptcy_count."""
        normalized_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE bankruptcy_state
                SET last_bankruptcy_at = ?,
                    penalty_games_remaining = ?,
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (last_bankruptcy_at, penalty_games_remaining, discord_id, normalized_id),
            )

    def decrement_penalty_games(self, discord_id: int, guild_id: int | None = None) -> int:
        """
        Decrement penalty games remaining by 1 if > 0.

        Returns the new count.
        """
        normalized_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE bankruptcy_state
                SET penalty_games_remaining = MAX(0, penalty_games_remaining - 1),
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (discord_id, normalized_id),
            )
            cursor.execute(
                "SELECT penalty_games_remaining FROM bankruptcy_state WHERE discord_id = ? AND guild_id = ?",
                (discord_id, normalized_id),
            )
            row = cursor.fetchone()
            return row["penalty_games_remaining"] if row else 0

    def get_penalty_games(self, discord_id: int, guild_id: int | None = None) -> int:
        """Get the number of penalty games remaining for a player."""
        normalized_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT penalty_games_remaining FROM bankruptcy_state WHERE discord_id = ? AND guild_id = ?",
                (discord_id, normalized_id),
            )
            row = cursor.fetchone()
            return row["penalty_games_remaining"] if row else 0
