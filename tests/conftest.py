"""
Pytest fixtures for tests.

Performance optimization: Uses session-scoped schema template to avoid running
56 database migrations for every test. Instead, we run migrations once and copy
the resulting database file (~1ms) instead of re-initializing (~50ms+).

This module provides centralized constants and fixtures to reduce duplication
across the test suite. Import TEST_GUILD_ID from here instead of defining it locally.
"""

import random
import shutil

import pytest

from database import Database
from domain.models.player import Player
from repositories.bet_repository import BetRepository
from repositories.guild_config_repository import GuildConfigRepository
from repositories.lobby_repository import LobbyRepository
from repositories.match_repository import MatchRepository
from repositories.pairings_repository import PairingsRepository
from repositories.player_repository import PlayerRepository
from services.betting_service import BettingService
from services.garnishment_service import GarnishmentService
from services.guild_config_service import GuildConfigService
from services.lobby_manager_service import LobbyManagerService as LobbyManager
from services.match_service import MatchService

# =============================================================================
# CENTRALIZED CONSTANTS
# =============================================================================
# Use these instead of defining TEST_GUILD_ID locally in each test file.
# This ensures consistency across all tests.

TEST_GUILD_ID = 12345
"""Standard guild ID for single-guild tests. Import and use this constant."""

TEST_GUILD_ID_SECONDARY = 67890
"""Secondary guild ID for multi-guild isolation tests."""




@pytest.fixture(autouse=True)
def _isolate_random_state():
    """
    Isolate tests from ``random.seed()`` calls leaking across test boundaries.

    Some tests seed ``random`` for deterministic behavior. Without this fixture
    the seeded state bleeds into subsequent tests (especially under
    pytest-xdist), producing order-dependent pass/fail results.
    """
    state = random.getstate()
    yield
    random.setstate(state)


@pytest.fixture(autouse=True)
def _disable_dig_weather(request, monkeypatch):
    """
    Default-disable the dig weather system for all tests.

    ``DigService._get_weather_effects`` rolls random weather lazily on the
    first ``/dig`` of each guild-day. That randomness leaks into any test that
    calls ``dig()`` and asserts on advance bonuses, cave-in rates, or drain —
    weather can add ±advance, ±cave_in, and ±luminosity_drain, which would
    otherwise make these tests order-dependent under pytest-xdist.

    Tests that specifically exercise the weather system should opt out with
    ``@pytest.mark.real_weather``.
    """
    if request.node.get_closest_marker("real_weather"):
        return
    import services.dig_service as _dig_service_module
    monkeypatch.setattr(
        _dig_service_module.DigService,
        "_get_weather_effects",
        lambda self, guild_id, layer_name: {},
    )


@pytest.fixture(scope="session")
def _schema_template_path(tmp_path_factory):
    """
    Create a schema template database once per test session.

    All 56 migrations run ONCE here. Tests copy from this template
    instead of running schema initialization each time.
    """
    template_dir = tmp_path_factory.mktemp("schema_template")
    template_path = str(template_dir / "template.db")
    db = Database(template_path)
    # Checkpoint WAL so all data is in the main .db file before copies.
    # Without this, shutil.copy2 misses data in the -wal file.
    if db._anchor_connection:
        db._anchor_connection.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        db._anchor_connection.close()
    yield template_path


@pytest.fixture
def temp_db_path(tmp_path):
    """Create a temporary database path (no schema)."""
    path = str(tmp_path / "temp.db")
    yield path


@pytest.fixture
def sample_players():
    """Create sample player data for testing."""
    return [
        Player(
            name=f"Player{i}",
            mmr=3000 + i * 100,
            wins=5,
            losses=3,
            preferred_roles=["1", "2"],
            glicko_rating=1500 + i * 50,
        )
        for i in range(12)
    ]


@pytest.fixture
def repo_db_path(_schema_template_path, tmp_path):
    """
    Create a temporary database with initialized schema for repository tests.

    Fast: file copy (~1ms) instead of schema initialization (~50ms+).
    The schema template is created once per session and reused.
    """
    test_db_path = str(tmp_path / "test.db")
    shutil.copy2(_schema_template_path, test_db_path)
    yield test_db_path


@pytest.fixture
def player_repository(repo_db_path):
    """Create a player repository with temp database."""
    return PlayerRepository(repo_db_path)


@pytest.fixture
def match_repository(repo_db_path):
    """Create a match repository with temp database."""
    return MatchRepository(repo_db_path)


@pytest.fixture
def lobby_repository(repo_db_path):
    """Create a lobby repository with temp database."""
    return LobbyRepository(repo_db_path)


@pytest.fixture
def lobby_manager(lobby_repository):
    """Create a lobby manager wired to lobby repository."""
    return LobbyManager(lobby_repository)


@pytest.fixture
def test_db(temp_db_path):
    """Create a Database instance with temporary file.

    Use this fixture instead of defining custom fixtures with time.sleep().
    """
    return Database(temp_db_path)


@pytest.fixture
def test_db_with_schema(repo_db_path):
    """Create a Database instance over a pre-initialized schema.

    Prefer this over ``test_db`` when a test needs tables to exist up front
    (e.g., record_match, player mutations).
    """
    return Database(repo_db_path)


@pytest.fixture
def test_db_memory():
    """Create an in-memory Database instance for faster tests.

    Use this when you don't need persistence across restarts.
    """
    return Database(":memory:")


# =============================================================================
# GUILD ID FIXTURES
# =============================================================================


@pytest.fixture
def guild_id():
    """Standard guild ID for single-guild tests.

    Use this fixture instead of hardcoding guild IDs in tests.
    For multi-guild isolation tests, also use secondary_guild_id.
    """
    return TEST_GUILD_ID


@pytest.fixture
def secondary_guild_id():
    """Secondary guild ID for multi-guild isolation tests.

    Use this alongside guild_id to verify data isolation between guilds.
    """
    return TEST_GUILD_ID_SECONDARY


# =============================================================================
# REPOSITORY FIXTURES
# =============================================================================


@pytest.fixture
def bet_repository(repo_db_path):
    """Create a bet repository with temp database."""
    return BetRepository(repo_db_path)


@pytest.fixture
def pairings_repository(repo_db_path):
    """Create a pairings repository with temp database."""
    return PairingsRepository(repo_db_path)


@pytest.fixture
def guild_config_repository(repo_db_path):
    """Create a guild config repository with temp database."""
    return GuildConfigRepository(repo_db_path)


# =============================================================================
# SERVICE FIXTURES
# =============================================================================


@pytest.fixture
def guild_config_service(guild_config_repository):
    """Create a guild config service."""
    return GuildConfigService(guild_config_repository)


@pytest.fixture
def garnishment_service(player_repository):
    """Create a garnishment service with default rate."""
    return GarnishmentService(player_repository)


@pytest.fixture
def betting_service(bet_repository, player_repository, garnishment_service):
    """Create a betting service with all dependencies wired."""
    return BettingService(
        bet_repo=bet_repository,
        player_repo=player_repository,
        garnishment_service=garnishment_service,
    )


@pytest.fixture
def match_service(player_repository, match_repository):
    """Create a minimal match service without betting.

    For tests that need betting integration, use match_service_with_betting.
    """
    return MatchService(
        player_repo=player_repository,
        match_repo=match_repository,
    )


@pytest.fixture
def match_service_with_betting(
    player_repository, match_repository, betting_service, pairings_repository
):
    """Create a match service with betting and pairings enabled.

    Use this for tests that involve betting, payouts, or pairwise stats.
    """
    return MatchService(
        player_repo=player_repository,
        match_repo=match_repository,
        betting_service=betting_service,
        pairings_repo=pairings_repository,
    )

