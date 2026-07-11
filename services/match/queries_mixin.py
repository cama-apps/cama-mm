"""QueriesMixin mixin for :class:`MatchService`.

Read-only query facade plus the participant-stat writer and the aggregated
scout-data builder. These methods provide query access without exposing the
repository directly.

Mixin split out of the former monolithic ``match_service`` module; it carries
no state of its own and is composed into ``MatchService``.
"""

from utils.guild import normalize_guild_id


class QueriesMixin:
    """QueriesMixin — see module docstring.

    Composed into :class:`~services.match_service.MatchService`; relies on the
    attributes and helpers that the other mixins and the constructor provide.
    """

    # ==================== Query Methods ====================
    # These methods provide query access without exposing the repository directly

    def get_match_by_id(self, match_id: int, guild_id: int | None = None) -> dict | None:
        """
        Get a match by its internal ID.

        Args:
            match_id: Internal match ID
            guild_id: Guild ID (optional, for validation)

        Returns:
            Match dict or None if not found
        """
        return self.match_repo.get_match(match_id, guild_id)

    def get_enrichment_data(self, match_id: int, guild_id: int | None = None) -> dict | None:
        """Get parsed enrichment_data JSON for a match."""
        return self.match_repo.get_enrichment_data(match_id, guild_id)

    def get_most_recent_match(self, guild_id: int | None = None) -> dict | None:
        """
        Get the most recently recorded match for a guild.

        Args:
            guild_id: Guild ID to filter by

        Returns:
            Match dict or None if no matches found
        """
        return self.match_repo.get_most_recent_match(guild_id)

    def get_match_participants(self, match_id: int, guild_id: int | None = None) -> list[dict]:
        """
        Get all participants for a match with their stats.

        Args:
            match_id: Internal match ID
            guild_id: Guild ID for multi-guild isolation (optional)

        Returns:
            List of participant dicts with hero, KDA, etc.
        """
        return self.match_repo.get_match_participants(match_id, guild_id)

    def get_player_matches(
        self, discord_id: int, guild_id: int | None, limit: int = 10
    ) -> list[dict]:
        """
        Get a player's recent matches.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID
            limit: Maximum number of matches to return

        Returns:
            List of match dicts, most recent first
        """
        return self.match_repo.get_player_matches(discord_id, guild_id, limit=limit)

    def get_rating_history_for_match(self, match_id: int) -> list[dict]:
        """
        Get rating history entries for a specific match.

        Args:
            match_id: Internal match ID

        Returns:
            List of rating history entries for participants
        """
        return self.match_repo.get_rating_history_for_match(match_id)

    def get_matches_without_fantasy_data(
        self, guild_id: int | None, limit: int = 100
    ) -> list[dict]:
        """
        Get matches that have enrichment but no fantasy data.

        Used for fantasy data backfill operations.

        Args:
            guild_id: Guild ID for multi-server isolation
            limit: Maximum number of matches to return

        Returns:
            List of match dicts needing fantasy data
        """
        return self.match_repo.get_matches_without_fantasy_data(
            normalize_guild_id(guild_id), limit=limit
        )

    def get_enriched_count(self, guild_id: int | None = None) -> int:
        """
        Get count of enriched matches for a guild.

        Args:
            guild_id: Guild ID to filter by

        Returns:
            Number of enriched matches
        """
        return self.match_repo.get_enriched_count(guild_id)

    def wipe_all_enrichments(self, guild_id: int | None = None) -> int:
        """
        Clear all match enrichments for a guild.

        Args:
            guild_id: Guild ID to filter by

        Returns:
            Number of matches wiped
        """
        return self.match_repo.wipe_all_enrichments(guild_id)

    def wipe_match_enrichment(self, match_id: int, guild_id: int | None = None) -> bool:
        """
        Clear enrichment data for a specific match.

        Args:
            match_id: Internal match ID
            guild_id: Guild ID (optional)

        Returns:
            True if match was found and wiped, False otherwise
        """
        return self.match_repo.wipe_match_enrichment(match_id, guild_id)

    def get_player_openskill_history(
        self, discord_id: int, guild_id: int, limit: int = 10
    ) -> list[dict]:
        """
        Get a player's OpenSkill rating history.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID
            limit: Maximum number of entries to return

        Returns:
            List of OpenSkill history entries, most recent first
        """
        return self.match_repo.get_player_openskill_history(discord_id, guild_id, limit=limit)

    # --- Statistics and calibration facade methods ---

    def get_match_count(self, guild_id: int | None = None) -> int:
        """Get total number of matches recorded for a guild."""
        return self.match_repo.get_match_count(guild_id)

    def get_recent_match_predictions(self, guild_id: int | None, limit: int = 200) -> list[dict]:
        """Get recent match predictions for calibration analysis."""
        return self.match_repo.get_recent_match_predictions(guild_id, limit)

    def get_recent_rating_history(self, guild_id: int | None, limit: int = 500) -> list[dict]:
        """Get recent rating history entries for calibration analysis."""
        return self.match_repo.get_recent_rating_history(guild_id, limit)

    def get_biggest_upsets(self, guild_id: int | None, limit: int = 5) -> list[dict]:
        """Get biggest upset matches (underdogs who won against the odds)."""
        return self.match_repo.get_biggest_upsets(guild_id, limit)

    def get_player_performance_stats(self, guild_id: int | None) -> list[dict]:
        """Get player performance vs expected stats."""
        return self.match_repo.get_player_performance_stats(guild_id)

    def get_lobby_type_stats(self, guild_id: int | None) -> list[dict]:
        """Get rating swing statistics by lobby type (shuffle vs draft)."""
        return self.match_repo.get_lobby_type_stats(guild_id)

    def get_player_rating_history_detailed(
        self, discord_id: int, guild_id: int | None, limit: int = 50
    ) -> list[dict]:
        """Get detailed rating history for a player including predictions."""
        return self.match_repo.get_player_rating_history_detailed(discord_id, guild_id, limit)

    def get_os_ratings_for_match(self, match_id: int, guild_id: int | None = None) -> dict:
        """Get OpenSkill ratings for teams in a match."""
        return self.match_repo.get_os_ratings_for_match(match_id, guild_id)

    def get_player_lobby_type_stats(self, discord_id: int, guild_id: int | None) -> list[dict]:
        """Get lobby type statistics for a specific player."""
        return self.match_repo.get_player_lobby_type_stats(discord_id, guild_id)

    def get_player_hero_stats_detailed(
        self, discord_id: int, guild_id: int | None, limit: int = 8
    ) -> list[dict]:
        """Get detailed hero performance stats for a player."""
        return self.match_repo.get_player_hero_stats_detailed(discord_id, guild_id, limit)

    def get_player_hero_role_breakdown(self, discord_id: int, guild_id: int | None) -> list[dict]:
        """Get hero role breakdown (core vs support) for a player."""
        return self.match_repo.get_player_hero_role_breakdown(discord_id, guild_id)

    def get_player_fantasy_stats(self, discord_id: int, guild_id: int | None) -> dict | None:
        """Get fantasy points statistics for a player."""
        return self.match_repo.get_player_fantasy_stats(discord_id, guild_id)

    def update_participant_stats(
        self,
        match_id: int,
        discord_id: int,
        hero_id: int,
        kills: int,
        deaths: int,
        assists: int,
        gpm: int,
        xpm: int,
        hero_damage: int,
        tower_damage: int,
        last_hits: int,
        denies: int,
        net_worth: int,
        hero_healing: int = 0,
        lane_role: int | None = None,
        lane_efficiency: int | None = None,
        towers_killed: int | None = None,
        roshans_killed: int | None = None,
        teamfight_participation: float | None = None,
        obs_placed: int | None = None,
        sen_placed: int | None = None,
        camps_stacked: int | None = None,
        rune_pickups: int | None = None,
        firstblood_claimed: int | None = None,
        stuns: float | None = None,
        fantasy_points: float | None = None,
    ) -> bool:
        """
        Update stats for a match participant.

        Args:
            match_id: Internal match ID
            discord_id: Player's Discord ID
            hero_id: Hero ID played
            kills, deaths, assists: KDA stats
            gpm, xpm: Gold/XP per minute
            hero_damage, tower_damage: Damage dealt
            last_hits, denies: Farming stats
            net_worth: Final net worth
            hero_healing: Healing done
            lane_role, lane_efficiency: Laning phase stats
            towers_killed, roshans_killed: Objectives
            teamfight_participation: Teamfight participation rate
            obs_placed, sen_placed: Vision stats
            camps_stacked, rune_pickups: Utility stats
            firstblood_claimed: First blood participation
            stuns: Total stun time dealt
            fantasy_points: Calculated fantasy points

        Returns:
            True if participant was updated, False otherwise
        """
        return self.match_repo.update_participant_stats(
            match_id=match_id,
            discord_id=discord_id,
            hero_id=hero_id,
            kills=kills,
            deaths=deaths,
            assists=assists,
            gpm=gpm,
            xpm=xpm,
            hero_damage=hero_damage,
            tower_damage=tower_damage,
            last_hits=last_hits,
            denies=denies,
            net_worth=net_worth,
            hero_healing=hero_healing,
            lane_role=lane_role,
            lane_efficiency=lane_efficiency,
            towers_killed=towers_killed,
            roshans_killed=roshans_killed,
            teamfight_participation=teamfight_participation,
            obs_placed=obs_placed,
            sen_placed=sen_placed,
            camps_stacked=camps_stacked,
            rune_pickups=rune_pickups,
            firstblood_claimed=firstblood_claimed,
            stuns=stuns,
            fantasy_points=fantasy_points,
        )

    def get_players_with_enriched_data(self, guild_id: int | None) -> list[dict]:
        """
        Get players who have enriched match data.

        Args:
            guild_id: Guild ID to filter by

        Returns:
            List of player dicts with discord_id and enriched match count
        """
        return self.match_repo.get_players_with_enriched_data(guild_id)

    def get_last_match_participant_ids(self, guild_id: int | None) -> list[int]:
        """
        Get participant IDs from the most recent recorded match.

        Args:
            guild_id: Guild ID for multi-server isolation

        Returns:
            List of Discord IDs from the last match, or empty list
        """
        return list(self.match_repo.get_last_match_participant_ids(guild_id))

    def get_multi_player_hero_stats(self, player_ids: list[int], guild_id: int | None) -> list[dict]:
        """
        Get hero statistics for multiple players for hero grid.

        Args:
            player_ids: List of Discord IDs
            guild_id: Guild ID for multi-server isolation

        Returns:
            List of dicts with player hero stats for grid visualization
        """
        return self.match_repo.get_multi_player_hero_stats(player_ids, guild_id)

    def get_scout_data(
        self, player_ids: list[int], guild_id: int | None, limit: int = 10
    ) -> dict:
        """
        Get aggregated hero scouting data for multiple players.

        Aggregates hero stats across all specified players, showing their combined
        hero pool with games, wins, losses, bans, and primary role.

        Args:
            player_ids: List of Discord IDs to scout
            guild_id: Guild ID for multi-server isolation
            limit: Maximum number of heroes to return (default 10)

        Returns:
            Dict with:
                - player_count: Number of players included
                - heroes: List of top heroes by games, each containing:
                    - hero_id, games, wins, losses, bans, primary_role
        """
        if not player_ids:
            return {"player_count": 0, "heroes": []}

        # Get per-player hero stats
        player_stats = self.match_repo.get_player_hero_stats_for_scout(player_ids, guild_id)

        # Get deduplicated ban counts (opposing team only)
        ban_data = self.match_repo.get_bans_for_players(player_ids, guild_id)

        # Get total unique match count for contest rate calculation
        total_matches = self.match_repo.get_match_count_for_players(player_ids, guild_id)

        # Aggregate hero stats across all players
        aggregated: dict[int, dict] = {}  # hero_id -> {games, wins, losses, roles}

        for heroes in player_stats.values():
            for hero in heroes:
                hero_id = hero["hero_id"]
                if hero_id not in aggregated:
                    aggregated[hero_id] = {
                        "games": 0,
                        "wins": 0,
                        "losses": 0,
                        "roles": [],
                    }
                aggregated[hero_id]["games"] += hero["games"]
                aggregated[hero_id]["wins"] += hero["wins"]
                aggregated[hero_id]["losses"] += hero["losses"]
                if hero.get("primary_role"):
                    aggregated[hero_id]["roles"].append(hero["primary_role"])

        # Sort by total relevance (games + bans), take top N
        sorted_heroes = sorted(
            aggregated.items(),
            key=lambda x: -(x[1]["games"] + ban_data.get(x[0], 0)),
        )[:limit]

        # Build result with primary_role (mode) and ban counts
        # Default to Carry (1) when no role data is available
        DEFAULT_ROLE = 1
        result_heroes = []
        for hero_id, stats in sorted_heroes:
            roles = stats["roles"]
            if roles:
                primary_role = max(set(roles), key=roles.count)
            else:
                primary_role = DEFAULT_ROLE

            result_heroes.append({
                "hero_id": hero_id,
                "games": stats["games"],
                "wins": stats["wins"],
                "losses": stats["losses"],
                "bans": ban_data.get(hero_id, 0),
                "primary_role": primary_role,
            })

        return {
            "player_count": len(player_ids),
            "total_matches": total_matches,
            "heroes": result_heroes,
        }
