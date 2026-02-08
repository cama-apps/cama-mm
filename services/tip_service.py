"""
Service layer for tip operations.
"""

from repositories.tip_repository import TipRepository


class TipService:
    """
    Service for tip-related operations.

    Wraps TipRepository to maintain clean layered architecture.
    """

    def __init__(self, tip_repo: TipRepository):
        self.tip_repo = tip_repo

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
        return self.tip_repo.log_tip(sender_id, recipient_id, amount, fee, guild_id)

    def get_tips_by_sender(
        self, sender_id: int, guild_id: int | None = None, limit: int = 10
    ) -> list[dict]:
        """
        Get tips sent by a user.

        Args:
            sender_id: Discord ID of the sender
            guild_id: Guild ID to filter by
            limit: Maximum number of tips to return

        Returns:
            List of tip transaction dicts
        """
        return self.tip_repo.get_tips_by_sender(sender_id, guild_id, limit)

    def get_tips_by_recipient(
        self, recipient_id: int, guild_id: int | None = None, limit: int = 10
    ) -> list[dict]:
        """
        Get tips received by a user.

        Args:
            recipient_id: Discord ID of the recipient
            guild_id: Guild ID to filter by
            limit: Maximum number of tips to return

        Returns:
            List of tip transaction dicts
        """
        return self.tip_repo.get_tips_by_recipient(recipient_id, guild_id, limit)

    def get_total_fees_collected(self, guild_id: int | None = None) -> int:
        """
        Get total fees collected from tips.

        Args:
            guild_id: Optional guild ID to filter by

        Returns:
            Total fees collected (sent to nonprofit fund)
        """
        return self.tip_repo.get_total_fees_collected(guild_id)

    def get_top_senders(self, guild_id: int | None, limit: int = 10) -> list[dict]:
        """
        Get top tip senders ranked by total amount sent.

        Args:
            guild_id: Guild ID to filter by (None for all guilds)
            limit: Maximum number of results

        Returns:
            List of dicts with discord_id, total_amount, tip_count
        """
        return self.tip_repo.get_top_senders(guild_id, limit)

    def get_top_receivers(self, guild_id: int | None, limit: int = 10) -> list[dict]:
        """
        Get top tip receivers ranked by total amount received.

        Args:
            guild_id: Guild ID to filter by (None for all guilds)
            limit: Maximum number of results

        Returns:
            List of dicts with discord_id, total_amount, tip_count
        """
        return self.tip_repo.get_top_receivers(guild_id, limit)

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
        return self.tip_repo.get_user_tip_stats(discord_id, guild_id)

    def get_total_tip_volume(self, guild_id: int | None = None) -> dict:
        """
        Get server-wide tip statistics.

        Args:
            guild_id: Guild ID to filter by (None for all guilds)

        Returns:
            Dict with total_amount, total_fees, total_transactions
        """
        return self.tip_repo.get_total_tip_volume(guild_id)
