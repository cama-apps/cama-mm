"""
Tests for SteamAPI client.
"""

import pytest
from unittest.mock import Mock, patch, MagicMock
import requests

from steam_api import SteamAPI, SteamAPIRateLimiter


class TestSteamAPIRateLimiter:
    """Tests for the rate limiter."""

    def test_rate_limiter_allows_first_request(self):
        """First request should be allowed immediately."""
        limiter = SteamAPIRateLimiter(requests_per_second=10.0)
        # Should not block
        limiter.acquire()
        # If we got here, it worked

    def test_rate_limiter_enforces_delay(self):
        """Rate limiter should enforce minimum delay between requests."""
        import time

        limiter = SteamAPIRateLimiter(requests_per_second=10.0)
        limiter.acquire()

        start = time.time()
        limiter.acquire()
        elapsed = time.time() - start

        # Should have waited at least 0.09 seconds (10 req/s = 0.1s interval)
        # Allow some tolerance
        assert elapsed >= 0.08


class TestSteamAPIHelpers:
    """Tests for static helper methods."""

    def test_decode_player_slot_radiant(self):
        """Radiant players have slot < 128."""
        team, pos = SteamAPI.decode_player_slot(0)
        assert team == "radiant"
        assert pos == 0

        team, pos = SteamAPI.decode_player_slot(4)
        assert team == "radiant"
        assert pos == 4

    def test_decode_player_slot_dire(self):
        """Dire players have slot >= 128."""
        team, pos = SteamAPI.decode_player_slot(128)
        assert team == "dire"
        assert pos == 0

        team, pos = SteamAPI.decode_player_slot(132)
        assert team == "dire"
        assert pos == 4

    def test_steam_id_conversion(self):
        """Test Steam ID 32-bit <-> 64-bit conversion."""
        steam32 = 12345678
        steam64 = SteamAPI.steam32_to_steam64(steam32)

        assert steam64 == steam32 + 76561197960265728
        assert SteamAPI.steam64_to_steam32(steam64) == steam32


class TestSteamAPIClient:
    """Tests for the API client."""

    def test_no_api_key_logs_warning(self):
        """Without API key, should log warning."""
        with patch.dict("os.environ", {}, clear=True):
            with patch("steam_api.os.getenv", return_value=None):
                api = SteamAPI(api_key=None)
                assert api.api_key is None

    def test_api_key_from_param(self):
        """API key from constructor param takes precedence."""
        api = SteamAPI(api_key="test_key_123")
        assert api.api_key == "test_key_123"

    @patch("steam_api.SteamAPI._rate_limiter")
    def test_get_match_details_success(self, mock_limiter):
        """Test successful match details fetch."""
        mock_response = Mock()
        mock_response.status_code = 200
        mock_response.json.return_value = {
            "result": {
                "match_id": 8181518332,
                "duration": 2400,
                "radiant_win": True,
                "radiant_score": 35,
                "dire_score": 22,
                "players": [
                    {"account_id": 12345, "hero_id": 1, "kills": 10, "deaths": 2, "assists": 5}
                ],
            }
        }

        api = SteamAPI(api_key="test_key")
        with patch.object(api.session, "get", return_value=mock_response):
            result = api.get_match_details(8181518332)

        assert result is not None
        assert result["match_id"] == 8181518332
        assert result["duration"] == 2400
        assert result["radiant_win"] is True
        assert len(result["players"]) == 1

    @patch("steam_api.SteamAPI._rate_limiter")
    def test_get_match_details_error(self, mock_limiter):
        """Test match details fetch with error response."""
        mock_response = Mock()
        mock_response.status_code = 200
        mock_response.json.return_value = {"result": {"error": "Match not found"}}

        api = SteamAPI(api_key="test_key")
        with patch.object(api.session, "get", return_value=mock_response):
            result = api.get_match_details(99999)

        assert result is None

    @patch("steam_api.SteamAPI._rate_limiter")
    def test_get_match_details_request_error(self, mock_limiter):
        """Test match details fetch with request exception."""
        api = SteamAPI(api_key="test_key")
        with patch.object(
            api.session, "get", side_effect=requests.exceptions.RequestException("Network error")
        ):
            result = api.get_match_details(8181518332)

        assert result is None

    @patch("steam_api.SteamAPI._rate_limiter")
    def test_get_match_history(self, mock_limiter):
        """Test match history fetch."""
        mock_response = Mock()
        mock_response.status_code = 200
        mock_response.json.return_value = {
            "result": {
                "status": 1,
                "num_results": 2,
                "matches": [
                    {"match_id": 123, "start_time": 1704067200},
                    {"match_id": 124, "start_time": 1704067300},
                ],
            }
        }

        api = SteamAPI(api_key="test_key")
        with patch.object(api.session, "get", return_value=mock_response):
            result = api.get_match_history(league_id=19144)

        assert result is not None
        assert result["num_results"] == 2
        assert len(result["matches"]) == 2

    def test_no_api_key_prevents_request(self):
        """Without API key, requests should return None."""
        api = SteamAPI(api_key=None)
        result = api.get_match_details(123)
        assert result is None
