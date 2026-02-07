"""
Pytest fixtures for tests.

Performance optimization: Uses session-scoped schema template to avoid running
56 database migrations for every test. Instead, we run migrations once and copy
the resulting database file (~1ms) instead of re-initializing (~50ms+).

This module provides centralized constants and fixtures to reduce duplication
across the test suite. Import TEST_GUILD_ID from here instead of defining it locally.
"""

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
from utils.role_assignment_cache import clear_role_assignment_cache


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
def clear_caches():
    """
    Clear global caches before and after each test to prevent cross-test contamination.

    The role assignment cache is process-global (LRU cache) and can cause
    intermittent failures in parallel test execution if stale data persists.
    """
    clear_role_assignment_cache()
    yield
    clear_role_assignment_cache()


@pytest.fixture(scope="session")
def _schema_template_path(tmp_path_factory):
    """
    Create a schema template database once per test session.

    All 56 migrations run ONCE here. Tests copy from this template
    instead of running schema initialization each time.
    """
    template_dir = tmp_path_factory.mktemp("schema_template")
    template_path = str(template_dir / "template.db")
    Database(template_path)
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


# =============================================================================
# PLAYER FIXTURES
# =============================================================================


@pytest.fixture
def registered_players(player_repository, guild_id):
    """Create 10 registered players with standard ratings.

    Returns list of discord_ids: [10001, 10002, ..., 10010]

    All players have:
    - Glicko rating: 1500-1680 (incrementing by 20)
    - MMR: 1500-1950 (incrementing by 50)
    - RD: 350 (uncalibrated)
    - Default jopacoin balance: 3

    Use this for most shuffle and match tests to avoid repetitive setup.
    """
    player_ids = list(range(10001, 10011))
    for idx, pid in enumerate(player_ids):
        player_repository.add(
            discord_id=pid,
            discord_username=f"TestPlayer{idx}",
            guild_id=guild_id,
            initial_mmr=1500 + idx * 50,
            glicko_rating=1500.0 + idx * 20,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
    return player_ids


@pytest.fixture
def registered_players_with_balance(player_repository, guild_id):
    """Create 10 registered players with 100 jopacoin each.

    Returns list of discord_ids: [10001, 10002, ..., 10010]

    Like registered_players but with 100 JC balance for betting tests.
    """
    player_ids = list(range(10001, 10011))
    for idx, pid in enumerate(player_ids):
        player_repository.add(
            discord_id=pid,
            discord_username=f"TestPlayer{idx}",
            guild_id=guild_id,
            initial_mmr=1500 + idx * 50,
            glicko_rating=1500.0 + idx * 20,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
        player_repository.update_balance(pid, guild_id, 100)
    return player_ids


@pytest.fixture
def registered_players_12(player_repository, guild_id):
    """Create 12 registered players for exclusion/overflow tests.

    Returns list of discord_ids: [10001, 10002, ..., 10012]

    Use when testing 10-player selection from larger pool.
    """
    player_ids = list(range(10001, 10013))
    for idx, pid in enumerate(player_ids):
        player_repository.add(
            discord_id=pid,
            discord_username=f"TestPlayer{idx}",
            guild_id=guild_id,
            initial_mmr=1500 + idx * 50,
            glicko_rating=1500.0 + idx * 20,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
    return player_ids


@pytest.fixture
def registered_players_14(player_repository, guild_id):
    """Create 14 registered players (max lobby size).

    Returns list of discord_ids: [10001, 10002, ..., 10014]

    Use for max-lobby and conditional player tests.
    """
    player_ids = list(range(10001, 10015))
    for idx, pid in enumerate(player_ids):
        player_repository.add(
            discord_id=pid,
            discord_username=f"TestPlayer{idx}",
            guild_id=guild_id,
            initial_mmr=1500 + idx * 50,
            glicko_rating=1500.0 + idx * 20,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
    return player_ids
