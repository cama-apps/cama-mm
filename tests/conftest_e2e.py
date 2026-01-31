"""
Shared fixtures and utilities for end-to-end tests.

Uses centralized fixtures from conftest.py for fast database setup.
"""

from unittest.mock import AsyncMock, Mock

import pytest

from database import Database


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
def e2e_test_db(repo_db_path):
    """Create a Database for e2e tests using centralized fast fixture."""
    return Database(repo_db_path)


@pytest.fixture
def match_test_db(repo_db_path):
    """Create a Database for match service tests using centralized fast fixture."""
    return Database(repo_db_path)


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
