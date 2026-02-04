"""
Tests for the /addfake command in AdminCommands.
"""

import random
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from commands import admin as admin_module
from commands.admin import AdminCommands
from database import Database
from services.lobby_manager_service import LobbyManagerService as LobbyManager
from repositories.player_repository import PlayerRepository
from services.lobby_service import LobbyService


@pytest.fixture(autouse=True)
def clear_processed_interactions():
    """Clear the module-level processed interactions set before each test."""
    admin_module._processed_interactions.clear()


class FakePlayerRepo:
    """Minimal player repo stub."""

    def __init__(self):
        self.players = {}

    def get_by_id(self, discord_id, guild_id=None):
        return self.players.get(discord_id)

    def get_by_ids(self, ids, guild_id=None):
        return [self.players[i] for i in ids if i in self.players]

    def add(self, discord_id, discord_username, guild_id=None, initial_mmr=None, glicko_rating=None,
            glicko_rd=None, glicko_volatility=None, preferred_roles=None):
        self.players[discord_id] = SimpleNamespace(
            discord_id=discord_id,
            name=discord_username,
            glicko_rating=glicko_rating,
            glicko_rd=glicko_rd,
            preferred_roles=preferred_roles or [],
            mmr=initial_mmr,
        )


class FakeFollowup:
    def __init__(self):
        self.messages = []

    async def send(self, content=None, ephemeral=None, embed=None, allowed_mentions=None):
        self.messages.append({"content": content, "ephemeral": ephemeral})


class FakeChannel:
    async def fetch_message(self, message_id):
        raise Exception("message not found")


class FakeInteraction:
    def __init__(self, user_id=1, guild_id=123):
        self.id = random.randint(1, 999999999)  # Unique ID each time
        self.user = SimpleNamespace(id=user_id)
        self.guild = SimpleNamespace(id=guild_id)
        self.channel = FakeChannel()
        self.followup = FakeFollowup()


def make_services():
    db = Database(db_path=":memory:")
    lobby_manager = LobbyManager(db)
    player_repo = FakePlayerRepo()
    lobby_service = LobbyService(lobby_manager, player_repo, max_players=14)
    return lobby_service, player_repo


def make_bot():
    bot = SimpleNamespace()
    bot.get_channel = lambda x: None
    bot.fetch_channel = AsyncMock(return_value=FakeChannel())
    return bot


async def invoke_addfake(cog, interaction, count):
    return await cog.addfake.callback(cog, interaction, count)


@pytest.mark.asyncio
async def test_addfake_adds_users_to_lobby(monkeypatch):
    """Test that addfake actually adds fake users to the lobby."""
    lobby_service, player_repo = make_services()
    lobby = lobby_service.get_or_create_lobby(creator_id=99)

    interaction = FakeInteraction(user_id=1)
    monkeypatch.setattr("commands.admin.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.admin.has_admin_permission", lambda _: True)
    monkeypatch.setattr("commands.admin.GLOBAL_RATE_LIMITER.check",
                        lambda **kw: SimpleNamespace(allowed=True, retry_after_seconds=0))

    cog = AdminCommands(make_bot(), lobby_service, player_repo)
    await invoke_addfake(cog, interaction, 5)

    # Verify 5 fake users were added
    assert lobby.get_player_count() == 5
    assert -1 in lobby.players
    assert -5 in lobby.players


@pytest.mark.asyncio
async def test_addfake_works_when_defer_fails(monkeypatch):
    """Critical: addfake should still add users even when Discord interaction times out."""
    lobby_service, player_repo = make_services()
    lobby = lobby_service.get_or_create_lobby(creator_id=99)

    interaction = FakeInteraction(user_id=1)
    # Simulate defer failing (Discord timeout)
    monkeypatch.setattr("commands.admin.safe_defer", AsyncMock(return_value=False))
    monkeypatch.setattr("commands.admin.has_admin_permission", lambda _: True)
    monkeypatch.setattr("commands.admin.GLOBAL_RATE_LIMITER.check",
                        lambda **kw: SimpleNamespace(allowed=True, retry_after_seconds=0))

    cog = AdminCommands(make_bot(), lobby_service, player_repo)
    await invoke_addfake(cog, interaction, 3)

    # Even though defer failed, users should be added
    assert lobby.get_player_count() == 3, "Fake users should be added even when defer fails"
    assert -1 in lobby.players
    assert -3 in lobby.players
    # No response should be sent (can't respond when defer fails)
    assert len(interaction.followup.messages) == 0


@pytest.mark.asyncio
async def test_addfake_continues_numbering(monkeypatch):
    """Test that subsequent addfake calls continue from the highest index."""
    lobby_service, player_repo = make_services()
    lobby = lobby_service.get_or_create_lobby(creator_id=99)

    interaction = FakeInteraction(user_id=1)
    monkeypatch.setattr("commands.admin.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.admin.has_admin_permission", lambda _: True)
    monkeypatch.setattr("commands.admin.GLOBAL_RATE_LIMITER.check",
                        lambda **kw: SimpleNamespace(allowed=True, retry_after_seconds=0))

    cog = AdminCommands(make_bot(), lobby_service, player_repo)

    # First call adds FakeUser1-3
    await invoke_addfake(cog, interaction, 3)
    assert lobby.get_player_count() == 3
    assert -1 in lobby.players
    assert -3 in lobby.players

    # Second call should add FakeUser4-6, not try to re-add 1-3
    interaction2 = FakeInteraction(user_id=1)
    await invoke_addfake(cog, interaction2, 3)

    assert lobby.get_player_count() == 6
    assert -4 in lobby.players
    assert -6 in lobby.players
