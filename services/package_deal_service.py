"""
Service layer for package deal operations.
"""

from repositories.package_deal_repository import PackageDeal, PackageDealRepository


class PackageDealService:
    """
    Service for package deal feature operations.

    Wraps PackageDealRepository to maintain clean layered architecture.
    """

    def __init__(self, package_deal_repo: PackageDealRepository):
        self.package_deal_repo = package_deal_repo

    def create_or_extend_deal(
        self,
        guild_id: int | None,
        buyer_id: int,
        partner_id: int,
        games: int = 10,
        cost: int = 0,
    ) -> PackageDeal:
        """
        Create a new package deal or extend existing one.

        If a deal already exists for this pair, adds games to games_remaining.

        Args:
            guild_id: Guild ID
            buyer_id: Discord ID of the player buying the deal
            partner_id: Discord ID of the player to be teamed with
            games: Number of games for the deal
            cost: Cost paid for this deal (for tracking)

        Returns:
            The created/updated PackageDeal

        Raises:
            ValueError: If buyer_id equals partner_id
        """
        return self.package_deal_repo.create_or_extend_deal(
            guild_id=guild_id,
            buyer_id=buyer_id,
            partner_id=partner_id,
            games=games,
            cost=cost,
        )

    def get_active_deals_for_players(
        self,
        guild_id: int | None,
        player_ids: list[int],
    ) -> list[PackageDeal]:
        """
        Get all active deals where BOTH buyer and partner are in player_ids.

        This is used during shuffle to find deals relevant to the current match.
        Only returns deals with games_remaining > 0.

        Args:
            guild_id: Guild ID
            player_ids: List of player Discord IDs in the match

        Returns:
            List of active PackageDeal objects
        """
        return self.package_deal_repo.get_active_deals_for_players(guild_id, player_ids)

    def get_user_deals(
        self,
        guild_id: int | None,
        discord_id: int,
    ) -> list[PackageDeal]:
        """
        Get all active deals created by a user.

        Used for /mydeals command.

        Args:
            guild_id: Guild ID
            discord_id: User's Discord ID

        Returns:
            List of active PackageDeal objects created by the user
        """
        return self.package_deal_repo.get_user_deals(guild_id, discord_id)

    def decrement_deals(
        self,
        guild_id: int | None,
        deal_ids: list[int],
    ) -> int:
        """
        Decrement games_remaining for the given deal IDs.

        Deals that reach 0 are kept but will be filtered out in future queries.

        Args:
            guild_id: Guild ID
            deal_ids: List of deal IDs to decrement

        Returns:
            The number of deals decremented
        """
        return self.package_deal_repo.decrement_deals(guild_id, deal_ids)

    def delete_expired_deals(self, guild_id: int | None) -> int:
        """
        Delete deals with games_remaining = 0.

        This is optional cleanup - expired deals are ignored anyway.

        Args:
            guild_id: Guild ID

        Returns:
            The number of deals deleted
        """
        return self.package_deal_repo.delete_expired_deals(guild_id)
