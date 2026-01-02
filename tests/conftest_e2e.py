"""
Shared fixtures and utilities for end-to-end tests.
"""

import pytest
import os
import tempfile
import time
from unittest.mock import Mock, AsyncMock

from database import Database
from domain.models.lobby import LobbyManager


class MockDiscordUser:
    """Mock Discord user for testing."""
    def __init__(self, user_id, username="TestUser"):
        self.id = user_id
        self.name = username
        self.display_name = username
        self.mention = f"<@{user_id}>"
    
    def __str__(self):
        return self.name


class MockDiscordInteraction:
    """Mock Discord interaction for testing."""
    def __init__(self, user_id, username="TestUser"):
        self.user = MockDiscordUser(user_id, username)
        self.response = AsyncMock()
        self.followup = AsyncMock()
        self.channel = Mock()
        self.guild = None
    
    async def defer(self, **kwargs):
        """Mock defer response."""
        pass


@pytest.fixture
def e2e_test_db():
    """Create a temporary Database for e2e tests."""
    fd, db_path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    db = Database(db_path)
    yield db
    try:
        import sqlite3
        sqlite3.connect(db_path).close()
    except Exception:
        pass
    try:
        os.unlink(db_path)
    except PermissionError:
        time.sleep(0.1)
        try:
            os.unlink(db_path)
        except Exception:
            pass


@pytest.fixture
def match_test_db():
    """Create a temporary Database for match service tests."""
    fd, db_path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    db = Database(db_path)
    yield db
    try:
        import sqlite3
        sqlite3.connect(db_path).close()
    except Exception:
        pass
    try:
        os.unlink(db_path)
    except PermissionError:
        time.sleep(0.1)
        try:
            os.unlink(db_path)
        except Exception:
            pass


def create_test_players(db, start_id=60000, count=10):
    """Helper function to create test players."""
    player_ids = list(range(start_id, start_id + count))
    for idx, pid in enumerate(player_ids):
        db.add_player(
            discord_id=pid,
            discord_username=f"TestGuildPlayer{pid}",
            initial_mmr=1600 + idx * 10,
            glicko_rating=1600.0 + idx * 2,
            glicko_rd=200.0,
            glicko_volatility=0.06,
        )
    return player_ids


def create_test_db_fixture():
    """Factory function for creating test_db fixtures (used in class-based tests)."""
    fd, db_path = tempfile.mkstemp(suffix='.db')
    os.close(fd)
    db = Database(db_path)
    return db, db_path


def cleanup_test_db(db_path):
    """Cleanup function for test databases."""
    try:
        import sqlite3
        sqlite3.connect(db_path).close()
    except:
        pass
    time.sleep(0.1)
    try:
        os.unlink(db_path)
    except PermissionError:
        time.sleep(0.2)
        try:
            os.unlink(db_path)
        except:
            pass

