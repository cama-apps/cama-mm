"""
Tests for the /kick command in LobbyCommands.
"""

from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from commands.lobby import LobbyCommands
from database import Database
from domain.models.lobby import LobbyManager
from services.lobby_service import LobbyService


async def invoke_kick(cog: LobbyCommands, interaction, player):
    """Invoke the app command callback directly for testing."""
    return await cog.kick.callback(cog, interaction, player)


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


class FakeMessage:
    def __init__(self):
        self.edits = []
        self.removed_reactions = []

    async def edit(self, embed=None, allowed_mentions=None):
        self.edits.append({"embed": embed, "allowed_mentions": allowed_mentions})

    async def remove_reaction(self, emoji, user):
        self.removed_reactions.append((emoji, user))


class FakeChannel:
    def __init__(self, message: FakeMessage):
        self.message = message
        self.fetched = []

    async def fetch_message(self, message_id):
        self.fetched.append(message_id)
        return self.message


class FakeInteraction:
    def __init__(self, user_id=1, message=None):
        self.user = SimpleNamespace(id=user_id, mention=f"<@{user_id}>")
        self.guild = None
        self.channel = FakeChannel(message or FakeMessage())
        self.followup = FakeFollowup()


def make_lobby_service():
    lobby_manager = LobbyManager(Database(db_path=":memory:"))
    player_repo = FakePlayerRepo()
    return lobby_manager, LobbyService(lobby_manager, player_repo)


def make_bot():
    return SimpleNamespace()


@pytest.mark.asyncio
async def test_kick_removes_reaction_and_updates_message(monkeypatch):
    lobby_manager, lobby_service = make_lobby_service()
    lobby = lobby_service.get_or_create_lobby(creator_id=99)
    lobby.add_player(42)
    lobby_service.set_lobby_message_id(message_id=12345)

    fake_message = FakeMessage()
    interaction = FakeInteraction(user_id=1, message=fake_message)
    kicked_player = SimpleNamespace(id=42, mention="<@42>")

    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.lobby.has_admin_permission", lambda _interaction: True)

    cog = LobbyCommands(make_bot(), lobby_service, FakePlayerService())
    await invoke_kick(cog, interaction, kicked_player)

    # Reaction should be removed for the kicked player
    assert fake_message.removed_reactions == [("⚔️", kicked_player)]
    # Lobby message should be edited to reflect updated embed
    assert fake_message.edits, "Lobby embed should be refreshed after kick"
    # Confirmation message should be sent to the admin
    assert interaction.followup.messages
    assert "Kicked" in interaction.followup.messages[0]["content"]
