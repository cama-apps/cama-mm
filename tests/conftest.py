"""
Pytest fixtures for tests.

Performance optimization: Uses session-scoped schema template to avoid running
56 database migrations for every test. Instead, we run migrations once and copy
the resulting database file (~1ms) instead of re-initializing (~50ms+).
"""

import os
import shutil
import sqlite3
import tempfile

import pytest

from database import Database
from services.lobby_manager_service import LobbyManagerService as LobbyManager
from domain.models.player import Player
from repositories.lobby_repository import LobbyRepository
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from utils.role_assignment_cache import clear_role_assignment_cache


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
