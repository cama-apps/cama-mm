"""
Tests for Branch E HTTP hardening in opendota_integration.py and steam_api.py.

Covers:
- timeout propagation on outgoing requests
- retry/backoff on 429 / 5xx with configured delay list
- no retry on non-429 4xx
- graceful handling of malformed JSON
- player-matches response size cap
"""

from __future__ import annotations

from unittest.mock import MagicMock, patch

import pytest
import requests

import opendota_integration
import steam_api
from opendota_integration import OpenDotaAPI
from steam_api import SteamAPI

# =============================================================================
# Helpers
# =============================================================================


def _make_response(status_code: int, *, json_body=None, text: str | None = None,
                   content_length: str | None = None, retry_after: str | None = None):
    """Build a lightweight fake requests.Response suitable for session.get mocks."""
    resp = MagicMock(spec=requests.Response)
    resp.status_code = status_code
    if text is None and json_body is not None:
        import json as _json
        text = _json.dumps(json_body)
    if text is None:
        text = ""
    resp.text = text
    resp.content = text.encode("utf-8")
    headers = {}
    if content_length is not None:
        headers["Content-Length"] = content_length
    if retry_after is not None:
        headers["Retry-After"] = retry_after
    resp.headers = headers

    if json_body is not None:
        resp.json.return_value = json_body
    else:
        resp.json.side_effect = requests.exceptions.JSONDecodeError(
            "Expecting value", text, 0
        )

    def _raise_for_status():
        if 400 <= status_code < 600:
            raise requests.exceptions.HTTPError(f"{status_code} error")

    resp.raise_for_status.side_effect = _raise_for_status
    return resp


@pytest.fixture(autouse=True)
def _reset_shared_rate_limiters():
    """Reset shared singletons so tests don't share state."""
    OpenDotaAPI._rate_limiter = None
    SteamAPI._rate_limiter = None
    yield
    OpenDotaAPI._rate_limiter = None
    SteamAPI._rate_limiter = None


@pytest.fixture(autouse=True)
def _no_sleep(monkeypatch):
    """Retries call time.sleep; make it a no-op so tests stay fast."""
    monkeypatch.setattr(opendota_integration.time, "sleep", lambda _s: None)
    monkeypatch.setattr(steam_api.time, "sleep", lambda _s: None)


@pytest.fixture(autouse=True)
def _fast_retry_delays(monkeypatch):
    """Shrink the configured retry delay list so tests exercise retries quickly
    but have a known, finite budget.
    """
    monkeypatch.setattr(opendota_integration, "ENRICHMENT_RETRY_DELAYS", [0, 0, 0])
    monkeypatch.setattr(steam_api, "ENRICHMENT_RETRY_DELAYS", [0, 0, 0])


# =============================================================================
# OpenDota tests
# =============================================================================


class TestOpenDotaTimeout:
    """Verify HTTP calls always pass a timeout parameter."""

    def test_get_player_data_passes_timeout(self):
        api = OpenDotaAPI(api_key="test")
        captured: dict = {}

        def _get(url, params=None, timeout=None):
            captured["timeout"] = timeout
            return _make_response(200, json_body={"profile": {}})

        with patch.object(api.session, "get", side_effect=_get):
            api.get_player_data(12345)

        assert captured["timeout"] is not None
        assert captured["timeout"] >= 5

    def test_get_match_details_passes_timeout(self):
        api = OpenDotaAPI(api_key="test")
        captured: dict = {}

        def _get(url, params=None, timeout=None):
            captured["timeout"] = timeout
            return _make_response(200, json_body={"match_id": 123})

        with patch.object(api.session, "get", side_effect=_get):
            api.get_match_details(123)

        assert captured["timeout"] is not None


class TestOpenDotaRetryBehavior:
    """Verify retries on 429/5xx, no retry on 403."""

    def test_retries_on_429_then_succeeds(self):
        api = OpenDotaAPI(api_key="test")
        responses = [
            _make_response(429),
            _make_response(429),
            _make_response(200, json_body={"profile": {"personaname": "pf"}}),
        ]
        call_count = {"n": 0}

        def _get(url, params=None, timeout=None):
            idx = call_count["n"]
            call_count["n"] += 1
            return responses[min(idx, len(responses) - 1)]

        with patch.object(api.session, "get", side_effect=_get):
            result = api.get_player_data(12345)

        assert result == {"profile": {"personaname": "pf"}}
        # 2 retries = 3 total attempts
        assert call_count["n"] == 3

    def test_retries_on_503_then_succeeds(self):
        api = OpenDotaAPI(api_key="test")
        responses = [
            _make_response(503),
            _make_response(200, json_body={"match_id": 42}),
        ]
        call_count = {"n": 0}

        def _get(url, params=None, timeout=None):
            idx = call_count["n"]
            call_count["n"] += 1
            return responses[min(idx, len(responses) - 1)]

        with patch.object(api.session, "get", side_effect=_get):
            result = api.get_match_details(42)

        assert result is not None
        assert call_count["n"] == 2

    def test_no_retry_on_403(self):
        """4xx other than 429 should not retry — a single call is made."""
        api = OpenDotaAPI(api_key="test")
        call_count = {"n": 0}

        def _get(url, params=None, timeout=None):
            call_count["n"] += 1
            return _make_response(403)

        with patch.object(api.session, "get", side_effect=_get):
            result = api.get_player_data(12345)

        assert result is None
        assert call_count["n"] == 1

    def test_retry_exhausted_returns_none(self):
        """After exhausting the configured delay list, stops retrying."""
        api = OpenDotaAPI(api_key="test")
        call_count = {"n": 0}

        def _get(url, params=None, timeout=None):
            call_count["n"] += 1
            return _make_response(429)

        with patch.object(api.session, "get", side_effect=_get):
            result = api.get_player_data(12345)

        assert result is None
        # len([0, 0, 0]) retries + 1 initial = 4 attempts total
        assert call_count["n"] == 4


class TestOpenDotaMalformedJSON:
    """Verify JSONDecodeError is caught and surfaced as None."""

    def test_get_player_data_malformed_json_returns_none(self):
        api = OpenDotaAPI(api_key="test")
        bad = _make_response(200, text="<html>not json</html>")

        with patch.object(api.session, "get", return_value=bad):
            result = api.get_player_data(12345)

        assert result is None

    def test_get_match_details_malformed_json_returns_none(self):
        api = OpenDotaAPI(api_key="test")
        bad = _make_response(200, text="not-json-body")

        with patch.object(api.session, "get", return_value=bad):
            result = api.get_match_details(42)

        assert result is None


class TestOpenDotaResponseSizeCap:
    """Verify oversize responses are rejected."""

    def test_get_player_matches_rejects_oversized_content_length(self):
        api = OpenDotaAPI(api_key="test")
        huge = _make_response(
            200,
            json_body=[{"match_id": 1}],
            content_length=str(10 * 1024 * 1024),  # 10 MB > 5 MB cap
        )

        with patch.object(api.session, "get", return_value=huge):
            result = api.get_player_matches(12345, limit=20)

        assert result is None

    def test_get_player_matches_passes_limit_param(self):
        api = OpenDotaAPI(api_key="test")
        captured: dict = {}

        def _get(url, params=None, timeout=None):
            captured["params"] = params
            return _make_response(200, json_body=[])

        with patch.object(api.session, "get", side_effect=_get):
            api.get_player_matches(12345, limit=20)

        assert captured["params"]["limit"] == 20


# =============================================================================
# Steam API tests
# =============================================================================


class TestSteamAPIHardening:
    """Timeout + retry + malformed JSON for Steam API."""

    def test_steam_passes_timeout(self):
        api = SteamAPI(api_key="test_key")
        captured: dict = {}

        def _get(url, params=None, timeout=None):
            captured["timeout"] = timeout
            return _make_response(200, json_body={"result": {"match_id": 1}})

        with patch.object(api.session, "get", side_effect=_get):
            api.get_match_details(1)

        assert captured["timeout"] is not None
        assert captured["timeout"] >= 5

    def test_steam_retries_on_429_then_succeeds(self):
        api = SteamAPI(api_key="test_key")
        responses = [
            _make_response(429),
            _make_response(200, json_body={"result": {"match_id": 42}}),
        ]
        call_count = {"n": 0}

        def _get(url, params=None, timeout=None):
            idx = call_count["n"]
            call_count["n"] += 1
            return responses[min(idx, len(responses) - 1)]

        with patch.object(api.session, "get", side_effect=_get):
            result = api.get_match_details(42)

        assert result == {"match_id": 42}
        assert call_count["n"] == 2

    def test_steam_no_retry_on_403(self):
        api = SteamAPI(api_key="test_key")
        call_count = {"n": 0}

        def _get(url, params=None, timeout=None):
            call_count["n"] += 1
            return _make_response(403)

        with patch.object(api.session, "get", side_effect=_get):
            result = api.get_match_details(42)

        assert result is None
        assert call_count["n"] == 1

    def test_steam_malformed_json_returns_none(self):
        api = SteamAPI(api_key="test_key")
        bad = _make_response(200, text="notjson")

        with patch.object(api.session, "get", return_value=bad):
            result = api.get_match_details(42)

        assert result is None

    def test_steam_retry_exhausted_returns_none(self):
        api = SteamAPI(api_key="test_key")
        call_count = {"n": 0}

        def _get(url, params=None, timeout=None):
            call_count["n"] += 1
            return _make_response(500)

        with patch.object(api.session, "get", side_effect=_get):
            result = api.get_match_details(42)

        assert result is None
        # 3 configured retries + 1 initial = 4 attempts
        assert call_count["n"] == 4

    def test_steam_rejects_oversized_content_length(self):
        """Steam endpoints should share the same 5 MB body cap as OpenDota."""
        api = SteamAPI(api_key="test_key")
        huge = _make_response(
            200,
            json_body={"result": {"match_id": 1}},
            content_length=str(10 * 1024 * 1024),  # 10 MB > 5 MB cap
        )

        with patch.object(api.session, "get", return_value=huge):
            result = api.get_match_details(1)

        assert result is None


# =============================================================================
# Retry-After honoring (shared helper)
# =============================================================================


class TestRetryAfterHonoring:
    """Verify Retry-After is honored when it exceeds the configured backoff."""

    def test_opendota_honors_retry_after_larger_than_delay(self, monkeypatch):
        """When Retry-After > configured delay, sleep for Retry-After."""
        api = OpenDotaAPI(api_key="test")
        sleeps: list[float] = []

        # Capture actual sleep requests from the OpenDota retry loop.
        monkeypatch.setattr(
            opendota_integration.time,
            "sleep",
            lambda s: sleeps.append(s),
        )

        responses = [
            _make_response(429, retry_after="60"),
            _make_response(200, json_body={"profile": {"personaname": "pf"}}),
        ]
        call_count = {"n": 0}

        def _get(url, params=None, timeout=None):
            idx = call_count["n"]
            call_count["n"] += 1
            return responses[min(idx, len(responses) - 1)]

        with patch.object(api.session, "get", side_effect=_get):
            result = api.get_player_data(12345)

        assert result == {"profile": {"personaname": "pf"}}
        # The _fast_retry_delays fixture sets delays to [0, 0, 0]; a 60s
        # Retry-After should win and we should sleep for 60s exactly once.
        assert 60 in sleeps

    def test_opendota_ignores_retry_after_smaller_than_delay(self, monkeypatch):
        """A tiny Retry-After shouldn't shorten our own backoff below it."""
        api = OpenDotaAPI(api_key="test")

        # Override delays to a non-trivial value so Retry-After: 1 doesn't win.
        monkeypatch.setattr(opendota_integration, "ENRICHMENT_RETRY_DELAYS", [30, 30, 30])
        sleeps: list[float] = []
        monkeypatch.setattr(
            opendota_integration.time,
            "sleep",
            lambda s: sleeps.append(s),
        )

        responses = [
            _make_response(429, retry_after="1"),
            _make_response(200, json_body={"profile": {}}),
        ]
        call_count = {"n": 0}

        def _get(url, params=None, timeout=None):
            idx = call_count["n"]
            call_count["n"] += 1
            return responses[min(idx, len(responses) - 1)]

        with patch.object(api.session, "get", side_effect=_get):
            api.get_player_data(12345)

        # We should not have slept for 1s; the configured 30s delay wins.
        assert 1 not in sleeps
        assert 30 in sleeps

    def test_opendota_tolerates_malformed_retry_after(self, monkeypatch):
        """An unparseable Retry-After falls back to configured delay."""
        api = OpenDotaAPI(api_key="test")
        sleeps: list[float] = []
        monkeypatch.setattr(
            opendota_integration.time,
            "sleep",
            lambda s: sleeps.append(s),
        )

        responses = [
            _make_response(429, retry_after="not-a-number"),
            _make_response(200, json_body={"profile": {}}),
        ]
        call_count = {"n": 0}

        def _get(url, params=None, timeout=None):
            idx = call_count["n"]
            call_count["n"] += 1
            return responses[min(idx, len(responses) - 1)]

        with patch.object(api.session, "get", side_effect=_get):
            result = api.get_player_data(12345)

        # Falls back to configured backoff (0 from the fixture) and still
        # succeeds on the retry.
        assert result == {"profile": {}}

    def test_steam_honors_retry_after_larger_than_delay(self, monkeypatch):
        """Steam should apply the same Retry-After preference as OpenDota."""
        api = SteamAPI(api_key="test_key")
        sleeps: list[float] = []
        monkeypatch.setattr(
            steam_api.time,
            "sleep",
            lambda s: sleeps.append(s),
        )

        responses = [
            _make_response(429, retry_after="60"),
            _make_response(200, json_body={"result": {"match_id": 42}}),
        ]
        call_count = {"n": 0}

        def _get(url, params=None, timeout=None):
            idx = call_count["n"]
            call_count["n"] += 1
            return responses[min(idx, len(responses) - 1)]

        with patch.object(api.session, "get", side_effect=_get):
            result = api.get_match_details(42)

        assert result == {"match_id": 42}
        assert 60 in sleeps


class TestOpenDotaGetPlayerRolesNoneCheck:
    """Verify get_player_roles propagates None from the JSON parse helper."""

    def test_get_player_roles_returns_none_on_malformed_json(self):
        api = OpenDotaAPI(api_key="test")
        bad = _make_response(200, text="<html>not json</html>")

        with patch.object(api.session, "get", return_value=bad):
            result = api.get_player_roles(12345)

        assert result is None

    def test_get_player_roles_returns_none_on_oversized_body(self):
        api = OpenDotaAPI(api_key="test")
        huge = _make_response(
            200,
            json_body=[{"hero_id": 1}],
            content_length=str(10 * 1024 * 1024),
        )

        with patch.object(api.session, "get", return_value=huge):
            result = api.get_player_roles(12345)

        assert result is None
