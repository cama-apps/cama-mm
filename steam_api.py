"""
Valve Steam Web API integration for fetching Dota 2 match data.
API Documentation: https://wiki.teamfortress.com/wiki/WebAPI#Dota_2
"""

import logging
import os
import threading
import time

import requests

from config import ENRICHMENT_RETRY_DELAYS
from utils.http_safety import DEFAULT_MAX_BYTES as _MAX_RESPONSE_BYTES
from utils.http_safety import parse_json_bounded, retry_after_seconds

logger = logging.getLogger("cama_bot.steam_api")

# Default timeout for all HTTP calls (seconds). Matches opendota_integration.py.
_REQUEST_TIMEOUT = 30

# Status codes worth retrying (429 rate-limit + transient 5xx).
_RETRYABLE_STATUS_CODES = frozenset({429, 500, 502, 503, 504})


class SteamAPIRateLimiter:
    """
    Simple rate limiter for Valve API.

    Valve doesn't publicly document rate limits, so we use a conservative
    1 request per second to avoid issues.
    """

    def __init__(self, requests_per_second: float = 1.0):
        self.min_interval = 1.0 / requests_per_second
        self.last_request = 0.0
        self.lock = threading.Lock()

    def acquire(self):
        """Wait until we can make a request."""
        with self.lock:
            now = time.time()
            time_since_last = now - self.last_request
            if time_since_last < self.min_interval:
                sleep_time = self.min_interval - time_since_last
                time.sleep(sleep_time)
            self.last_request = time.time()


class SteamAPI:
    """Wrapper for Valve's Dota 2 Web API with rate limiting."""

    BASE_URL = "https://api.steampowered.com/IDOTA2Match_570"

    # Shared rate limiter across all instances
    _rate_limiter = None
    _rate_limiter_lock = threading.Lock()

    def __init__(self, api_key: str | None = None):
        """
        Initialize Steam API client.

        Args:
            api_key: Steam Web API key (required for all endpoints)
        """
        self.session = requests.Session()
        self.api_key = api_key or os.getenv("STEAM_API_KEY")

        if not self.api_key:
            logger.warning("No STEAM_API_KEY configured - Valve API calls will fail")

        # Initialize shared rate limiter
        with SteamAPI._rate_limiter_lock:
            if SteamAPI._rate_limiter is None:
                SteamAPI._rate_limiter = SteamAPIRateLimiter(requests_per_second=1.0)
                logger.info("Steam API rate limiter initialized: 1 request/second")

    def _make_request(self, endpoint: str, params: dict | None = None) -> dict | None:
        """
        Make a rate-limited request to the Steam API with retry on transient errors.

        Retries on 429 and 5xx responses using ``ENRICHMENT_RETRY_DELAYS`` for
        backoff. A 429 response with a ``Retry-After`` header longer than the
        configured backoff uses the server hint instead. 4xx (other than 429)
        and malformed-JSON responses are not retried. Successful responses are
        decoded through :func:`utils.http_safety.parse_json_bounded`, which
        enforces a post-hoc body-size ceiling and catches malformed JSON.

        Args:
            endpoint: API endpoint (e.g., "GetMatchDetails/v1")
            params: Query parameters

        Returns:
            Response JSON or None if error / retries exhausted.
        """
        if not self.api_key:
            logger.error("Cannot make Steam API request: no API key configured")
            return None

        # Wait for rate limiter
        SteamAPI._rate_limiter.acquire()

        url = f"{self.BASE_URL}/{endpoint}"
        params = params or {}
        params["key"] = self.api_key

        delays = list(ENRICHMENT_RETRY_DELAYS) or [0]
        for attempt in range(len(delays) + 1):
            try:
                response = self.session.get(url, params=params, timeout=_REQUEST_TIMEOUT)
            except requests.exceptions.RequestException as e:
                if attempt >= len(delays):
                    logger.error(f"Steam API request failed after retries: {e}")
                    return None
                delay = delays[attempt]
                logger.info(
                    f"Steam API request to {endpoint} failed ({e}); "
                    f"retrying in {delay}s (attempt {attempt + 1}/{len(delays)})"
                )
                time.sleep(delay)
                continue

            if response.status_code in _RETRYABLE_STATUS_CODES:
                if attempt >= len(delays):
                    logger.error(
                        f"Steam API request to {endpoint} exhausted retries "
                        f"(last status={response.status_code})"
                    )
                    return None
                delay = delays[attempt]
                # Honor an upstream-supplied Retry-After (common on 429). If it
                # is larger than our configured backoff, wait the longer
                # duration so we don't stomp on a rate-limit ban.
                if response.status_code == 429:
                    server_hint = retry_after_seconds(response)
                    if server_hint is not None:
                        delay = max(delay, server_hint)
                logger.info(
                    f"Steam API request to {endpoint} returned {response.status_code}; "
                    f"retrying in {delay}s (attempt {attempt + 1}/{len(delays)})"
                )
                time.sleep(delay)
                continue

            try:
                response.raise_for_status()
            except requests.exceptions.RequestException as e:
                logger.error(f"Steam API request failed: {e}")
                return None
            return parse_json_bounded(
                response, f"steam endpoint {endpoint}", max_bytes=_MAX_RESPONSE_BYTES
            )

        return None

    def get_match_details(self, match_id: int) -> dict | None:
        """
        Get detailed information about a specific match.

        Args:
            match_id: The Dota 2 match ID

        Returns:
            Match details dict or None if not found
        """
        logger.info(f"Fetching match details for match_id={match_id}")

        result = self._make_request("GetMatchDetails/v1", {"match_id": match_id})
        if not result:
            return None

        # Valve API wraps response in "result" key
        match_data = result.get("result")
        if not match_data:
            logger.warning(f"No result in response for match_id={match_id}")
            return None

        # Check for error
        if match_data.get("error"):
            logger.warning(f"Match {match_id} error: {match_data['error']}")
            return None

        return match_data

    def get_match_history(
        self,
        league_id: int | None = None,
        account_id: int | None = None,
        matches_requested: int = 25,
        start_at_match_id: int | None = None,
    ) -> dict | None:
        """
        Get match history, optionally filtered by league or player.

        Args:
            league_id: Filter by league ID
            account_id: Filter by player's 32-bit Steam ID
            matches_requested: Number of matches to return (max 100)
            start_at_match_id: For pagination, start after this match

        Returns:
            Match history dict with 'matches' array
        """
        params = {"matches_requested": min(matches_requested, 100)}

        if league_id:
            params["league_id"] = league_id
        if account_id:
            params["account_id"] = account_id
        if start_at_match_id:
            params["start_at_match_id"] = start_at_match_id

        logger.info(f"Fetching match history: {params}")

        result = self._make_request("GetMatchHistory/v1", params)
        if not result:
            return None

        return result.get("result")

    def get_league_listing(self) -> list[dict] | None:
        """
        Get list of all leagues.

        Returns:
            List of league dicts with leagueid, name, description
        """
        result = self._make_request("GetLeagueListing/v1")
        if not result:
            return None

        return result.get("result", {}).get("leagues", [])

    @staticmethod
    def decode_player_slot(player_slot: int) -> tuple:
        """
        Decode player_slot into team and position.

        The player_slot is an 8-bit value:
        - Bit 7 (0x80): Team (0 = Radiant, 1 = Dire)
        - Bits 0-2: Position within team (0-4)

        Args:
            player_slot: The player_slot value from API

        Returns:
            Tuple of (team: str, position: int)
            team is "radiant" or "dire"
            position is 0-4
        """
        team = "dire" if player_slot & 0x80 else "radiant"
        position = player_slot & 0x07
        return team, position

    @staticmethod
    def steam64_to_steam32(steam64: int) -> int:
        """Convert Steam64 ID to Steam32 ID (account_id)."""
        return steam64 - 76561197960265728

    @staticmethod
    def steam32_to_steam64(steam32: int) -> int:
        """Convert Steam32 ID (account_id) to Steam64 ID."""
        return steam32 + 76561197960265728


def test_steam_api():
    """Test Steam API integration."""
    api = SteamAPI()

    if not api.api_key:
        print("STEAM_API_KEY not configured")
        return

    # Test with a known public match
    match_id = 8181518332  # Example match ID
    print(f"Fetching match {match_id}...")

    match_data = api.get_match_details(match_id)
    if match_data:
        print(f"Match ID: {match_data.get('match_id')}")
        print(f"Duration: {match_data.get('duration')} seconds")
        print(f"Radiant Win: {match_data.get('radiant_win')}")
        print(f"Players: {len(match_data.get('players', []))}")

        for player in match_data.get("players", [])[:2]:
            team, pos = SteamAPI.decode_player_slot(player["player_slot"])
            print(
                f"  - Hero {player['hero_id']} ({team} pos {pos}): "
                f"{player['kills']}/{player['deaths']}/{player['assists']}"
            )
    else:
        print("Could not fetch match data")


if __name__ == "__main__":
    test_steam_api()
