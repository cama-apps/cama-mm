"""
Service for auto-discovering Dota 2 match IDs for internal matches.

Uses OpenDota API to find matches by correlating:
- Match timestamps (within configurable time window)
- Player overlap (steam_ids appearing in the same match)
- Winning team validation
- Player side validation
"""

import logging
import time
from datetime import datetime

from config import ENRICHMENT_DISCOVERY_TIME_WINDOW, ENRICHMENT_MIN_PLAYER_MATCH
from opendota_integration import OpenDotaAPI

logger = logging.getLogger("cama_bot.services.match_discovery")

# Minimum players with steam_id to attempt discovery
MIN_PLAYERS_FOR_DISCOVERY = 5

# For discovery phase, we require all 10 players by default (from config)
# But we can still try discovery with fewer if we have at least MIN_PLAYERS_FOR_DISCOVERY


class MatchDiscoveryService:
    """
    Discovers Dota 2 match IDs for internal matches by correlating
    player match histories from OpenDota.
    """

    def __init__(
        self,
        match_repo,
        player_repo,
        opendota_api: OpenDotaAPI | None = None,
        match_service=None,
    ):
        self.match_repo = match_repo
        self.player_repo = player_repo
        self.opendota_api = opendota_api or OpenDotaAPI()
        self.match_service = match_service

    def discover_all_matches(self, guild_id: int | None = None, dry_run: bool = False) -> dict:
        """
        Discover Dota 2 match IDs for all unenriched internal matches.

        Args:
            dry_run: If True, don't apply enrichments, just report findings

        Returns:
            Dict with:
            - total_unenriched: int
            - discovered: int (matches found with high confidence)
            - skipped_low_confidence: int
            - skipped_no_steam_ids: int
            - errors: int
            - details: list of {match_id, valve_match_id, confidence, status}
        """
        logger.info(f"Starting match discovery (dry_run={dry_run})")

        normalized_guild = guild_id if guild_id is not None else 0
        unenriched = self.match_repo.get_matches_without_enrichment(normalized_guild, limit=1000)
        results = {
            "total_unenriched": len(unenriched),
            "discovered": 0,
            "skipped_low_confidence": 0,
            "skipped_no_steam_ids": 0,
            "skipped_validation_failed": 0,
            "errors": 0,
            "details": [],
        }

        for match in unenriched:
            match_id = match["match_id"]
            try:
                result = self._discover_single_match(match_id, normalized_guild, dry_run)
                results["details"].append(result)

                if result["status"] == "discovered":
                    results["discovered"] += 1
                elif result["status"] == "low_confidence":
                    results["skipped_low_confidence"] += 1
                elif result["status"] == "no_steam_ids":
                    results["skipped_no_steam_ids"] += 1
                elif result["status"] == "validation_failed":
                    results["skipped_validation_failed"] += 1

            except Exception as e:
                logger.error(f"Error discovering match {match_id}: {e}")
                results["errors"] += 1
                results["details"].append(
                    {
                        "match_id": match_id,
                        "status": "error",
                        "error": str(e),
                    }
                )

            # Small delay to be nice to OpenDota API
            time.sleep(0.5)

        logger.info(
            f"Discovery complete: {results['discovered']} discovered, "
            f"{results['skipped_low_confidence']} low confidence, "
            f"{results['skipped_validation_failed']} validation failed, "
            f"{results['skipped_no_steam_ids']} no steam_ids, "
            f"{results['errors']} errors"
        )

        return results

    def _discover_single_match(
        self, match_id: int, guild_id: int | None, dry_run: bool
    ) -> dict:
        """
        Attempt to discover the Dota 2 match ID for a single internal match.

        Returns dict with match_id, status, and optionally valve_match_id/confidence.
        """
        match = self.match_repo.get_match(match_id, guild_id)
        if not match:
            return {"match_id": match_id, "status": "not_found"}

        participants = self.match_repo.get_match_participants(match_id, guild_id)

        # Get all steam_ids for participants (supports multiple per player)
        discord_ids = [p["discord_id"] for p in participants]
        discord_to_steam_ids = self.player_repo.get_steam_ids_bulk(discord_ids)

        # Flatten all steam_ids and track which discord_id each came from
        steam_ids = []
        steam_to_discord: dict[int, int] = {}  # For validation later
        for p in participants:
            player_steam_ids = discord_to_steam_ids.get(p["discord_id"], [])
            for sid in player_steam_ids:
                if sid not in steam_to_discord:
                    steam_ids.append(sid)
                    steam_to_discord[sid] = p["discord_id"]

        # Count unique players with at least one steam_id
        players_with_steam_id = sum(1 for did in discord_ids if discord_to_steam_ids.get(did))

        if players_with_steam_id < MIN_PLAYERS_FOR_DISCOVERY:
            logger.debug(
                f"Match {match_id}: Only {players_with_steam_id} players with steam_id, "
                f"need {MIN_PLAYERS_FOR_DISCOVERY}"
            )
            return {
                "match_id": match_id,
                "status": "no_steam_ids",
                "players_with_steam_id": players_with_steam_id,
            }

        # Parse match timestamp
        match_time = self._parse_match_time(match.get("match_date"))
        if not match_time:
            return {"match_id": match_id, "status": "no_timestamp"}

        # Query OpenDota for each player's recent matches
        candidate_matches = {}
        time_window = ENRICHMENT_DISCOVERY_TIME_WINDOW

        for steam_id in steam_ids:
            try:
                recent_matches = self.opendota_api.get_player_matches(steam_id, limit=100)
                if not recent_matches:
                    continue

                for m in recent_matches:
                    start_time = m.get("start_time", 0)
                    if abs(start_time - match_time) <= time_window:
                        valve_match_id = m.get("match_id")
                        if valve_match_id not in candidate_matches:
                            candidate_matches[valve_match_id] = set()
                        # Track the discord_id (player), not the steam_id
                        # This way, multiple steam_ids for same player count as one
                        discord_id = steam_to_discord.get(steam_id)
                        if discord_id:
                            candidate_matches[valve_match_id].add(discord_id)

            except Exception as e:
                logger.warning(f"Error fetching matches for steam_id {steam_id}: {e}")

            # Small delay between API calls
            time.sleep(0.2)

        if not candidate_matches:
            return {"match_id": match_id, "status": "no_candidates"}

        # Find best candidate based on unique player count
        best_match_id = None
        best_player_count = 0

        for valve_match_id, matched_discord_ids in candidate_matches.items():
            player_count = len(matched_discord_ids)
            if player_count > best_player_count:
                best_match_id = valve_match_id
                best_player_count = player_count

        # Use strict validation: require all players (configurable via ENRICHMENT_MIN_PLAYER_MATCH)
        min_required = ENRICHMENT_MIN_PLAYER_MATCH
        confidence = best_player_count / players_with_steam_id if players_with_steam_id else 0.0

        if best_match_id and best_player_count >= min_required:
            logger.info(
                f"Match {match_id}: Found valve_match_id={best_match_id} "
                f"with {best_player_count}/{players_with_steam_id} players"
            )

            if not dry_run:
                # Enrich the match with source='auto'
                # The enrichment service will perform additional validation
                # (winning team, player sides) before committing
                from services.match_enrichment_service import MatchEnrichmentService

                enrichment_service = MatchEnrichmentService(
                    self.match_repo,
                    self.player_repo,
                    self.opendota_api,
                    match_service=self.match_service,
                )
                result = enrichment_service.enrich_match(
                    match_id,
                    best_match_id,
                    source="auto",
                    confidence=confidence,
                    guild_id=guild_id,
                )

                # If validation failed in enrichment service, report as low_confidence
                if not result.get("success"):
                    logger.warning(
                        f"Match {match_id}: Enrichment validation failed: {result.get('error')}"
                    )
                    return {
                        "match_id": match_id,
                        "status": "validation_failed",
                        "best_valve_match_id": best_match_id,
                        "confidence": confidence,
                        "player_count": best_player_count,
                        "total_players": players_with_steam_id,
                        "validation_error": result.get("validation_error", result.get("error")),
                    }

            return {
                "match_id": match_id,
                "status": "discovered",
                "valve_match_id": best_match_id,
                "confidence": confidence,
                "player_count": best_player_count,
                "total_players": players_with_steam_id,
            }
        else:
            logger.debug(
                f"Match {match_id}: Best candidate {best_match_id} has only "
                f"{best_player_count}/{players_with_steam_id} players (need {min_required})"
            )
            return {
                "match_id": match_id,
                "status": "low_confidence",
                "best_valve_match_id": best_match_id,
                "confidence": confidence,
                "player_count": best_player_count,
                "total_players": players_with_steam_id,
            }

    def discover_match(self, match_id: int, guild_id: int | None = None) -> dict:
        """
        Public method to discover and enrich a single match.

        Args:
            match_id: Internal match ID to discover
            guild_id: Guild ID for multi-guild isolation

        Returns:
            Dict with status and details (same as _discover_single_match)
        """
        logger.info(f"Auto-discovery triggered for match {match_id}")
        return self._discover_single_match(match_id, guild_id, dry_run=False)

    def _parse_match_time(self, match_date) -> int | None:
        """Parse match_date to Unix timestamp."""
        if not match_date:
            return None

        if isinstance(match_date, (int, float)):
            return int(match_date)

        if isinstance(match_date, str):
            try:
                # Try ISO format
                dt = datetime.fromisoformat(match_date.replace("Z", "+00:00"))
                return int(dt.timestamp())
            except ValueError:
                pass

            try:
                # Try common SQLite format
                dt = datetime.strptime(match_date, "%Y-%m-%d %H:%M:%S")
                return int(dt.timestamp())
            except ValueError:
                pass

        return None
