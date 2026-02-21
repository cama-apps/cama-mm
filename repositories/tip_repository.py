"""
Repository for tip transaction logging.
"""

import time

from repositories.base_repository import BaseRepository
from repositories.interfaces import ITipRepository


class TipRepository(BaseRepository, ITipRepository):
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

    def get_tips_by_sender(self, sender_id: int, guild_id: int | None = None, limit: int = 10) -> list[dict]:
        """
        Get tips sent by a user.

        Args:
            sender_id: Discord ID of the sender
            guild_id: Guild ID to filter by
            limit: Maximum number of tips to return

        Returns:
            List of tip transaction dicts
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT id, sender_id, recipient_id, amount, fee, guild_id, timestamp
                FROM tip_transactions
                WHERE sender_id = ? AND guild_id = ?
                ORDER BY timestamp DESC
                LIMIT ?
                """,
                (sender_id, normalized_guild, limit),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_tips_by_recipient(self, recipient_id: int, guild_id: int | None = None, limit: int = 10) -> list[dict]:
        """
        Get tips received by a user.

        Args:
            recipient_id: Discord ID of the recipient
            guild_id: Guild ID to filter by
            limit: Maximum number of tips to return

        Returns:
            List of tip transaction dicts
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT id, sender_id, recipient_id, amount, fee, guild_id, timestamp
                FROM tip_transactions
                WHERE recipient_id = ? AND guild_id = ?
                ORDER BY timestamp DESC
                LIMIT ?
                """,
                (recipient_id, normalized_guild, limit),
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

    def get_top_senders(self, guild_id: int | None, limit: int = 10) -> list[dict]:
        """
        Get top tip senders ranked by total amount sent.

        Args:
            guild_id: Guild ID to filter by (None for all guilds)
            limit: Maximum number of results

        Returns:
            List of dicts with discord_id, total_amount, tip_count
        """
        normalized_guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT sender_id, SUM(amount) as total_amount, COUNT(*) as tip_count
                FROM tip_transactions
                WHERE guild_id = ?
                GROUP BY sender_id
                ORDER BY total_amount DESC
                LIMIT ?
                """,
                (normalized_guild_id, limit),
            )
            return [
                {
                    "discord_id": row["sender_id"],
                    "total_amount": row["total_amount"],
                    "tip_count": row["tip_count"],
                }
                for row in cursor.fetchall()
            ]

    def get_top_receivers(self, guild_id: int | None, limit: int = 10) -> list[dict]:
        """
        Get top tip receivers ranked by total amount received.

        Args:
            guild_id: Guild ID to filter by (None for all guilds)
            limit: Maximum number of results

        Returns:
            List of dicts with discord_id, total_amount, tip_count
        """
        normalized_guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT recipient_id, SUM(amount) as total_amount, COUNT(*) as tip_count
                FROM tip_transactions
                WHERE guild_id = ?
                GROUP BY recipient_id
                ORDER BY total_amount DESC
                LIMIT ?
                """,
                (normalized_guild_id, limit),
            )
            return [
                {
                    "discord_id": row["recipient_id"],
                    "total_amount": row["total_amount"],
                    "tip_count": row["tip_count"],
                }
                for row in cursor.fetchall()
            ]

    def get_user_tip_stats(self, discord_id: int, guild_id: int | None) -> dict:
        """
        Get individual user's tip statistics.

        Args:
            discord_id: User's Discord ID
            guild_id: Guild ID to filter by (None for all guilds)

        Returns:
            Dict with total_sent, tips_sent_count, fees_paid,
            total_received, tips_received_count
        """
        normalized_guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()

            # Get sent stats
            cursor.execute(
                """
                SELECT COALESCE(SUM(amount), 0) as total_sent,
                       COUNT(*) as tips_sent_count,
                       COALESCE(SUM(fee), 0) as fees_paid
                FROM tip_transactions
                WHERE sender_id = ? AND guild_id = ?
                """,
                (discord_id, normalized_guild_id),
            )
            sent_row = cursor.fetchone()

            # Get received stats
            cursor.execute(
                """
                SELECT COALESCE(SUM(amount), 0) as total_received,
                       COUNT(*) as tips_received_count
                FROM tip_transactions
                WHERE recipient_id = ? AND guild_id = ?
                """,
                (discord_id, normalized_guild_id),
            )
            received_row = cursor.fetchone()

            return {
                "total_sent": sent_row["total_sent"],
                "tips_sent_count": sent_row["tips_sent_count"],
                "fees_paid": sent_row["fees_paid"],
                "total_received": received_row["total_received"],
                "tips_received_count": received_row["tips_received_count"],
            }

    def get_total_tip_volume(self, guild_id: int | None) -> dict:
        """
        Get server-wide tip statistics.

        Args:
            guild_id: Guild ID to filter by (None for all guilds)

        Returns:
            Dict with total_amount, total_fees, total_transactions
        """
        normalized_guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT COALESCE(SUM(amount), 0) as total_amount,
                       COALESCE(SUM(fee), 0) as total_fees,
                       COUNT(*) as total_transactions
                FROM tip_transactions
                WHERE guild_id = ?
                """,
                (normalized_guild_id,),
            )
            row = cursor.fetchone()
            return {
                "total_amount": row["total_amount"],
                "total_fees": row["total_fees"],
                "total_transactions": row["total_transactions"],
            }
