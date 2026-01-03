"""
Service for fetching player profile statistics from OpenDota API.
"""

import logging
from typing import Dict, List, Optional
from datetime import datetime, timedelta

from opendota_integration import OpenDotaAPI
from utils.hero_lookup import get_hero_name

logger = logging.getLogger("cama_bot.services.opendota_player")

# Cache TTL in seconds (1 hour)
CACHE_TTL_SECONDS = 3600


class OpenDotaPlayerService:
    """
    Fetches and caches player statistics from OpenDota API.

    Provides:
    - Win/loss totals
    - Average KDA, GPM, XPM
    - Top heroes
    - Recent match info
    """

    def __init__(self, player_repo, profile_cache_repo=None):
        """
        Initialize the service.

        Args:
            player_repo: PlayerRepository for steam_id lookup
            profile_cache_repo: Optional cache repository (future use)
        """
        self.player_repo = player_repo
        self.profile_cache_repo = profile_cache_repo
        self.api = OpenDotaAPI()
        # In-memory cache (fallback if no DB cache)
        self._memory_cache: Dict[int, Dict] = {}

    def get_player_profile(self, discord_id: int, force_refresh: bool = False) -> Optional[Dict]:
        """
        Get comprehensive player profile from OpenDota.

        Args:
            discord_id: Player's Discord ID
            force_refresh: Force API refresh, ignoring cache

        Returns:
            Profile dict with stats, or None if unavailable
        """
        # Get steam_id
        steam_id = self.player_repo.get_steam_id(discord_id)
        if not steam_id:
            logger.warning(f"No steam_id for discord {discord_id}")
            return None

        # Check memory cache
        if not force_refresh and discord_id in self._memory_cache:
            cached = self._memory_cache[discord_id]
            if datetime.now() - cached["cached_at"] < timedelta(seconds=CACHE_TTL_SECONDS):
                logger.debug(f"Returning cached profile for discord {discord_id}")
                return cached["data"]

        # Fetch from API
        logger.info(f"Fetching OpenDota profile for steam_id {steam_id}")
        profile = self._fetch_profile(steam_id)

        if profile:
            # Cache in memory
            self._memory_cache[discord_id] = {
                "data": profile,
                "cached_at": datetime.now(),
            }

        return profile

    def _fetch_profile(self, steam_id: int) -> Optional[Dict]:
        """Fetch all profile data from OpenDota APIs."""
        try:
            # Basic player info
            player_data = self.api.get_player_data(steam_id)
            if not player_data:
                return None

            # Win/Loss
            wl = self._fetch_win_loss(steam_id)

            # Totals for averages
            totals = self._fetch_totals(steam_id)

            # Top heroes
            top_heroes = self._fetch_top_heroes(steam_id)

            # Recent matches
            recent_matches = self._fetch_recent_matches(steam_id)

            return {
                "steam_id": steam_id,
                "persona_name": player_data.get("profile", {}).get("personaname", "Unknown"),
                "avatar": player_data.get("profile", {}).get("avatar"),
                "rank_tier": player_data.get("rank_tier"),
                "mmr_estimate": player_data.get("mmr_estimate", {}).get("estimate"),
                "wins": wl.get("win", 0),
                "losses": wl.get("lose", 0),
                "win_rate": self._calc_win_rate(wl.get("win", 0), wl.get("lose", 0)),
                "avg_kills": totals.get("avg_kills", 0),
                "avg_deaths": totals.get("avg_deaths", 0),
                "avg_assists": totals.get("avg_assists", 0),
                "avg_gpm": totals.get("avg_gpm", 0),
                "avg_xpm": totals.get("avg_xpm", 0),
                "avg_last_hits": totals.get("avg_last_hits", 0),
                "top_heroes": top_heroes,
                "recent_matches": recent_matches,
                "last_match_id": recent_matches[0]["match_id"] if recent_matches else None,
            }
        except Exception as e:
            logger.error(f"Error fetching profile for steam_id {steam_id}: {e}")
            return None

    def _fetch_win_loss(self, steam_id: int) -> Dict:
        """Fetch win/loss totals."""
        try:
            response = self.api._make_request(f"{self.api.BASE_URL}/players/{steam_id}/wl")
            if response and response.status_code == 200:
                return response.json()
        except Exception as e:
            logger.error(f"Error fetching W/L for {steam_id}: {e}")
        return {"win": 0, "lose": 0}

    def _fetch_totals(self, steam_id: int) -> Dict:
        """Fetch player totals for calculating averages."""
        try:
            response = self.api._make_request(f"{self.api.BASE_URL}/players/{steam_id}/totals")
            if response and response.status_code == 200:
                totals_data = response.json()
                result = {}
                for item in totals_data:
                    field = item.get("field")
                    n = item.get("n", 0)
                    total = item.get("sum", 0)
                    if n > 0:
                        avg = total / n
                        if field == "kills":
                            result["avg_kills"] = round(avg, 1)
                        elif field == "deaths":
                            result["avg_deaths"] = round(avg, 1)
                        elif field == "assists":
                            result["avg_assists"] = round(avg, 1)
                        elif field == "gold_per_min":
                            result["avg_gpm"] = int(avg)
                        elif field == "xp_per_min":
                            result["avg_xpm"] = int(avg)
                        elif field == "last_hits":
                            result["avg_last_hits"] = int(avg)
                return result
        except Exception as e:
            logger.error(f"Error fetching totals for {steam_id}: {e}")
        return {}

    def _fetch_top_heroes(self, steam_id: int, limit: int = 5) -> List[Dict]:
        """Fetch top heroes by games played."""
        try:
            response = self.api._make_request(f"{self.api.BASE_URL}/players/{steam_id}/heroes")
            if response and response.status_code == 200:
                heroes_data = response.json()
                # Sort by games and take top N
                top = sorted(heroes_data, key=lambda x: x.get("games", 0), reverse=True)[:limit]
                return [
                    {
                        "hero_id": h["hero_id"],
                        "hero_name": get_hero_name(int(h["hero_id"])),
                        "games": h.get("games", 0),
                        "wins": h.get("win", 0),
                        "win_rate": self._calc_win_rate(h.get("win", 0), h.get("games", 0) - h.get("win", 0)),
                    }
                    for h in top
                    if h.get("games", 0) > 0
                ]
        except Exception as e:
            logger.error(f"Error fetching heroes for {steam_id}: {e}")
        return []

    def _fetch_recent_matches(self, steam_id: int, limit: int = 5) -> List[Dict]:
        """Fetch recent matches."""
        try:
            response = self.api._make_request(
                f"{self.api.BASE_URL}/players/{steam_id}/recentMatches"
            )
            if response and response.status_code == 200:
                matches_data = response.json()[:limit]
                return [
                    {
                        "match_id": m.get("match_id"),
                        "hero_id": m.get("hero_id"),
                        "hero_name": get_hero_name(m.get("hero_id", 0)),
                        "kills": m.get("kills", 0),
                        "deaths": m.get("deaths", 0),
                        "assists": m.get("assists", 0),
                        "won": self._did_win(m),
                        "duration": m.get("duration", 0),
                        "start_time": m.get("start_time", 0),
                    }
                    for m in matches_data
                ]
        except Exception as e:
            logger.error(f"Error fetching recent matches for {steam_id}: {e}")
        return []

    def _did_win(self, match: Dict) -> bool:
        """Determine if player won the match."""
        player_slot = match.get("player_slot", 0)
        radiant_win = match.get("radiant_win", False)
        is_radiant = player_slot < 128
        return is_radiant == radiant_win

    def _calc_win_rate(self, wins: int, losses: int) -> float:
        """Calculate win rate as a percentage."""
        total = wins + losses
        if total == 0:
            return 0.0
        return round((wins / total) * 100, 1)

    def format_profile_embed(self, discord_id: int, target_name: str) -> Optional[Dict]:
        """
        Format profile data for Discord embed.

        Returns dict suitable for creating discord.Embed, or None if unavailable.
        """
        profile = self.get_player_profile(discord_id)
        if not profile:
            return None

        # Format KDA
        kda = f"{profile['avg_kills']}/{profile['avg_deaths']}/{profile['avg_assists']}"

        # Format top heroes
        heroes_lines = []
        for i, hero in enumerate(profile.get("top_heroes", [])[:3], 1):
            heroes_lines.append(
                f"{i}. {hero['hero_name']} - {hero['games']} games ({hero['win_rate']}%)"
            )

        # Format recent matches
        recent_lines = []
        for match in profile.get("recent_matches", [])[:3]:
            result = "W" if match["won"] else "L"
            kda_str = f"{match['kills']}/{match['deaths']}/{match['assists']}"
            recent_lines.append(f"[{result}] {match['hero_name']} ({kda_str})")

        return {
            "title": f"Profile: {target_name}",
            "fields": [
                {
                    "name": "Overall",
                    "value": f"{profile['wins']}W / {profile['losses']}L ({profile['win_rate']}%)",
                    "inline": True,
                },
                {
                    "name": "Avg KDA",
                    "value": kda,
                    "inline": True,
                },
                {
                    "name": "Avg GPM/XPM",
                    "value": f"{profile['avg_gpm']} / {profile['avg_xpm']}",
                    "inline": True,
                },
                {
                    "name": "Top Heroes",
                    "value": "\n".join(heroes_lines) if heroes_lines else "No data",
                    "inline": False,
                },
                {
                    "name": "Recent Matches",
                    "value": "\n".join(recent_lines) if recent_lines else "No data",
                    "inline": False,
                },
            ],
            "footer": f"Data from OpenDota | Steam ID: {profile['steam_id']}",
            "last_match_id": profile.get("last_match_id"),
        }
