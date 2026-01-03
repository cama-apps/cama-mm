"""
Service for enriching match data from OpenDota API.

Note: Valve's GetMatchDetails API has been broken since May 2024 (patch 7.36).
We use OpenDota instead, which parses replay files directly.
"""

import json
import logging
from typing import Dict, List, Optional, Tuple

from opendota_integration import OpenDotaAPI
from utils.hero_lookup import get_hero_name

logger = logging.getLogger("cama_bot.services.match_enrichment")


class MatchEnrichmentService:
    """
    Enriches match records with data from OpenDota API.

    Correlates OpenDota account_id with registered players' steam_id
    to populate KDA, hero, GPM, damage, etc.
    """

    def __init__(self, match_repo, player_repo, opendota_api: Optional[OpenDotaAPI] = None):
        """
        Initialize the enrichment service.

        Args:
            match_repo: MatchRepository instance
            player_repo: PlayerRepository instance
            opendota_api: Optional OpenDotaAPI instance (creates one if not provided)
        """
        self.match_repo = match_repo
        self.player_repo = player_repo
        self.opendota_api = opendota_api or OpenDotaAPI()

    def enrich_match(
        self,
        internal_match_id: int,
        dota_match_id: int,
        source: str = "manual",
        confidence: Optional[float] = None,
    ) -> Dict:
        """
        Enrich an internal match with OpenDota API data.

        Args:
            internal_match_id: Our database match_id
            dota_match_id: The Dota 2 match ID

        Returns:
            Dict with enrichment results:
            - success: bool
            - players_enriched: int
            - players_not_found: list of account_ids not matched
            - error: str if failed
        """
        logger.info(
            f"Enriching match {internal_match_id} with Dota match {dota_match_id}"
        )

        # Fetch match details from OpenDota API
        match_data = self.opendota_api.get_match_details(dota_match_id)
        if not match_data:
            return {
                "success": False,
                "error": "Failed to fetch match from OpenDota API",
                "players_enriched": 0,
                "players_not_found": [],
            }

        # Update match-level data
        self.match_repo.update_match_enrichment(
            match_id=internal_match_id,
            valve_match_id=dota_match_id,
            duration_seconds=match_data.get("duration", 0),
            radiant_score=match_data.get("radiant_score", 0),
            dire_score=match_data.get("dire_score", 0),
            game_mode=match_data.get("game_mode", 0),
            enrichment_data=json.dumps(match_data),
            enrichment_source=source,
            enrichment_confidence=confidence,
        )

        # Get our match participants
        participants = self.match_repo.get_match_participants(internal_match_id)
        discord_to_steam = {}
        for p in participants:
            steam_id = self.player_repo.get_steam_id(p["discord_id"])
            if steam_id:
                discord_to_steam[p["discord_id"]] = steam_id

        # Build account_id -> player data mapping from OpenDota response
        opendota_players = {p["account_id"]: p for p in match_data.get("players", [])}

        players_enriched = 0
        players_not_found = []

        # Match each participant
        for participant in participants:
            discord_id = participant["discord_id"]
            steam_id = discord_to_steam.get(discord_id)

            if not steam_id:
                logger.warning(
                    f"Player {discord_id} has no steam_id, cannot enrich"
                )
                continue

            player_data = opendota_players.get(steam_id)
            if not player_data:
                logger.warning(
                    f"Steam ID {steam_id} (discord {discord_id}) not found in match"
                )
                players_not_found.append(steam_id)
                continue

            # Update participant stats (OpenDota uses same field names as Valve API)
            self.match_repo.update_participant_stats(
                match_id=internal_match_id,
                discord_id=discord_id,
                hero_id=player_data.get("hero_id", 0),
                kills=player_data.get("kills", 0),
                deaths=player_data.get("deaths", 0),
                assists=player_data.get("assists", 0),
                gpm=player_data.get("gold_per_min", 0),
                xpm=player_data.get("xp_per_min", 0),
                hero_damage=player_data.get("hero_damage", 0),
                tower_damage=player_data.get("tower_damage", 0),
                last_hits=player_data.get("last_hits", 0),
                denies=player_data.get("denies", 0),
                net_worth=player_data.get("net_worth", player_data.get("total_gold", 0)),
            )
            players_enriched += 1

        logger.info(
            f"Enrichment complete: {players_enriched} players enriched, "
            f"{len(players_not_found)} not found"
        )

        return {
            "success": True,
            "players_enriched": players_enriched,
            "players_not_found": players_not_found,
            "duration": match_data.get("duration", 0),
            "radiant_win": match_data.get("radiant_win", False),
            "radiant_score": match_data.get("radiant_score", 0),
            "dire_score": match_data.get("dire_score", 0),
        }

    def backfill_steam_ids(self) -> Dict:
        """
        Backfill steam_id from dotabuff_url for all players missing it.

        Returns:
            Dict with:
            - players_updated: int
            - players_failed: list of discord_ids that failed
        """
        players = self.player_repo.get_all_with_dotabuff_no_steam_id()
        updated = 0
        failed = []

        for player in players:
            discord_id = player["discord_id"]
            dotabuff_url = player["dotabuff_url"]

            steam_id = self.opendota_api.extract_player_id_from_dotabuff(dotabuff_url)
            if steam_id:
                self.player_repo.set_steam_id(discord_id, steam_id)
                updated += 1
                logger.info(f"Backfilled steam_id {steam_id} for discord {discord_id}")
            else:
                failed.append(discord_id)
                logger.warning(
                    f"Could not extract steam_id from {dotabuff_url} for discord {discord_id}"
                )

        return {"players_updated": updated, "players_failed": failed}

    def format_match_summary(self, internal_match_id: int) -> Optional[str]:
        """
        Format a human-readable summary of an enriched match.

        Args:
            internal_match_id: Our database match_id

        Returns:
            Formatted string or None if match not found/not enriched
        """
        match = self.match_repo.get_match(internal_match_id)
        if not match:
            return None

        participants = self.match_repo.get_match_participants(internal_match_id)
        if not participants or not any(p.get("hero_id") for p in participants):
            return None  # Not enriched

        # Sort by team
        radiant = [p for p in participants if p.get("side") == "radiant"]
        dire = [p for p in participants if p.get("side") == "dire"]

        def format_player(p: dict) -> str:
            hero = get_hero_name(p.get("hero_id", 0))
            kda = f"{p.get('kills', 0)}/{p.get('deaths', 0)}/{p.get('assists', 0)}"
            return f"{hero} ({kda})"

        lines = []
        winner = "Radiant" if match.get("winning_team") == 1 else "Dire"
        lines.append(f"**{winner} Victory**")

        # Get duration if available - need to fetch from DB
        # For now, skip duration in summary

        lines.append("**Radiant:**")
        for p in radiant:
            lines.append(f"  {format_player(p)}")

        lines.append("**Dire:**")
        for p in dire:
            lines.append(f"  {format_player(p)}")

        return "\n".join(lines)
