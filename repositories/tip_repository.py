"""
Repository for tip transaction logging.
"""

import time

from repositories.base_repository import BaseRepository


class TipRepository(BaseRepository):
    """
    Repository for logging tip transactions.
    """

    def log_tip(
        self,
        sender_id: int,
        recipient_id: int,
        amount: int,
        fee: int,
        guild_id: int | None,
    ) -> int:
        """
        Log a tip transaction.

        Args:
            sender_id: Discord ID of the sender
            recipient_id: Discord ID of the recipient
            amount: Amount of jopacoin transferred (before fee)
            fee: Fee amount sent to nonprofit fund
            guild_id: Guild ID where tip occurred

        Returns:
            The transaction ID
        """
        normalized_guild_id = self.normalize_guild_id(guild_id)
        timestamp = int(time.time())

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO tip_transactions
                    (sender_id, recipient_id, amount, fee, guild_id, timestamp)
                VALUES (?, ?, ?, ?, ?, ?)
                """,
                (sender_id, recipient_id, amount, fee, normalized_guild_id, timestamp),
            )
            return cursor.lastrowid

    def get_tips_by_sender(self, sender_id: int, limit: int = 10) -> list[dict]:
        """
        Get tips sent by a user.

        Args:
            sender_id: Discord ID of the sender
            limit: Maximum number of tips to return

        Returns:
            List of tip transaction dicts
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT id, sender_id, recipient_id, amount, fee, guild_id, timestamp
                FROM tip_transactions
                WHERE sender_id = ?
                ORDER BY timestamp DESC
                LIMIT ?
                """,
                (sender_id, limit),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_tips_by_recipient(self, recipient_id: int, limit: int = 10) -> list[dict]:
        """
        Get tips received by a user.

        Args:
            recipient_id: Discord ID of the recipient
            limit: Maximum number of tips to return

        Returns:
            List of tip transaction dicts
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT id, sender_id, recipient_id, amount, fee, guild_id, timestamp
                FROM tip_transactions
                WHERE recipient_id = ?
                ORDER BY timestamp DESC
                LIMIT ?
                """,
                (recipient_id, limit),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_total_fees_collected(self, guild_id: int | None = None) -> int:
        """
        Get total fees collected from tips.

        Args:
            guild_id: Optional guild ID to filter by

        Returns:
            Total fees collected (sent to nonprofit fund)
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            if guild_id is not None:
                normalized_guild_id = self.normalize_guild_id(guild_id)
                cursor.execute(
                    "SELECT COALESCE(SUM(fee), 0) FROM tip_transactions WHERE guild_id = ?",
                    (normalized_guild_id,),
                )
            else:
                cursor.execute("SELECT COALESCE(SUM(fee), 0) FROM tip_transactions")
            return cursor.fetchone()[0]
