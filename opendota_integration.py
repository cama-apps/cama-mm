"""
OpenDota API integration for fetching player data.
OpenDota API: https://docs.opendota.com/
"""

import logging
import os
import re
import threading
import time

import requests

logger = logging.getLogger("cama_bot.opendota")


class RateLimiter:
    """
    Simple token bucket rate limiter.

    OpenDota rate limits:
    - Without API key: 60 requests/minute
    - With API key: 1200 requests/minute
    """

    def __init__(self, requests_per_minute: int = 60):
        """
        Initialize rate limiter.

        Args:
            requests_per_minute: Maximum requests allowed per minute
        """
        self.requests_per_minute = requests_per_minute
        self.tokens = requests_per_minute
        self.last_update = time.time()
        self.lock = threading.Lock()

    def _refill(self):
        """Refill tokens based on time elapsed."""
        now = time.time()
        time_passed = now - self.last_update
        # Add tokens based on time passed (linear refill)
        tokens_to_add = time_passed * (self.requests_per_minute / 60.0)
        self.tokens = min(self.requests_per_minute, self.tokens + tokens_to_add)
        self.last_update = now

    def acquire(self, timeout: float = 10.0) -> bool:
        """
        Acquire a token, blocking if necessary.

        Args:
            timeout: Maximum time to wait for a token (seconds)

        Returns:
            True if token was acquired, False if timeout
        """
        start_time = time.time()

        while True:
            with self.lock:
                self._refill()
                if self.tokens >= 1:
                    self.tokens -= 1
                    return True

            # Check timeout
            if time.time() - start_time >= timeout:
                return False

            # Wait a bit before trying again
            time.sleep(0.1)

    def wait_for_token(self):
        """Wait until a token is available (no timeout)."""
        while True:
            with self.lock:
                self._refill()
                if self.tokens >= 1:
                    self.tokens -= 1
                    return
            time.sleep(0.1)


class OpenDotaAPI:
    """Wrapper for OpenDota API calls with rate limiting."""

    BASE_URL = "https://api.opendota.com/api"

    # Shared rate limiter across all instances
    _rate_limiter = None
    _rate_limiter_lock = threading.Lock()

    def __init__(self, api_key: str | None = None):
        """
        Initialize OpenDota API client.

        Args:
            api_key: Optional OpenDota API key for higher rate limits
        """
        self.session = requests.Session()
        self.api_key = api_key or os.getenv("OPENDOTA_API_KEY")

        # Initialize shared rate limiter if not already done
        with OpenDotaAPI._rate_limiter_lock:
            if OpenDotaAPI._rate_limiter is None:
                # Use higher rate limit if API key is available
                rate_limit = 1200 if self.api_key else 60
                OpenDotaAPI._rate_limiter = RateLimiter(requests_per_minute=rate_limit)
                logger.info(f"OpenDota rate limiter initialized: {rate_limit} requests/minute")

    def _make_request(self, url: str, params: dict | None = None) -> requests.Response | None:
        """
        Make a rate-limited request to the OpenDota API.

        Args:
            url: Full URL to request
            params: Optional query parameters

        Returns:
            Response object or None if rate limit exceeded
        """
        # Wait for rate limiter
        if not OpenDotaAPI._rate_limiter.acquire(timeout=30.0):
            logger.warning("OpenDota API rate limit exceeded, request timed out")
            return None

        # Add API key if available
        if self.api_key:
            params = params or {}
            params["api_key"] = self.api_key

        return self.session.get(url, params=params)

    def extract_player_id_from_dotabuff(self, dotabuff_url: str) -> int | None:
        """
        Extract Steam ID from Dotabuff URL.

        Dotabuff URLs look like: https://www.dotabuff.com/players/123456789
        We need to convert this to Steam ID for OpenDota.

        Args:
            dotabuff_url: Dotabuff profile URL

        Returns:
            Steam ID (32-bit) or None if invalid
        """
        # Extract the number from Dotabuff URL
        match = re.search(r"/players/(\d+)", dotabuff_url)
        if not match:
            return None

        dotabuff_id = int(match.group(1))
        # Dotabuff uses Steam ID64, OpenDota needs Steam ID32
        # Steam ID64 = Steam ID32 + 76561197960265728
        # So Steam ID32 = Steam ID64 - 76561197960265728
        steam_id32 = dotabuff_id - 76561197960265728

        return steam_id32

    def get_player_data(self, steam_id: int) -> dict | None:
        """
        Get player data from OpenDota.

        Args:
            steam_id: Steam ID (32-bit)

        Returns:
            Player data dictionary or None if not found
        """
        try:
            logger.debug(f"Fetching player data from OpenDota for Steam ID {steam_id}")
            response = self._make_request(f"{self.BASE_URL}/players/{steam_id}")
            if response is None:
                logger.warning(f"Rate limit prevented fetching player data for Steam ID {steam_id}")
                return None
            response.raise_for_status()
            data = response.json()
            logger.debug(f"Successfully fetched player data for Steam ID {steam_id}")
            return data
        except requests.exceptions.RequestException as e:
            logger.error(f"Error fetching player data for Steam ID {steam_id}: {e}")
            return None

    def get_player_mmr(self, steam_id: int) -> int | None:
        """
        Get player's current MMR.

        Args:
            steam_id: Steam ID (32-bit)

        Returns:
            MMR value or None if not available
        """
        player_data = self.get_player_data(steam_id)
        if not player_data:
            return None

        rank_tier = player_data.get("rank_tier", 0)
        is_immortal = rank_tier == 80
        leaderboard_rank = player_data.get("leaderboard_rank")

        # Try mmr_estimate first (OpenDota's estimate)
        mmr_estimate = player_data.get("mmr_estimate", {}).get("estimate")
        if mmr_estimate:
            return int(mmr_estimate)

        # Fallback to computed_mmr (calculated from match data)
        computed_mmr = player_data.get("computed_mmr")
        if computed_mmr:
            mmr = int(computed_mmr)
            # If Immortal but computed_mmr seems too low, use minimum Immortal MMR
            if is_immortal and mmr < 5500:
                return self._estimate_immortal_mmr(leaderboard_rank)
            return mmr

        # Fallback to legacy ranked MMR
        solo_mmr = player_data.get("solo_competitive_rank")
        if solo_mmr:
            return int(solo_mmr)

        # If Immortal but no MMR data, estimate from leaderboard rank
        if is_immortal:
            return self._estimate_immortal_mmr(leaderboard_rank)

        return None

    def _estimate_immortal_mmr(self, leaderboard_rank: int | None) -> int:
        """
        Estimate MMR for Immortal players based on leaderboard rank.

        Args:
            leaderboard_rank: Immortal leaderboard rank (lower = better)

        Returns:
            Estimated MMR (minimum 5500 for Immortal)
        """
        if leaderboard_rank is None:
            # No leaderboard rank = bottom of Immortal, estimate ~5500
            return 5500

        # Rough estimation: higher rank (lower number) = higher MMR
        # Top 100: ~7000+, Top 500: ~6500+, Top 1000: ~6000+, Rest: ~5500-6000
        if leaderboard_rank <= 100:
            return 7000
        elif leaderboard_rank <= 500:
            return 6500
        elif leaderboard_rank <= 1000:
            return 6000
        else:
            # For ranks > 1000, use a formula: 5500 + (1000 / rank) * 500
            # This gives diminishing returns as rank increases
            estimated = 5500 + int((1000 / leaderboard_rank) * 500)
            return min(estimated, 6000)  # Cap at 6000 for lower ranks

    def get_player_rank_tier(self, steam_id: int) -> int | None:
        """
        Get player's rank tier (medal).

        Args:
            steam_id: Steam ID (32-bit)

        Returns:
            Rank tier number or None
        """
        player_data = self.get_player_data(steam_id)
        if not player_data:
            return None

        return player_data.get("rank_tier")

    def get_player_roles(self, steam_id: int) -> dict | None:
        """
        Get player's role preferences based on match history.

        Args:
            steam_id: Steam ID (32-bit)

        Returns:
            Dictionary with role statistics
        """
        try:
            response = self._make_request(f"{self.BASE_URL}/players/{steam_id}/heroes")
            if response is None:
                logger.warning(f"Rate limit prevented fetching hero data for Steam ID {steam_id}")
                return None
            response.raise_for_status()
            heroes_data = response.json()

            # Analyze heroes to determine preferred roles
            # This is a simplified version - you'd want to map heroes to roles
            # and calculate which roles they play most
            return heroes_data
        except requests.exceptions.RequestException as e:
            logger.error(f"Error fetching hero data: {e}")
            return None

    def get_player_matches(self, steam_id: int, limit: int = 20) -> list | None:
        """
        Get recent matches for a player.

        Args:
            steam_id: Steam ID (32-bit)
            limit: Number of matches to fetch

        Returns:
            List of match data
        """
        try:
            response = self._make_request(
                f"{self.BASE_URL}/players/{steam_id}/matches", params={"limit": limit}
            )
            if response is None:
                logger.warning(f"Rate limit prevented fetching matches for Steam ID {steam_id}")
                return None
            response.raise_for_status()
            return response.json()
        except requests.exceptions.RequestException as e:
            logger.error(f"Error fetching matches: {e}")
            return None

    def get_match_details(self, match_id: int) -> dict | None:
        """
        Get detailed match data from OpenDota.

        Note: Valve's GetMatchDetails API has been broken since May 2024 (patch 7.36).
        OpenDota parses replay files directly, so it still works.

        Args:
            match_id: The Dota 2 match ID

        Returns:
            Match details dict or None if not found
        """
        try:
            logger.info(f"Fetching match details from OpenDota for match_id={match_id}")
            response = self._make_request(f"{self.BASE_URL}/matches/{match_id}")
            if response is None:
                logger.warning(f"Rate limit prevented fetching match {match_id}")
                return None
            response.raise_for_status()
            data = response.json()

            # Check if match was found (OpenDota returns empty object or error for missing matches)
            if not data or data.get("error"):
                logger.warning(f"Match {match_id} not found in OpenDota")
                return None

            logger.info(f"Successfully fetched match {match_id} from OpenDota")
            return data
        except requests.exceptions.RequestException as e:
            logger.error(f"Error fetching match {match_id}: {e}")
            return None


def test_opendota():
    """Test OpenDota API integration."""
    api = OpenDotaAPI()

    # Example: Test with a known Dotabuff URL
    # You'd replace this with actual player URLs
    dotabuff_url = "https://www.dotabuff.com/players/123456789"
    steam_id = api.extract_player_id_from_dotabuff(dotabuff_url)

    if steam_id:
        print(f"Extracted Steam ID: {steam_id}")
        player_data = api.get_player_data(steam_id)
        if player_data:
            print(f"Player: {player_data.get('profile', {}).get('personaname')}")
            mmr = api.get_player_mmr(steam_id)
            print(f"MMR: {mmr}")
        else:
            print("Could not fetch player data")
    else:
        print("Invalid Dotabuff URL")


if __name__ == "__main__":
    test_opendota()
