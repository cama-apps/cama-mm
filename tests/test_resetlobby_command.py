"""
Tests for the /resetlobby command in LobbyCommands.
"""

from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from commands.lobby import LobbyCommands
from database import Database
from services.lobby_manager_service import LobbyManagerService as LobbyManager
from services.lobby_service import LobbyService


async def invoke_reset(cog: LobbyCommands, interaction):
    """Invoke the app command callback directly for testing."""
    return await cog.resetlobby.callback(cog, interaction)


class FakePlayerRepo:
    """Minimal player repo stub used by LobbyService."""

    def get_by_ids(self, _ids):
        return []


class FakePlayerService:
    """Minimal player service stub used by LobbyCommands."""

    def get_player(self, discord_id):
        return None


class FakeFollowup:
    def __init__(self):
        self.messages = []

    async def send(self, content=None, ephemeral=None, embed=None, allowed_mentions=None):
        self.messages.append(
            {
                "content": content,
                "ephemeral": ephemeral,
                "embed": embed,
                "allowed_mentions": allowed_mentions,
            }
        )


class FakeChannel:
    def __init__(self):
        self.fetched = []

    async def fetch_message(self, message_id):
        self.fetched.append(message_id)
        raise Exception("message not found")


class FakeInteraction:
    def __init__(self, user_id=1, guild_id=123):
        self.user = SimpleNamespace(id=user_id)
        self.guild = SimpleNamespace(id=guild_id)
        self.channel = FakeChannel()
        self.followup = FakeFollowup()


def make_lobby_service():
    lobby_manager = LobbyManager(Database(db_path=":memory:"))
    player_repo = FakePlayerRepo()
    return lobby_manager, LobbyService(lobby_manager, player_repo)


def make_bot(match_service=None):
    bot = SimpleNamespace()
    bot.match_service = match_service
    bot.user = SimpleNamespace(id=999999)  # Mock bot user
    bot.get_channel = lambda x: None  # Return None to use interaction channel
    return bot


@pytest.mark.asyncio
async def test_resetlobby_allows_admin(monkeypatch):
    lobby_manager, lobby_service = make_lobby_service()
    lobby = lobby_service.get_or_create_lobby(creator_id=99)
    lobby.add_player(42)
    interaction = FakeInteraction(user_id=1)

    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.lobby.has_admin_permission", lambda _interaction: True)

    cog = LobbyCommands(make_bot(match_service=None), lobby_service, FakePlayerService())
    await invoke_reset(cog, interaction)

    assert lobby_service.get_lobby() is None
    assert interaction.followup.messages
    assert "Lobby reset" in interaction.followup.messages[0]["content"]


@pytest.mark.asyncio
async def test_resetlobby_allows_creator(monkeypatch):
    lobby_manager, lobby_service = make_lobby_service()
    lobby = lobby_service.get_or_create_lobby(creator_id=7)
    lobby.add_player(7)
    interaction = FakeInteraction(user_id=7)

    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.lobby.has_admin_permission", lambda _interaction: False)

    cog = LobbyCommands(make_bot(match_service=None), lobby_service, FakePlayerService())
    await invoke_reset(cog, interaction)

    assert lobby_service.get_lobby() is None
    assert interaction.followup.messages
    assert "Lobby reset" in interaction.followup.messages[0]["content"]


@pytest.mark.asyncio
async def test_resetlobby_blocks_pending_match(monkeypatch):
    class PendingMatchService:
        def get_last_shuffle(self, _guild_id):
            return {"shuffle_message_jump_url": "http://example.com"}

    lobby_manager, lobby_service = make_lobby_service()
    lobby_service.get_or_create_lobby(creator_id=99)
    interaction = FakeInteraction(user_id=1)

    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.lobby.has_admin_permission", lambda _interaction: True)

    cog = LobbyCommands(
        make_bot(match_service=PendingMatchService()), lobby_service, FakePlayerService()
    )
    await invoke_reset(cog, interaction)

    assert lobby_service.get_lobby() is not None, "Should not reset when a match is pending"
    assert interaction.followup.messages
    assert "pending match" in interaction.followup.messages[0]["content"]


@pytest.mark.asyncio
async def test_resetlobby_denies_non_admin_non_creator(monkeypatch):
    lobby_manager, lobby_service = make_lobby_service()
    lobby = lobby_service.get_or_create_lobby(creator_id=5)
    lobby.add_player(5)
    interaction = FakeInteraction(user_id=6)

    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.lobby.has_admin_permission", lambda _interaction: False)

    cog = LobbyCommands(make_bot(match_service=None), lobby_service, FakePlayerService())
    await invoke_reset(cog, interaction)

    assert lobby_service.get_lobby() is not None, "Lobby should remain when permission denied"
    assert interaction.followup.messages
    assert "Permission denied" in interaction.followup.messages[0]["content"]


@pytest.mark.asyncio
async def test_resetlobby_new_lobby_is_empty(monkeypatch):
    """After reset, creating a new lobby should have zero players."""
    lobby_manager, lobby_service = make_lobby_service()

    # Create lobby and add players
    lobby = lobby_service.get_or_create_lobby(creator_id=99)
    lobby_service.join_lobby(1)
    lobby_service.join_lobby(2)
    lobby_service.join_lobby(3)
    assert lobby.get_player_count() == 3

    interaction = FakeInteraction(user_id=99)
    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.lobby.has_admin_permission", lambda _interaction: True)

    cog = LobbyCommands(make_bot(match_service=None), lobby_service, FakePlayerService())
    await invoke_reset(cog, interaction)

    # After reset, get_lobby should be None
    assert lobby_service.get_lobby() is None

    # Create a new lobby
    new_lobby = lobby_service.get_or_create_lobby(creator_id=99)

    # New lobby should have 0 players, not the old 3
    assert new_lobby.get_player_count() == 0, "New lobby should be empty after reset"
