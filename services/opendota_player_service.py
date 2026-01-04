"""
Service for fetching player profile statistics from OpenDota API.
"""

import logging
from datetime import datetime, timedelta

from opendota_integration import OpenDotaAPI
from utils.hero_lookup import get_hero_name

logger = logging.getLogger("cama_bot.services.opendota_player")

# Cache TTL in seconds (1 hour)
CACHE_TTL_SECONDS = 3600

# Lane role mapping from OpenDota
# 0 = Unknown/Roaming, 1-4 = Standard lanes
LANE_ROLE_NAMES = {
    0: "Roaming",
    1: "Safe Lane",
    2: "Mid",
    3: "Off Lane",
    4: "Jungle",
}

# Hero primary attribute mapping (from OpenDota)
# Will be fetched once and cached
_HERO_ATTRIBUTES_CACHE: dict[int, str] | None = None


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
        self._memory_cache: dict[int, dict] = {}

    def get_player_profile(self, discord_id: int, force_refresh: bool = False) -> dict | None:
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

    def _fetch_profile(self, steam_id: int) -> dict | None:
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

    def _fetch_win_loss(self, steam_id: int) -> dict:
        """Fetch win/loss totals."""
        try:
            response = self.api._make_request(f"{self.api.BASE_URL}/players/{steam_id}/wl")
            if response and response.status_code == 200:
                return response.json()
        except Exception as e:
            logger.error(f"Error fetching W/L for {steam_id}: {e}")
        return {"win": 0, "lose": 0}

    def _fetch_totals(self, steam_id: int) -> dict:
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

    def _fetch_top_heroes(self, steam_id: int, limit: int = 5) -> list[dict]:
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
                        "win_rate": self._calc_win_rate(
                            h.get("win", 0), h.get("games", 0) - h.get("win", 0)
                        ),
                    }
                    for h in top
                    if h.get("games", 0) > 0
                ]
        except Exception as e:
            logger.error(f"Error fetching heroes for {steam_id}: {e}")
        return []

    def _fetch_recent_matches(self, steam_id: int, limit: int = 5) -> list[dict]:
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

    def _did_win(self, match: dict) -> bool:
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

    def format_profile_embed(self, discord_id: int, target_name: str) -> dict | None:
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

    def get_full_stats(self, discord_id: int, match_limit: int = 100) -> dict | None:
        """
        Get comprehensive player statistics including hero and lane distributions.

        Args:
            discord_id: Player's Discord ID
            match_limit: Number of recent matches to analyze (default 100)

        Returns:
            Full stats dict with:
            - Basic stats (W/L, KDA, GPM/XPM)
            - Hero attribute distribution (STR/AGI/INT/Universal %)
            - Lane distribution (Safe/Mid/Off/Jungle %)
            - Top heroes
            - Recent performance
        """
        steam_id = self.player_repo.get_steam_id(discord_id)
        if not steam_id:
            logger.warning(f"No steam_id for discord {discord_id}")
            return None

        try:
            # Get basic profile first
            profile = self.get_player_profile(discord_id)
            if not profile:
                return None

            # Fetch matches with lane_role projection for distribution analysis
            matches = self._fetch_matches_for_stats(steam_id, limit=match_limit)

            # Calculate distributions
            attr_dist = self._calc_attribute_distribution(matches)
            lane_dist, lane_parsed_count = self._calc_lane_distribution(matches)

            # Calculate recent performance (last 20 matches)
            recent_matches = matches[:20] if matches else []
            recent_wins = sum(1 for m in recent_matches if self._did_win(m))
            recent_losses = len(recent_matches) - recent_wins
            recent_winrate = self._calc_win_rate(recent_wins, recent_losses)

            return {
                # Basic stats from profile
                "steam_id": steam_id,
                "persona_name": profile.get("persona_name", "Unknown"),
                "rank_tier": profile.get("rank_tier"),
                "mmr_estimate": profile.get("mmr_estimate"),
                "total_wins": profile.get("wins", 0),
                "total_losses": profile.get("losses", 0),
                "total_winrate": profile.get("win_rate", 0),
                "avg_kills": profile.get("avg_kills", 0),
                "avg_deaths": profile.get("avg_deaths", 0),
                "avg_assists": profile.get("avg_assists", 0),
                "avg_gpm": profile.get("avg_gpm", 0),
                "avg_xpm": profile.get("avg_xpm", 0),
                "avg_last_hits": profile.get("avg_last_hits", 0),
                # Distributions
                "attribute_distribution": attr_dist,
                "lane_distribution": lane_dist,
                "lane_parsed_count": lane_parsed_count,
                # Top heroes
                "top_heroes": profile.get("top_heroes", []),
                # Recent matches (for display)
                "recent_matches": profile.get("recent_matches", []),
                # Recent performance
                "recent_wins": recent_wins,
                "recent_losses": recent_losses,
                "recent_winrate": recent_winrate,
                "matches_analyzed": len(matches) if matches else 0,
            }
        except Exception as e:
            logger.error(f"Error getting full stats for discord {discord_id}: {e}")
            return None

    def _fetch_matches_for_stats(self, steam_id: int, limit: int = 100) -> list[dict]:
        """Fetch recent matches with projections for stats analysis."""
        try:
            response = self.api._make_request(
                f"{self.api.BASE_URL}/players/{steam_id}/matches",
                params={
                    "limit": limit,
                    "project": [
                        "hero_id",
                        "lane_role",
                        "player_slot",
                        "radiant_win",
                        "kills",
                        "deaths",
                        "assists",
                    ],
                },
            )
            if response and response.status_code == 200:
                return response.json()
        except Exception as e:
            logger.error(f"Error fetching matches for stats: {e}")
        return []

    def _get_hero_attributes(self) -> dict[int, str]:
        """Get hero ID -> primary attribute mapping from OpenDota."""
        global _HERO_ATTRIBUTES_CACHE

        if _HERO_ATTRIBUTES_CACHE is not None:
            return _HERO_ATTRIBUTES_CACHE

        try:
            response = self.api._make_request(f"{self.api.BASE_URL}/heroes")
            if response and response.status_code == 200:
                heroes_data = response.json()
                _HERO_ATTRIBUTES_CACHE = {
                    h["id"]: h.get("primary_attr", "all")  # "agi", "str", "int", "all" (universal)
                    for h in heroes_data
                }
                logger.info(f"Cached hero attributes for {len(_HERO_ATTRIBUTES_CACHE)} heroes")
                return _HERO_ATTRIBUTES_CACHE
        except Exception as e:
            logger.error(f"Error fetching hero attributes: {e}")

        return {}

    def _calc_attribute_distribution(self, matches: list[dict]) -> dict[str, float]:
        """
        Calculate hero attribute distribution from matches.

        Returns dict like {"str": 25.0, "agi": 35.0, "int": 30.0, "all": 10.0}
        """
        if not matches:
            return {"str": 0, "agi": 0, "int": 0, "all": 0}

        hero_attrs = self._get_hero_attributes()
        counts = {"str": 0, "agi": 0, "int": 0, "all": 0}
        total = 0

        for match in matches:
            hero_id = match.get("hero_id")
            if hero_id and hero_id in hero_attrs:
                attr = hero_attrs[hero_id]
                if attr in counts:
                    counts[attr] += 1
                    total += 1

        if total == 0:
            return {"str": 0, "agi": 0, "int": 0, "all": 0}

        return {attr: round((count / total) * 100, 1) for attr, count in counts.items()}

    def _calc_lane_distribution(self, matches: list[dict]) -> tuple[dict[str, float], int]:
        """
        Calculate lane role distribution from matches.

        Returns:
            Tuple of (distribution dict, parsed match count)
            Distribution like {"Safe Lane": 40.0, "Mid": 20.0, "Off Lane": 30.0, ...}
        """
        if not matches:
            return dict.fromkeys(LANE_ROLE_NAMES.values(), 0), 0

        counts = dict.fromkeys(LANE_ROLE_NAMES.values(), 0)
        total = 0

        for match in matches:
            lane_role = match.get("lane_role")
            # Use 'is not None' since lane_role=0 (Roaming) is valid but falsy
            if lane_role is not None and lane_role in LANE_ROLE_NAMES:
                counts[LANE_ROLE_NAMES[lane_role]] += 1
                total += 1

        if total == 0:
            return dict.fromkeys(LANE_ROLE_NAMES.values(), 0), 0

        distribution = {lane: round((count / total) * 100, 1) for lane, count in counts.items()}
        return distribution, total

    def get_recent_matches_detailed(self, discord_id: int, limit: int = 10) -> list[dict] | None:
        """
        Get recent matches with details for image generation.

        Args:
            discord_id: Player's Discord ID
            limit: Number of matches to fetch (default 10)

        Returns:
            List of match dicts with: hero_id, hero_name, kills, deaths, assists,
            won, duration, match_id, start_time
        """
        steam_id = self.player_repo.get_steam_id(discord_id)
        if not steam_id:
            return None

        try:
            response = self.api._make_request(
                f"{self.api.BASE_URL}/players/{steam_id}/matches",
                params={
                    "limit": limit,
                    "project": [
                        "hero_id",
                        "kills",
                        "deaths",
                        "assists",
                        "duration",
                        "player_slot",
                        "radiant_win",
                        "start_time",
                        "match_id",
                    ],
                },
            )
            if response and response.status_code == 200:
                matches = response.json()
                return [
                    {
                        "hero_id": m.get("hero_id"),
                        "hero_name": get_hero_name(m.get("hero_id", 0)),
                        "kills": m.get("kills", 0),
                        "deaths": m.get("deaths", 0),
                        "assists": m.get("assists", 0),
                        "won": self._did_win(m),
                        "duration": m.get("duration", 0),
                        "match_id": m.get("match_id"),
                        "start_time": m.get("start_time", 0),
                    }
                    for m in matches
                ]
        except Exception as e:
            logger.error(f"Error fetching recent matches for discord {discord_id}: {e}")

        return None

    def get_hero_role_distribution(
        self, discord_id: int, match_limit: int = 50
    ) -> dict[str, float] | None:
        """
        Calculate hero role distribution based on heroes played.

        Uses dotabase to map hero_id to roles, then aggregates across matches.

        Args:
            discord_id: Player's Discord ID
            match_limit: Number of matches to analyze

        Returns:
            Dict mapping role name to percentage (0-100)
        """
        steam_id = self.player_repo.get_steam_id(discord_id)
        if not steam_id:
            return None

        try:
            # Fetch matches
            matches = self._fetch_matches_for_stats(steam_id, limit=match_limit)
            if not matches:
                return None

            # Get hero roles from dotabase
            hero_roles = self._get_hero_roles()
            if not hero_roles:
                return None

            # Count role occurrences (weighted by games)
            role_counts: dict[str, float] = {}
            total = 0

            for match in matches:
                hero_id = match.get("hero_id")
                if hero_id and hero_id in hero_roles:
                    roles = hero_roles[hero_id]
                    # Each role gets weighted contribution
                    for role, weight in roles.items():
                        role_counts[role] = role_counts.get(role, 0) + weight
                        total += weight

            if total == 0:
                return None

            # Normalize to percentages
            return {
                role: round((count / total) * 100, 1)
                for role, count in sorted(role_counts.items(), key=lambda x: x[1], reverse=True)
            }
        except Exception as e:
            logger.error(f"Error calculating role distribution: {e}")
            return None

    def _get_hero_roles(self) -> dict[int, dict[str, float]]:
        """
        Get hero ID -> role weights mapping from dotabase.

        Returns dict like {1: {"Carry": 3, "Escape": 3, "Nuker": 1}, ...}
        Weights come from dotabase role_levels (e.g., "3|3|1" -> weights)
        """
        try:
            from dotabase import Hero, dotabase_session

            session = dotabase_session()
            heroes = session.query(Hero).all()

            result = {}
            for hero in heroes:
                if not hero.roles or not hero.role_levels:
                    continue

                roles = hero.roles.split("|")
                levels = hero.role_levels.split("|")

                role_weights = {}
                for role, level in zip(roles, levels):
                    try:
                        role_weights[role] = int(level)
                    except ValueError:
                        role_weights[role] = 1

                result[hero.id] = role_weights

            return result
        except ImportError:
            logger.warning("dotabase not available for role lookup")
            return {}
        except Exception as e:
            logger.error(f"Error loading hero roles from dotabase: {e}")
            return {}
