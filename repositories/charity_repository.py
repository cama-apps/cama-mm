"""
Repository for charity tracking (reduced blind rate for /paydebt contributors).
"""

import time

from repositories.base_repository import BaseRepository


class CharityRepository(BaseRepository):
    """
    Data access for charity tracker table.

    Tracks players who have earned reduced blind bet rates by
    helping others pay off their debt.
    """

    def get_state(self, discord_id: int) -> dict | None:
        """
        Get charity state for a player.

        Returns:
            Dict with reduced_rate_games_remaining, last_charity_at, total_charity_given
            or None if no record exists.
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT reduced_rate_games_remaining, last_charity_at, total_charity_given
                FROM charity_tracker
                WHERE discord_id = ?
                """,
                (discord_id,),
            )
            row = cursor.fetchone()
            if not row:
                return None
            return {
                "reduced_rate_games_remaining": row["reduced_rate_games_remaining"],
                "last_charity_at": row["last_charity_at"],
                "total_charity_given": row["total_charity_given"],
            }

    def get_reduced_rate_games(self, discord_id: int) -> int:
        """Get remaining games with reduced blind rate."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT reduced_rate_games_remaining FROM charity_tracker WHERE discord_id = ?",
                (discord_id,),
            )
            row = cursor.fetchone()
            return row["reduced_rate_games_remaining"] if row else 0

    def grant_reduced_rate(
        self, discord_id: int, games: int, amount: int, max_games: int = 2
    ) -> None:
        """
        Grant reduced blind rate games to a player.

        Args:
            discord_id: Player who made the charitable contribution
            games: Number of games to grant
            amount: Amount of charity given (for tracking total)
            max_games: Maximum games allowed (no stacking beyond this)
        """
        now = int(time.time())
        with self.connection() as conn:
            cursor = conn.cursor()
            # Upsert: insert or update
            cursor.execute(
                """
                INSERT INTO charity_tracker (
                    discord_id, reduced_rate_games_remaining, last_charity_at, total_charity_given
                )
                VALUES (?, ?, ?, ?)
                ON CONFLICT(discord_id) DO UPDATE SET
                    reduced_rate_games_remaining = MIN(?, reduced_rate_games_remaining + ?),
                    last_charity_at = ?,
                    total_charity_given = total_charity_given + ?,
                    updated_at = CURRENT_TIMESTAMP
                """,
                (
                    discord_id,
                    min(games, max_games),
                    now,
                    amount,
                    max_games,
                    games,
                    now,
                    amount,
                ),
            )

    def decrement_games_remaining(self, discord_id: int) -> int:
        """
        Decrement reduced rate games remaining by 1.

        Returns the new count (0 if not found or already 0).
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE charity_tracker
                SET reduced_rate_games_remaining = MAX(0, reduced_rate_games_remaining - 1),
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
                """,
                (discord_id,),
            )
            cursor.execute(
                "SELECT reduced_rate_games_remaining FROM charity_tracker WHERE discord_id = ?",
                (discord_id,),
            )
            row = cursor.fetchone()
            return row["reduced_rate_games_remaining"] if row else 0
