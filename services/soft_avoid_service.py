"""
Service layer for soft avoid operations.
"""

from repositories.soft_avoid_repository import SoftAvoid, SoftAvoidRepository


class SoftAvoidService:
    """
    Service for soft avoid feature operations.

    Wraps SoftAvoidRepository to maintain clean layered architecture.
    """

    def __init__(self, soft_avoid_repo: SoftAvoidRepository):
        self.soft_avoid_repo = soft_avoid_repo

    def create_or_extend_avoid(
        self,
        guild_id: int | None,
        avoider_id: int,
        avoided_id: int,
        games: int = 10,
    ) -> SoftAvoid:
        """
        Create a new soft avoid or extend existing one.

        If an avoid already exists for this pair, adds games to games_remaining.

        Args:
            guild_id: Guild ID
            avoider_id: Discord ID of the player creating the avoid
            avoided_id: Discord ID of the player being avoided
            games: Number of games for the avoid

        Returns:
            The created/updated SoftAvoid

        Raises:
            ValueError: If avoider_id equals avoided_id
        """
        return self.soft_avoid_repo.create_or_extend_avoid(
            guild_id=guild_id,
            avoider_id=avoider_id,
            avoided_id=avoided_id,
            games=games,
        )

    def get_active_avoids_for_players(
        self,
        guild_id: int | None,
        player_ids: list[int],
    ) -> list[SoftAvoid]:
        """
        Get all active avoids where BOTH avoider and avoided are in player_ids.

        This is used during shuffle to find avoids relevant to the current match.
        Only returns avoids with games_remaining > 0.

        Args:
            guild_id: Guild ID
            player_ids: List of player Discord IDs in the match

        Returns:
            List of active SoftAvoid objects
        """
        return self.soft_avoid_repo.get_active_avoids_for_players(guild_id, player_ids)

    def get_user_avoids(
        self,
        guild_id: int | None,
        discord_id: int,
    ) -> list[SoftAvoid]:
        """
        Get all active avoids created by a user.

        Used for /myavoids command.

        Args:
            guild_id: Guild ID
            discord_id: User's Discord ID

        Returns:
            List of active SoftAvoid objects created by the user
        """
        return self.soft_avoid_repo.get_user_avoids(guild_id, discord_id)

    def decrement_avoids(
        self,
        guild_id: int | None,
        avoid_ids: list[int],
    ) -> int:
        """
        Decrement games_remaining for the given avoid IDs.

        Avoids that reach 0 are kept but will be filtered out in future queries.

        Args:
            guild_id: Guild ID
            avoid_ids: List of avoid IDs to decrement

        Returns:
            The number of avoids decremented
        """
        return self.soft_avoid_repo.decrement_avoids(guild_id, avoid_ids)

    def delete_expired_avoids(self, guild_id: int | None) -> int:
        """
        Delete avoids with games_remaining = 0.

        This is optional cleanup - expired avoids are ignored anyway.

        Args:
            guild_id: Guild ID

        Returns:
            The number of avoids deleted
        """
        return self.soft_avoid_repo.delete_expired_avoids(guild_id)
