"""Service layer for pairwise player statistics."""

from repositories.interfaces import IPairingsRepository
from utils.guild import normalize_guild_id


class PairingsService:
    """Expose command-oriented pairing operations over the repository."""

    def __init__(self, pairings_repo: IPairingsRepository):
        self.pairings_repo = pairings_repo

    def get_head_to_head(
        self,
        player1_id: int,
        player2_id: int,
        guild_id: int | None = None,
    ) -> dict | None:
        return self.pairings_repo.get_head_to_head(
            player1_id,
            player2_id,
            normalize_guild_id(guild_id),
        )

    def rebuild_all_pairings(self, guild_id: int | None = None) -> int:
        return self.pairings_repo.rebuild_all_pairings(normalize_guild_id(guild_id))

    def get_player_pairing_summary(
        self,
        discord_id: int,
        guild_id: int | None = None,
        *,
        min_games: int = 3,
        limit: int = 5,
    ) -> dict:
        """Build every profile pairing category from one repository query."""
        pairings = self.pairings_repo.get_pairings_for_player(
            discord_id,
            normalize_guild_id(guild_id),
        )
        teammates = []
        opponents = []

        for pairing in pairings:
            other_id = (
                pairing["player2_id"]
                if pairing["player1_id"] == discord_id
                else pairing["player1_id"]
            )

            games_together = pairing["games_together"]
            if games_together >= min_games:
                wins_together = pairing["wins_together"]
                teammates.append(
                    {
                        "teammate_id": other_id,
                        "games_together": games_together,
                        "wins_together": wins_together,
                        "win_rate": wins_together / games_together,
                    }
                )

            games_against = pairing["games_against"]
            if games_against >= min_games:
                wins_against = pairing["player1_wins_against"]
                if pairing["player1_id"] != discord_id:
                    wins_against = games_against - wins_against
                opponents.append(
                    {
                        "opponent_id": other_id,
                        "games_against": games_against,
                        "wins_against": wins_against,
                        "win_rate": wins_against / games_against,
                    }
                )

        def teammate_rate_desc(row):
            return -row["win_rate"], -row["games_together"], row["teammate_id"]

        def teammate_rate_asc(row):
            return row["win_rate"], -row["games_together"], row["teammate_id"]

        def opponent_rate_desc(row):
            return -row["win_rate"], -row["games_against"], row["opponent_id"]

        def opponent_rate_asc(row):
            return row["win_rate"], -row["games_against"], row["opponent_id"]

        return {
            "best_teammates": sorted(
                (row for row in teammates if row["win_rate"] > 0.5),
                key=teammate_rate_desc,
            )[:limit],
            "worst_teammates": sorted(
                (row for row in teammates if row["win_rate"] < 0.5),
                key=teammate_rate_asc,
            )[:limit],
            "best_matchups": sorted(
                (row for row in opponents if row["win_rate"] > 0.5),
                key=opponent_rate_desc,
            )[:limit],
            "worst_matchups": sorted(
                (row for row in opponents if row["win_rate"] < 0.5),
                key=opponent_rate_asc,
            )[:limit],
            "most_played_with": sorted(
                teammates,
                key=lambda row: (
                    -row["games_together"],
                    -row["win_rate"],
                    row["teammate_id"],
                ),
            )[:limit],
            "most_played_against": sorted(
                opponents,
                key=lambda row: (
                    -row["games_against"],
                    -row["win_rate"],
                    row["opponent_id"],
                ),
            )[:limit],
            "even_teammates": sorted(
                (row for row in teammates if row["wins_together"] * 2 == row["games_together"]),
                key=lambda row: (-row["games_together"], row["teammate_id"]),
            )[:limit],
            "even_opponents": sorted(
                (row for row in opponents if row["wins_against"] * 2 == row["games_against"]),
                key=lambda row: (-row["games_against"], row["opponent_id"]),
            )[:limit],
            "counts": {
                "unique_teammates": len(teammates),
                "unique_opponents": len(opponents),
            },
        }
