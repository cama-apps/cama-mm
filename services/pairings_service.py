"""
Service layer for pairwise player statistics operations.
"""

from repositories.interfaces import IPairingsRepository


class PairingsService:
    """
    Service for pairwise player statistics operations.

    Wraps IPairingsRepository to maintain clean layered architecture.
    """

    def __init__(self, pairings_repo: IPairingsRepository):
        self.pairings_repo = pairings_repo

    def get_head_to_head(self, player1_id: int, player2_id: int, guild_id: int | None = None) -> dict | None:
        """
        Get head-to-head statistics between two players.

        Args:
            player1_id: First player's Discord ID
            player2_id: Second player's Discord ID
            guild_id: Guild ID

        Returns:
            Dict with games_together, wins_together, games_against, player1_wins_against,
            or None if no history exists
        """
        normalized_guild = guild_id if guild_id is not None else 0
        return self.pairings_repo.get_head_to_head(player1_id, player2_id, normalized_guild)

    def rebuild_all_pairings(self) -> int:
        """
        Rebuild all pairwise statistics from match history.

        Returns:
            Number of pairings calculated
        """
        return self.pairings_repo.rebuild_all_pairings()

    def get_best_teammates(
        self, discord_id: int, guild_id: int | None = None, min_games: int = 3, limit: int = 5
    ) -> list[dict]:
        """Get players with highest win rate when on the same team."""
        return self.pairings_repo.get_best_teammates(discord_id, guild_id=guild_id, min_games=min_games, limit=limit)

    def get_worst_teammates(
        self, discord_id: int, guild_id: int | None = None, min_games: int = 3, limit: int = 5
    ) -> list[dict]:
        """Get players with lowest win rate when on the same team."""
        return self.pairings_repo.get_worst_teammates(discord_id, guild_id=guild_id, min_games=min_games, limit=limit)

    def get_best_matchups(
        self, discord_id: int, guild_id: int | None = None, min_games: int = 3, limit: int = 5
    ) -> list[dict]:
        """Get opponents with highest win rate against."""
        return self.pairings_repo.get_best_matchups(discord_id, guild_id=guild_id, min_games=min_games, limit=limit)

    def get_worst_matchups(
        self, discord_id: int, guild_id: int | None = None, min_games: int = 3, limit: int = 5
    ) -> list[dict]:
        """Get opponents with lowest win rate against."""
        return self.pairings_repo.get_worst_matchups(discord_id, guild_id=guild_id, min_games=min_games, limit=limit)

    def get_most_played_with(
        self, discord_id: int, guild_id: int | None = None, min_games: int = 1, limit: int = 5
    ) -> list[dict]:
        """Get players played with most often as teammates."""
        return self.pairings_repo.get_most_played_with(discord_id, guild_id=guild_id, min_games=min_games, limit=limit)

    def get_most_played_against(
        self, discord_id: int, guild_id: int | None = None, min_games: int = 1, limit: int = 5
    ) -> list[dict]:
        """Get players played against most often."""
        return self.pairings_repo.get_most_played_against(discord_id, guild_id=guild_id, min_games=min_games, limit=limit)

    def get_evenly_matched_teammates(
        self, discord_id: int, guild_id: int | None = None, min_games: int = 5, limit: int = 5
    ) -> list[dict]:
        """Get teammates with ~50% win rate (evenly matched partnerships)."""
        return self.pairings_repo.get_evenly_matched_teammates(
            discord_id, guild_id=guild_id, min_games=min_games, limit=limit
        )

    def get_evenly_matched_opponents(
        self, discord_id: int, guild_id: int | None = None, min_games: int = 5, limit: int = 5
    ) -> list[dict]:
        """Get opponents with ~50% win rate (evenly matched rivals)."""
        return self.pairings_repo.get_evenly_matched_opponents(
            discord_id, guild_id=guild_id, min_games=min_games, limit=limit
        )

    def get_pairing_counts(
        self, discord_id: int, guild_id: int | None = None, min_games: int = 1
    ) -> dict:
        """Get summary counts of pairings for a player."""
        return self.pairings_repo.get_pairing_counts(discord_id, guild_id=guild_id, min_games=min_games)
