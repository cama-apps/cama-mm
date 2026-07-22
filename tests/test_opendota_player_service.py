"""
Tests for OpenDotaPlayerService.
"""

import threading
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timedelta
from unittest.mock import Mock, patch

import pytest

import opendota_integration
import services.opendota_player_service as player_service_module
from opendota_integration import OpenDotaAPI
from services.opendota_player_service import CACHE_TTL_SECONDS, OpenDotaPlayerService
from tests.conftest import TEST_GUILD_ID

# =============================================================================
# Offline hardening (mirrors tests/test_opendota_http_hardening.py):
# a cache-miss regression must fail fast, not fall through to real HTTP
# plus retry sleeps.
# =============================================================================


@pytest.fixture(autouse=True)
def _reset_shared_rate_limiter():
    """Reset the shared singleton so tests don't share state."""
    OpenDotaAPI._rate_limiter = None
    yield
    OpenDotaAPI._rate_limiter = None


@pytest.fixture(autouse=True)
def _no_sleep(monkeypatch):
    """Retries call time.sleep; make it a no-op so tests stay fast."""
    monkeypatch.setattr(opendota_integration.time, "sleep", lambda _s: None)


@pytest.fixture(autouse=True)
def _fast_retry_delays(monkeypatch):
    """Shrink the configured retry delay list so any accidental retry path
    has a known, finite budget.
    """
    monkeypatch.setattr(opendota_integration, "ENRICHMENT_RETRY_DELAYS", [0, 0, 0])


@pytest.fixture(autouse=True)
def _block_real_http(monkeypatch):
    """Any real network attempt fails immediately instead of reaching OpenDota."""

    def _refuse(self, method, url, *args, **kwargs):
        raise AssertionError(f"Test attempted real HTTP request: {method} {url}")

    monkeypatch.setattr("requests.Session.request", _refuse)


class TestOpenDotaPlayerService:
    """Tests for OpenDotaPlayerService."""

    @pytest.fixture
    def mock_player_repo(self):
        """Create mock player repository."""
        repo = Mock()
        return repo

    def test_get_player_profile_no_steam_id(self, mock_player_repo):
        """Test profile fetch when player has no steam_id."""
        mock_player_repo.get_steam_id.return_value = None

        service = OpenDotaPlayerService(mock_player_repo)
        result = service.get_player_profile(discord_id=100)

        assert result is None

    def test_get_player_profile_cached(self, mock_player_repo):
        """Test profile fetch uses cache."""
        mock_player_repo.get_steam_id.return_value = 12345

        service = OpenDotaPlayerService(mock_player_repo)

        # Prime the cache
        service._memory_cache[100] = {
            "data": {"steam_id": 12345, "wins": 100, "losses": 50},
            "cached_at": datetime.now(),
        }

        result = service.get_player_profile(discord_id=100)

        assert result is not None
        assert result["steam_id"] == 12345
        assert result["wins"] == 100

    def test_get_player_profile_cache_expired(self, mock_player_repo):
        """Test profile fetch refreshes expired cache."""
        mock_player_repo.get_steam_id.return_value = 12345

        service = OpenDotaPlayerService(mock_player_repo)

        # Prime cache with expired entry
        service._memory_cache[100] = {
            "data": {"steam_id": 12345, "wins": 100},
            "cached_at": datetime.now() - timedelta(seconds=CACHE_TTL_SECONDS + 100),
        }

        # Mock the API fetch to return new data
        with patch.object(service, "_fetch_profile", return_value={"steam_id": 12345, "wins": 150}):
            result = service.get_player_profile(discord_id=100)

        assert result is not None
        assert result["wins"] == 150  # Should have new data

    def test_get_player_profile_force_refresh(self, mock_player_repo):
        """Test force_refresh bypasses cache."""
        mock_player_repo.get_steam_id.return_value = 12345

        service = OpenDotaPlayerService(mock_player_repo)

        # Prime cache with recent entry
        service._memory_cache[100] = {
            "data": {"steam_id": 12345, "wins": 100},
            "cached_at": datetime.now(),
        }

        with patch.object(service, "_fetch_profile", return_value={"steam_id": 12345, "wins": 200}):
            result = service.get_player_profile(discord_id=100, force_refresh=True)

        assert result["wins"] == 200  # Should have refreshed data

    def test_profile_cache_is_not_reused_after_steam_account_changes(
        self, mock_player_repo
    ):
        service = OpenDotaPlayerService(mock_player_repo)
        service._memory_cache[100] = {
            "data": {"steam_id": 111, "wins": 10},
            "cached_at": datetime.now(),
        }

        with patch.object(
            service,
            "_fetch_profile",
            return_value={"steam_id": 222, "wins": 20},
        ) as fetch_profile:
            result = service.get_player_profile(100, steam_id=222)

        assert result["steam_id"] == 222
        fetch_profile.assert_called_once_with(222, recent_matches=None)

    def test_concurrent_profile_cache_misses_share_one_refresh(
        self, mock_player_repo
    ):
        service = OpenDotaPlayerService(mock_player_repo)
        started = threading.Event()
        release = threading.Event()

        def slow_fetch(steam_id, *, recent_matches=None):
            started.set()
            assert release.wait(timeout=2)
            return {"steam_id": steam_id, "wins": 20}

        with patch.object(service, "_fetch_profile", side_effect=slow_fetch) as fetch:
            with ThreadPoolExecutor(max_workers=2) as executor:
                leader = executor.submit(
                    service.get_player_profile, 100, steam_id=12345
                )
                assert started.wait(timeout=2)
                follower = executor.submit(
                    service.get_player_profile, 100, steam_id=12345
                )
                release.set()
                futures = [leader, follower]
                results = [future.result() for future in futures]

        assert results == [
            {"steam_id": 12345, "wins": 20},
            {"steam_id": 12345, "wins": 20},
        ]
        assert fetch.call_count == 1

    def test_profile_components_are_fetched_in_parallel_and_reuse_match_sample(
        self, mock_player_repo
    ):
        service = OpenDotaPlayerService(mock_player_repo)
        active = 0
        max_active = 0
        lock = threading.Lock()
        all_started = threading.Barrier(4)

        def component(component_name, _steam_id):
            nonlocal active, max_active
            with lock:
                active += 1
                max_active = max(max_active, active)
            all_started.wait(timeout=2)
            with lock:
                active -= 1
            return {
                "player": {"profile": {"personaname": "Parallel"}},
                "wl": {"win": 2, "lose": 1},
                "totals": {"avg_kills": 7.5},
                "heroes": [{"hero_id": 1, "hero_name": "Anti-Mage"}],
            }[component_name]

        recent = [
            {
                "match_id": 77,
                "hero_id": 1,
                "kills": 8,
                "deaths": 2,
                "assists": 9,
                "player_slot": 0,
                "radiant_win": True,
            }
        ]
        with (
            patch.object(service, "_fetch_profile_component", side_effect=component),
            patch.object(service, "_fetch_recent_matches") as fetch_recent,
        ):
            profile = service._fetch_profile(12345, recent_matches=recent)

        assert max_active == 4
        fetch_recent.assert_not_called()
        assert profile["recent_matches"][0]["match_id"] == 77
        assert profile["last_match_id"] == 77

    def test_calc_win_rate(self, mock_player_repo):
        """Test win rate calculation."""
        service = OpenDotaPlayerService(mock_player_repo)

        assert service._calc_win_rate(50, 50) == 50.0
        assert service._calc_win_rate(75, 25) == 75.0
        assert service._calc_win_rate(0, 0) == 0.0
        assert service._calc_win_rate(100, 0) == 100.0

    def test_did_win_radiant(self, mock_player_repo):
        """Test win detection for radiant player."""
        service = OpenDotaPlayerService(mock_player_repo)

        # Radiant player (slot < 128) in radiant win
        assert service._did_win({"player_slot": 0, "radiant_win": True}) is True
        assert service._did_win({"player_slot": 0, "radiant_win": False}) is False

    def test_did_win_dire(self, mock_player_repo):
        """Test win detection for dire player."""
        service = OpenDotaPlayerService(mock_player_repo)

        # Dire player (slot >= 128) in dire win
        assert service._did_win({"player_slot": 128, "radiant_win": False}) is True
        assert service._did_win({"player_slot": 128, "radiant_win": True}) is False

    def test_format_profile_embed_no_profile(self, mock_player_repo):
        """Test format_profile_embed when profile unavailable."""
        mock_player_repo.get_steam_id.return_value = None

        service = OpenDotaPlayerService(mock_player_repo)
        result = service.format_profile_embed(discord_id=100, target_name="TestUser")

        assert result is None

    def test_format_profile_embed_success(self, mock_player_repo):
        """Test format_profile_embed returns proper structure."""
        mock_player_repo.get_steam_id.return_value = 12345

        service = OpenDotaPlayerService(mock_player_repo)

        # Mock the full profile
        with patch.object(
            service,
            "get_player_profile",
            return_value={
                "steam_id": 12345,
                "wins": 100,
                "losses": 50,
                "win_rate": 66.7,
                "avg_kills": 8.5,
                "avg_deaths": 5.2,
                "avg_assists": 12.0,
                "avg_gpm": 500,
                "avg_xpm": 550,
                "top_heroes": [
                    {"hero_name": "Pudge", "games": 50, "win_rate": 60.0},
                ],
                "recent_matches": [
                    {"hero_name": "Pudge", "kills": 10, "deaths": 3, "assists": 8, "won": True},
                ],
                "last_match_id": 8181518332,
            },
        ):
            result = service.format_profile_embed(discord_id=100, target_name="TestUser")

        assert result is not None
        assert result["title"] == "Profile: TestUser"
        assert len(result["fields"]) >= 4
        assert result["last_match_id"] == 8181518332


class TestPlayerRepositorySteamId:
    """Tests for PlayerRepository steam_id methods."""

    @pytest.fixture
    def temp_db_path(self, tmp_path):
        """Create temporary database."""
        return str(tmp_path / "test.db")

    def test_set_and_get_steam_id(self, temp_db_path):
        """Test setting and getting steam_id."""
        from infrastructure.schema_manager import SchemaManager
        from repositories.player_repository import PlayerRepository

        SchemaManager(temp_db_path).initialize()
        repo = PlayerRepository(temp_db_path)

        # Add a player
        repo.add(discord_id=100, discord_username="TestUser", guild_id=TEST_GUILD_ID)

        # Initially no steam_id
        assert repo.get_steam_id(100) is None

        # Set steam_id
        repo.set_steam_id(100, 12345678)

        # Now should have steam_id
        assert repo.get_steam_id(100) == 12345678

    def test_get_by_steam_id(self, temp_db_path):
        """Test finding player by steam_id."""
        from infrastructure.schema_manager import SchemaManager
        from repositories.player_repository import PlayerRepository

        SchemaManager(temp_db_path).initialize()
        repo = PlayerRepository(temp_db_path)

        repo.add(discord_id=100, discord_username="TestUser", guild_id=TEST_GUILD_ID)
        repo.set_steam_id(100, 12345678)

        player = repo.get_by_steam_id(12345678, guild_id=TEST_GUILD_ID)
        assert player is not None
        assert player.discord_id == 100
        assert player.name == "TestUser"

    def test_get_by_steam_id_not_found(self, temp_db_path):
        """Test finding player by steam_id that doesn't exist."""
        from infrastructure.schema_manager import SchemaManager
        from repositories.player_repository import PlayerRepository

        SchemaManager(temp_db_path).initialize()
        repo = PlayerRepository(temp_db_path)

        player = repo.get_by_steam_id(99999999, guild_id=TEST_GUILD_ID)
        assert player is None

    def test_get_all_with_dotabuff_no_steam_id(self, temp_db_path):
        """Test getting players needing steam_id backfill."""
        from infrastructure.schema_manager import SchemaManager
        from repositories.player_repository import PlayerRepository

        SchemaManager(temp_db_path).initialize()
        repo = PlayerRepository(temp_db_path)

        # Add players with various states
        repo.add(
            discord_id=100,
            discord_username="HasBoth",
            guild_id=TEST_GUILD_ID,
            dotabuff_url="https://dotabuff.com/players/123",
        )
        repo.set_steam_id(100, 12345)

        repo.add(
            discord_id=101,
            discord_username="NeedsSteamId",
            guild_id=TEST_GUILD_ID,
            dotabuff_url="https://dotabuff.com/players/456",
        )
        # No steam_id set

        repo.add(discord_id=102, discord_username="NoDotabuff", guild_id=TEST_GUILD_ID)
        # No dotabuff_url

        needs_backfill = repo.get_all_with_dotabuff_no_steam_id()
        assert len(needs_backfill) == 1
        assert needs_backfill[0]["discord_id"] == 101


class TestDistributionCalculations:
    """Tests for hero attribute and lane distribution calculations."""

    @pytest.fixture
    def mock_player_repo(self):
        """Create mock player repository."""
        repo = Mock()
        return repo

    def test_calc_attribute_distribution_empty(self, mock_player_repo):
        """Test attribute distribution with no matches."""
        service = OpenDotaPlayerService(mock_player_repo)
        result = service._calc_attribute_distribution([])

        assert result == {"str": 0, "agi": 0, "int": 0, "all": 0}

    def test_calc_attribute_distribution_with_data(self, mock_player_repo):
        """Test attribute distribution calculation."""
        service = OpenDotaPlayerService(mock_player_repo)

        # Mock hero attributes (normally fetched from API)
        with patch.object(
            service,
            "_get_hero_attributes",
            return_value={
                1: "agi",  # Anti-Mage
                2: "str",  # Axe
                3: "int",  # Bane
                138: "all",  # Primal Beast (Universal)
            },
        ):
            matches = [
                {"hero_id": 1},  # agi
                {"hero_id": 1},  # agi
                {"hero_id": 2},  # str
                {"hero_id": 3},  # int
                {"hero_id": 138},  # all
            ]
            result = service._calc_attribute_distribution(matches)

        assert result["agi"] == 40.0  # 2/5 = 40%
        assert result["str"] == 20.0  # 1/5 = 20%
        assert result["int"] == 20.0  # 1/5 = 20%
        assert result["all"] == 20.0  # 1/5 = 20%

    def test_calc_lane_distribution_empty(self, mock_player_repo):
        """Test lane distribution with no matches."""
        service = OpenDotaPlayerService(mock_player_repo)
        result, count = service._calc_lane_distribution([])

        assert result["Safe Lane"] == 0
        assert result["Mid"] == 0
        assert result["Off Lane"] == 0
        assert result["Jungle"] == 0
        assert result["Roaming"] == 0
        assert count == 0

    def test_calc_lane_distribution_with_data(self, mock_player_repo):
        """Test lane distribution calculation."""
        service = OpenDotaPlayerService(mock_player_repo)

        matches = [
            {"lane_role": 1},  # Safe Lane
            {"lane_role": 1},  # Safe Lane
            {"lane_role": 2},  # Mid
            {"lane_role": 3},  # Off Lane
            {"lane_role": None},  # Unknown - should be skipped
        ]
        result, count = service._calc_lane_distribution(matches)

        assert result["Safe Lane"] == 50.0  # 2/4 = 50%
        assert result["Mid"] == 25.0  # 1/4 = 25%
        assert result["Off Lane"] == 25.0  # 1/4 = 25%
        assert result["Jungle"] == 0  # 0/4 = 0%
        assert result["Roaming"] == 0  # 0/4 = 0%
        assert count == 4

    def test_calc_lane_distribution_with_roaming(self, mock_player_repo):
        """Test lane distribution includes roaming (lane_role=0)."""
        service = OpenDotaPlayerService(mock_player_repo)

        matches = [
            {"lane_role": 0},  # Roaming
            {"lane_role": 0},  # Roaming
            {"lane_role": 1},  # Safe Lane
            {"lane_role": 2},  # Mid
        ]
        result, count = service._calc_lane_distribution(matches)

        assert result["Roaming"] == 50.0  # 2/4 = 50%
        assert result["Safe Lane"] == 25.0  # 1/4 = 25%
        assert result["Mid"] == 25.0  # 1/4 = 25%
        assert count == 4

    def test_dotabase_role_weights_use_bundled_hero_data(self, mock_player_repo):
        service = OpenDotaPlayerService(mock_player_repo)

        roles = service._get_hero_roles()

        assert len(roles) > 100
        assert roles[1] == {"Carry": 3, "Escape": 3, "Nuker": 1}
        assert roles[5] == {"Support": 3, "Disabler": 2, "Nuker": 2}

    def test_get_full_stats_no_steam_id(self, mock_player_repo):
        """Test get_full_stats when player has no steam_id."""
        mock_player_repo.get_steam_id.return_value = None

        service = OpenDotaPlayerService(mock_player_repo)
        result = service.get_full_stats(discord_id=100)

        assert result is None

    def test_get_full_stats_success(self, mock_player_repo):
        """Test get_full_stats returns complete data structure."""
        mock_player_repo.get_steam_id.return_value = 12345

        service = OpenDotaPlayerService(mock_player_repo)

        # Mock all the dependencies
        with patch.object(
            service,
            "get_player_profile",
            return_value={
                "persona_name": "TestPlayer",
                "rank_tier": 55,
                "mmr_estimate": 4500,
                "wins": 500,
                "losses": 400,
                "win_rate": 55.6,
                "avg_kills": 8.0,
                "avg_deaths": 5.0,
                "avg_assists": 10.0,
                "avg_gpm": 450,
                "avg_xpm": 500,
                "avg_last_hits": 150,
                "top_heroes": [{"hero_name": "Pudge", "games": 100, "win_rate": 55.0}],
            },
        ):
            with patch.object(
                service,
                "_fetch_matches_for_stats",
                return_value=[
                    {"hero_id": 1, "lane_role": 1, "player_slot": 0, "radiant_win": True},
                    {"hero_id": 2, "lane_role": 2, "player_slot": 0, "radiant_win": False},
                ],
            ):
                with patch.object(
                    service,
                    "_calc_attribute_distribution",
                    return_value={"str": 50.0, "agi": 50.0, "int": 0, "all": 0},
                ):
                    with patch.object(
                        service,
                        "_calc_lane_distribution",
                        return_value=(
                            {"Safe Lane": 50.0, "Mid": 50.0, "Off Lane": 0, "Jungle": 0, "Roaming": 0},
                            2,  # parsed count
                        ),
                    ):
                        result = service.get_full_stats(discord_id=100)

        assert result is not None
        assert result["steam_id"] == 12345
        assert result["total_wins"] == 500
        assert result["total_losses"] == 400
        assert result["attribute_distribution"]["str"] == 50.0
        assert result["lane_distribution"]["Safe Lane"] == 50.0
        assert result["lane_parsed_count"] == 2
        assert len(result["top_heroes"]) == 1

    def test_get_dota_tab_stats_reuses_matches_for_roles_and_lanes(
        self, mock_player_repo
    ):
        service = OpenDotaPlayerService(mock_player_repo)
        steam_id = 12345
        matches = [
            {"hero_id": 1, "lane_role": 1, "player_slot": 0, "radiant_win": True},
            {"hero_id": 1, "lane_role": 1, "player_slot": 0, "radiant_win": False},
            {"hero_id": 5, "lane_role": 2, "player_slot": 128, "radiant_win": False},
        ]

        with (
            patch.object(
                service,
                "_fetch_matches_for_stats",
                return_value=matches,
            ) as fetch_matches,
            patch.object(
                service,
                "_fetch_profile",
                return_value={"wins": 20, "losses": 10, "win_rate": 66.7},
            ) as fetch_profile,
            patch.object(
                service,
                "_get_hero_roles",
                return_value={
                    1: {"Carry": 3, "Escape": 1},
                    5: {"Support": 2},
                },
            ),
            patch.object(
                service,
                "_get_hero_attributes",
                return_value={1: "agi", 5: "int"},
            ),
        ):
            result = service.get_dota_tab_stats(
                discord_id=100,
                match_limit=50,
                steam_id=steam_id,
            )

        mock_player_repo.get_steam_id.assert_not_called()
        fetch_matches.assert_called_once_with(steam_id, limit=50)
        fetch_profile.assert_called_once_with(steam_id, recent_matches=matches)
        assert result["role_distribution"] == {
            "Carry": 60.0,
            "Escape": 20.0,
            "Support": 20.0,
        }
        assert result["full_stats"]["lane_distribution"]["Safe Lane"] == 66.7
        assert result["full_stats"]["lane_distribution"]["Mid"] == 33.3
        assert result["full_stats"]["lane_parsed_count"] == 3

    def test_get_dota_tab_stats_caches_complete_result(self, mock_player_repo):
        service = OpenDotaPlayerService(mock_player_repo)
        matches = [{"hero_id": 1, "lane_role": 1}]
        full_stats = {"matches_analyzed": 1}

        with (
            patch.object(
                service, "_fetch_matches_for_stats", return_value=matches
            ) as fetch_matches,
            patch.object(
                service,
                "_calc_hero_role_distribution",
                return_value={"Carry": 100.0},
            ),
            patch.object(
                service,
                "get_player_profile",
                return_value={"steam_id": 12345},
            ) as get_profile,
            patch.object(service, "_build_full_stats", return_value=full_stats),
        ):
            first = service.get_dota_tab_stats(100, steam_id=12345)
            second = service.get_dota_tab_stats(100, steam_id=12345)

        assert first is second
        assert first["full_stats"] == full_stats
        fetch_matches.assert_called_once_with(12345, limit=50)
        get_profile.assert_called_once()

    def test_hero_attributes_use_bundled_data_without_http(self, mock_player_repo):
        service = OpenDotaPlayerService(mock_player_repo)
        heroes = [
            type("Hero", (), {"id": 1, "attr_primary": "agility"})(),
            type("Hero", (), {"id": 2, "attr_primary": "universal"})(),
        ]
        original = player_service_module._HERO_ATTRIBUTES_CACHE
        player_service_module._HERO_ATTRIBUTES_CACHE = None
        try:
            with (
                patch("services.trivia_data.load_heroes", return_value=heroes),
                patch.object(service.api, "make_request") as make_request,
            ):
                attributes = service._get_hero_attributes()
                make_request.assert_not_called()
        finally:
            player_service_module._HERO_ATTRIBUTES_CACHE = original

        assert attributes == {1: "agi", 2: "all"}

    def test_get_dota_tab_stats_preserves_roles_when_profile_is_unavailable(
        self, mock_player_repo
    ):
        mock_player_repo.get_steam_id.return_value = 12345
        service = OpenDotaPlayerService(mock_player_repo)
        matches = [
            {"hero_id": 5, "lane_role": 1},
            {"hero_id": 5, "lane_role": 2},
        ]

        with (
            patch.object(
                service,
                "_fetch_matches_for_stats",
                return_value=matches,
            ) as fetch_matches,
            patch.object(service, "_fetch_profile", return_value=None),
            patch.object(
                service,
                "_get_hero_roles",
                return_value={5: {"Support": 3, "Nuker": 1}},
            ),
        ):
            result = service.get_dota_tab_stats(discord_id=100, match_limit=50)

        mock_player_repo.get_steam_id.assert_called_once_with(100)
        fetch_matches.assert_called_once_with(12345, limit=50)
        assert result == {
            "role_distribution": {"Support": 75.0, "Nuker": 25.0},
            "full_stats": None,
        }
