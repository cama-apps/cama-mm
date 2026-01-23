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

    def __init__(self, match_repo, player_repo, opendota_api: OpenDotaAPI | None = None):
        self.match_repo = match_repo
        self.player_repo = player_repo
        self.opendota_api = opendota_api or OpenDotaAPI()

    def discover_all_matches(self, dry_run: bool = False) -> dict:
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

        unenriched = self.match_repo.get_matches_without_enrichment(limit=1000)
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
                result = self._discover_single_match(match_id, dry_run)
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

    def _discover_single_match(self, match_id: int, dry_run: bool) -> dict:
        """
        Attempt to discover the Dota 2 match ID for a single internal match.

        Returns dict with match_id, status, and optionally valve_match_id/confidence.
        """
        match = self.match_repo.get_match(match_id)
        if not match:
            return {"match_id": match_id, "status": "not_found"}

        participants = self.match_repo.get_match_participants(match_id)

        # Get steam_ids for participants
        steam_ids = []
        for p in participants:
            steam_id = self.player_repo.get_steam_id(p["discord_id"])
            if steam_id:
                steam_ids.append(steam_id)

        if len(steam_ids) < MIN_PLAYERS_FOR_DISCOVERY:
            logger.debug(
                f"Match {match_id}: Only {len(steam_ids)} players with steam_id, "
                f"need {MIN_PLAYERS_FOR_DISCOVERY}"
            )
            return {
                "match_id": match_id,
                "status": "no_steam_ids",
                "players_with_steam_id": len(steam_ids),
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
                        candidate_matches[valve_match_id].add(steam_id)

            except Exception as e:
                logger.warning(f"Error fetching matches for steam_id {steam_id}: {e}")

            # Small delay between API calls
            time.sleep(0.2)

        if not candidate_matches:
            return {"match_id": match_id, "status": "no_candidates"}

        # Find best candidate based on player count
        best_match_id = None
        best_player_count = 0

        for valve_match_id, players in candidate_matches.items():
            player_count = len(players)
            if player_count > best_player_count:
                best_match_id = valve_match_id
                best_player_count = player_count

        # Use strict validation: require all players (configurable via ENRICHMENT_MIN_PLAYER_MATCH)
        min_required = ENRICHMENT_MIN_PLAYER_MATCH
        confidence = best_player_count / len(steam_ids) if steam_ids else 0.0

        if best_match_id and best_player_count >= min_required:
            logger.info(
                f"Match {match_id}: Found valve_match_id={best_match_id} "
                f"with {best_player_count}/{len(steam_ids)} players"
            )

            if not dry_run:
                # Enrich the match with source='auto'
                # The enrichment service will perform additional validation
                # (winning team, player sides) before committing
                from services.match_enrichment_service import MatchEnrichmentService

                enrichment_service = MatchEnrichmentService(
                    self.match_repo, self.player_repo, self.opendota_api
                )
                result = enrichment_service.enrich_match(
                    match_id,
                    best_match_id,
                    source="auto",
                    confidence=confidence,
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
                        "total_players": len(steam_ids),
                        "validation_error": result.get("validation_error", result.get("error")),
                    }

            return {
                "match_id": match_id,
                "status": "discovered",
                "valve_match_id": best_match_id,
                "confidence": confidence,
                "player_count": best_player_count,
                "total_players": len(steam_ids),
            }
        else:
            logger.debug(
                f"Match {match_id}: Best candidate {best_match_id} has only "
                f"{best_player_count}/{len(steam_ids)} players (need {min_required})"
            )
            return {
                "match_id": match_id,
                "status": "low_confidence",
                "best_valve_match_id": best_match_id,
                "confidence": confidence,
                "player_count": best_player_count,
                "total_players": len(steam_ids),
            }

    def discover_match(self, match_id: int) -> dict:
        """
        Public method to discover and enrich a single match.

        Args:
            match_id: Internal match ID to discover

        Returns:
            Dict with status and details (same as _discover_single_match)
        """
        logger.info(f"Auto-discovery triggered for match {match_id}")
        return self._discover_single_match(match_id, dry_run=False)

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
