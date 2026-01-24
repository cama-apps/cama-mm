"""
Service for enriching match data from OpenDota API.

Note: Valve's GetMatchDetails API has been broken since May 2024 (patch 7.36).
We use OpenDota instead, which parses replay files directly.
"""

import json
import logging

from config import ENRICHMENT_MIN_PLAYER_MATCH
from opendota_integration import OpenDotaAPI
from utils.hero_lookup import get_hero_name

logger = logging.getLogger("cama_bot.services.match_enrichment")


def calculate_fantasy_points(player_data: dict) -> float:
    """
    Calculate Dota 2 fantasy points from OpenDota player data.

    Fantasy scoring formula based on official DPC fantasy:
    - Kills: +0.3 per kill
    - Deaths: -0.3 per death
    - Assists: +0.15 per assist
    - Last hits: +0.003 per LH
    - GPM: +0.002 per GPM
    - Tower kills: +1.0 per tower
    - Roshan kills: +1.0 per Roshan
    - Teamfight participation: +3.0 weighted (0-1 scale)
    - Wards placed: +0.5 per ward (obs + sen)
    - Camps stacked: +0.5 per stack
    - Rune pickups: +0.25 per rune
    - First blood: +4.0 if claimed
    - Stun duration: +0.05 per second

    Args:
        player_data: OpenDota player data dict from match response

    Returns:
        Calculated fantasy points (rounded to 2 decimal places)
    """
    points = 0.0

    # Core stats
    points += player_data.get("kills", 0) * 0.3
    points -= player_data.get("deaths", 0) * 0.3
    points += player_data.get("assists", 0) * 0.15
    points += player_data.get("last_hits", 0) * 0.003
    points += player_data.get("gold_per_min", 0) * 0.002

    # Objectives
    points += player_data.get("towers_killed", 0) * 1.0
    points += player_data.get("roshans_killed", 0) * 1.0

    # Teamfight (0-1 scale, weighted by 3.0)
    points += player_data.get("teamfight_participation", 0) * 3.0

    # Vision game
    points += (player_data.get("obs_placed", 0) + player_data.get("sen_placed", 0)) * 0.5

    # Economy
    points += player_data.get("camps_stacked", 0) * 0.5
    points += player_data.get("rune_pickups", 0) * 0.25

    # Early game
    if player_data.get("firstblood_claimed"):
        points += 4.0

    # Crowd control
    points += player_data.get("stuns", 0) * 0.05

    return round(points, 2)


class MatchEnrichmentService:
    """
    Enriches match records with data from OpenDota API.

    Correlates OpenDota account_id with registered players' steam_id
    to populate KDA, hero, GPM, damage, fantasy points, etc.
    """

    def __init__(
        self,
        match_repo,
        player_repo,
        opendota_api: OpenDotaAPI | None = None,
        match_service=None,
    ):
        """
        Initialize the enrichment service.

        Args:
            match_repo: MatchRepository instance
            player_repo: PlayerRepository instance
            opendota_api: Optional OpenDotaAPI instance (creates one if not provided)
            match_service: Optional MatchService for OpenSkill updates after enrichment
        """
        self.match_repo = match_repo
        self.player_repo = player_repo
        self.opendota_api = opendota_api or OpenDotaAPI()
        self.match_service = match_service

    def _validate_enrichment(
        self,
        internal_match: dict,
        opendota_match: dict,
        participants: list[dict],
        steam_ids: list[int],
    ) -> tuple[bool, str]:
        """
        Validate that OpenDota match data matches our internal match.

        Strict validation rules:
        1. All 10 players must be found in the OpenDota match
        2. Winning team must match (radiant_win vs winning_team)
        3. Player sides must match (radiant/dire assignment)

        Args:
            internal_match: Our internal match data
            opendota_match: OpenDota API response
            participants: List of our match participants
            steam_ids: List of steam_ids for participants (parallel to participants)

        Returns:
            Tuple of (is_valid, error_message)
        """
        # Build OpenDota player lookup by account_id
        od_players = {p.get("account_id"): p for p in opendota_match.get("players", [])}

        # Rule 1: All players must match
        matched_count = sum(1 for sid in steam_ids if sid and sid in od_players)
        min_required = ENRICHMENT_MIN_PLAYER_MATCH

        if matched_count < min_required:
            return False, f"Only {matched_count}/{len(steam_ids)} players matched (need {min_required})"

        # Rule 2: Winning team must match
        internal_radiant_won = internal_match.get("winning_team") == 1
        od_radiant_won = opendota_match.get("radiant_win", False)

        if internal_radiant_won != od_radiant_won:
            return False, f"Winning team mismatch: internal={'Radiant' if internal_radiant_won else 'Dire'}, OpenDota={'Radiant' if od_radiant_won else 'Dire'}"

        # Rule 3: Player sides must match
        for i, participant in enumerate(participants):
            steam_id = steam_ids[i] if i < len(steam_ids) else None
            if not steam_id:
                continue

            od_player = od_players.get(steam_id)
            if not od_player:
                continue

            internal_is_radiant = participant.get("side") == "radiant"
            # OpenDota uses player_slot: 0-4 = Radiant, 128-132 = Dire
            od_player_slot = od_player.get("player_slot", 0)
            od_is_radiant = od_player_slot < 128

            if internal_is_radiant != od_is_radiant:
                discord_id = participant.get("discord_id", "unknown")
                return False, f"Player {discord_id} on wrong team: internal={'Radiant' if internal_is_radiant else 'Dire'}, OpenDota={'Radiant' if od_is_radiant else 'Dire'}"

        return True, "Valid"

    def enrich_match(
        self,
        internal_match_id: int,
        dota_match_id: int,
        source: str = "manual",
        confidence: float | None = None,
        skip_validation: bool = False,
    ) -> dict:
        """
        Enrich an internal match with OpenDota API data including fantasy points.

        Args:
            internal_match_id: Our database match_id
            dota_match_id: The Dota 2 match ID
            source: 'manual' or 'auto' to indicate enrichment source
            confidence: Optional confidence score for auto-discovered matches
            skip_validation: If True, skip strict validation (for manual overrides)

        Returns:
            Dict with enrichment results:
            - success: bool
            - players_enriched: int
            - players_not_found: list of account_ids not matched
            - fantasy_points_calculated: bool
            - total_fantasy_points: dict with radiant/dire totals
            - validation_error: str if validation failed
            - error: str if failed
        """
        logger.info(f"Enriching match {internal_match_id} with Dota match {dota_match_id}")

        # Get internal match data
        internal_match = self.match_repo.get_match(internal_match_id)
        if not internal_match:
            return {
                "success": False,
                "error": f"Internal match {internal_match_id} not found",
                "players_enriched": 0,
                "players_not_found": [],
            }

        # Fetch match details from OpenDota API
        match_data = self.opendota_api.get_match_details(dota_match_id)
        if not match_data:
            return {
                "success": False,
                "error": "Failed to fetch match from OpenDota API",
                "players_enriched": 0,
                "players_not_found": [],
            }

        # Get our match participants and their steam_ids (bulk lookup)
        participants = self.match_repo.get_match_participants(internal_match_id)
        discord_ids = [p["discord_id"] for p in participants]
        discord_to_steam = self.player_repo.get_steam_ids_bulk(discord_ids)
        steam_ids = [discord_to_steam.get(p["discord_id"]) for p in participants]

        # Strict validation (unless skipped for manual enrichment)
        if not skip_validation:
            is_valid, validation_error = self._validate_enrichment(
                internal_match, match_data, participants, steam_ids
            )
            if not is_valid:
                logger.warning(f"Validation failed for match {internal_match_id}: {validation_error}")
                return {
                    "success": False,
                    "error": f"Validation failed: {validation_error}",
                    "validation_error": validation_error,
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

        # Build account_id -> player data mapping from OpenDota response
        opendota_players = {p["account_id"]: p for p in match_data.get("players", [])}

        players_enriched = 0
        players_not_found = []
        radiant_fantasy = 0.0
        dire_fantasy = 0.0
        participant_updates = []  # Collect updates for bulk operation

        # Match each participant
        for participant in participants:
            discord_id = participant["discord_id"]
            steam_id = discord_to_steam.get(discord_id)

            if not steam_id:
                logger.warning(f"Player {discord_id} has no steam_id, cannot enrich")
                continue

            player_data = opendota_players.get(steam_id)
            if not player_data:
                logger.warning(f"Steam ID {steam_id} (discord {discord_id}) not found in match")
                players_not_found.append(steam_id)
                continue

            # Calculate fantasy points
            fantasy_points = calculate_fantasy_points(player_data)

            # Track team fantasy totals
            if participant.get("side") == "radiant":
                radiant_fantasy += fantasy_points
            else:
                dire_fantasy += fantasy_points

            # Collect participant stats for bulk update
            participant_updates.append({
                "discord_id": discord_id,
                "hero_id": player_data.get("hero_id", 0),
                "kills": player_data.get("kills", 0),
                "deaths": player_data.get("deaths", 0),
                "assists": player_data.get("assists", 0),
                "gpm": player_data.get("gold_per_min", 0),
                "xpm": player_data.get("xp_per_min", 0),
                "hero_damage": player_data.get("hero_damage", 0),
                "tower_damage": player_data.get("tower_damage", 0),
                "last_hits": player_data.get("last_hits", 0),
                "denies": player_data.get("denies", 0),
                "net_worth": player_data.get("net_worth", player_data.get("total_gold", 0)),
                "hero_healing": player_data.get("hero_healing", 0),
                "lane_role": player_data.get("lane_role"),  # 1=Safe, 2=Mid, 3=Off, 4=Jungle
                "lane_efficiency": player_data.get("lane_efficiency_pct"),  # 0-100
                # Fantasy fields
                "towers_killed": player_data.get("towers_killed"),
                "roshans_killed": player_data.get("roshans_killed"),
                "teamfight_participation": player_data.get("teamfight_participation"),
                "obs_placed": player_data.get("obs_placed"),
                "sen_placed": player_data.get("sen_placed"),
                "camps_stacked": player_data.get("camps_stacked"),
                "rune_pickups": player_data.get("rune_pickups"),
                "firstblood_claimed": 1 if player_data.get("firstblood_claimed") else 0,
                "stuns": player_data.get("stuns"),
                "fantasy_points": fantasy_points,
            })
            players_enriched += 1

        # Bulk update all participant stats in a single transaction
        if participant_updates:
            self.match_repo.update_participant_stats_bulk(internal_match_id, participant_updates)

        logger.info(
            f"Enrichment complete: {players_enriched} players enriched, "
            f"{len(players_not_found)} not found, "
            f"Fantasy: Radiant={radiant_fantasy:.1f}, Dire={dire_fantasy:.1f}"
        )

        # Update OpenSkill ratings using fantasy points as weights
        openskill_result = None
        if self.match_service and players_enriched > 0:
            try:
                openskill_result = self.match_service.update_openskill_ratings_for_match(
                    internal_match_id
                )
                if openskill_result.get("success"):
                    logger.info(
                        f"OpenSkill update: {openskill_result.get('players_updated', 0)} players updated"
                    )
                else:
                    logger.warning(
                        f"OpenSkill update failed: {openskill_result.get('error', 'unknown')}"
                    )
            except Exception as e:
                logger.error(f"OpenSkill update error: {e}")
                openskill_result = {"success": False, "error": str(e)}

        return {
            "success": True,
            "players_enriched": players_enriched,
            "players_not_found": players_not_found,
            "duration": match_data.get("duration", 0),
            "radiant_win": match_data.get("radiant_win", False),
            "radiant_score": match_data.get("radiant_score", 0),
            "dire_score": match_data.get("dire_score", 0),
            "fantasy_points_calculated": True,
            "total_fantasy_points": {
                "radiant": round(radiant_fantasy, 2),
                "dire": round(dire_fantasy, 2),
            },
            "openskill_update": openskill_result,
        }

    def backfill_steam_ids(self) -> dict:
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

    def format_match_summary(self, internal_match_id: int) -> str | None:
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
