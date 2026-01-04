"""
Pytest fixtures for tests.
"""

import os
import sqlite3
import tempfile

import pytest

from database import Database
from domain.models.lobby import LobbyManager
from domain.models.player import Player
from repositories.lobby_repository import LobbyRepository
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository


@pytest.fixture
def temp_db_path():
    """Create a temporary database path."""
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    yield path
    try:
        os.unlink(path)
    except OSError:
        pass


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
def repo_db_path():
    """Create a temporary database path for repository tests."""
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    # Initialize the database schema
    Database(path)
    # Sanity-check required tables exist in the file we just created
    with sqlite3.connect(path) as conn:
        cursor = conn.cursor()
        cursor.execute(
            "SELECT name FROM sqlite_master WHERE type='table' AND name IN ('players', 'matches')"
        )
        existing = {row[0] for row in cursor.fetchall()}
        required = {"players", "matches"}
        if not required.issubset(existing):
            raise RuntimeError(
                f"Schema initialization failed for {path}; missing tables: {sorted(required - existing)}"
            )
    yield path
    try:
        os.unlink(path)
    except OSError:
        pass


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
