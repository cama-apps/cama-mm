"""
Tests for the /kick command in LobbyCommands.
"""

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from commands.lobby import LobbyCommands
from domain.models.lobby import LobbyKind
from services.lobby_manager_service import LobbyManagerService as LobbyManager
from services.lobby_service import LobbyService
from tests.fakes.lobby_repo import FakeLobbyRepo


async def invoke_kick(cog: LobbyCommands, interaction, player, reason=None):
    """Invoke the app command callback directly for testing."""
    return await cog.kick.callback(cog, interaction, player, reason=reason)


class FakePlayerRepo:
    """Minimal player repo stub used by LobbyService."""

    def get_by_ids(self, _ids, guild_id=None):
        return []

    def get_captain_eligible_players(self, _ids, guild_id=None):
        return []


class FakePlayerService:
    """Minimal player service stub used by LobbyCommands."""

    def get_player(self, discord_id, guild_id=None):
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

    async def edit(self, embed=None, allowed_mentions=None, content=None):
        self.edits.append({"embed": embed, "allowed_mentions": allowed_mentions, "content": content})

    async def remove_reaction(self, emoji, user):
        self.removed_reactions.append((emoji, user))


class FakeChannel:
    def __init__(self, message: FakeMessage):
        self.message = message
        self.fetched = []
        self.partial = []

    async def fetch_message(self, message_id):
        self.fetched.append(message_id)
        return self.message

    def get_partial_message(self, message_id):
        self.partial.append(message_id)
        return self.message


class FakeInteraction:
    def __init__(self, user_id=1, message=None, guild_id=9000):
        self.user = SimpleNamespace(id=user_id, mention=f"<@{user_id}>")
        self.guild = SimpleNamespace(id=guild_id) if guild_id is not None else None
        self.channel = FakeChannel(message or FakeMessage())
        self.followup = FakeFollowup()
        self.response = AsyncMock()


def make_lobby_service():
    lobby_manager = LobbyManager(FakeLobbyRepo())
    player_repo = FakePlayerRepo()
    return lobby_manager, LobbyService(lobby_manager, player_repo)


def make_bot(channel=None, moderation_service=None):
    """Create a fake bot with get_channel support."""
    bot = SimpleNamespace(moderation_service=moderation_service)
    bot.get_channel = lambda cid: channel
    return bot


def seat_in_lowskill(lobby_manager, player_id, *, creator_id, guild_id=9000):
    """Open the low-skill lobby and seat a player in it.

    Goes through the manager so the rating gate LobbyService applies to
    low-skill creation stays out of kick fixtures, which do not exercise it.
    """
    lobby = lobby_manager.get_or_create_lobby(
        creator_id=creator_id,
        guild_id=guild_id,
        lobby_kind=LobbyKind.LOWSKILL,
    )
    lobby.add_player(player_id)
    return lobby


def test_kick_reason_is_optional_and_privately_bounded():
    parameters = {parameter.name: parameter for parameter in LobbyCommands.kick.parameters}

    assert parameters["reason"].required is False
    assert parameters["reason"].min_value == 3
    assert parameters["reason"].max_value == 300


@pytest.mark.asyncio
async def test_kick_removes_reaction_and_updates_message(monkeypatch):
    _, lobby_service = make_lobby_service()
    lobby = lobby_service.get_or_create_lobby(creator_id=99, guild_id=9000)
    lobby.add_player(42)
    lobby_service.set_lobby_message_id(message_id=12345, channel_id=100, guild_id=9000)

    fake_message = FakeMessage()
    fake_channel = FakeChannel(fake_message)
    interaction = FakeInteraction(user_id=1, message=fake_message, guild_id=9000)
    kicked_player = SimpleNamespace(id=42, mention="<@42>", display_name="TestPlayer")

    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.lobby.has_admin_permission", lambda _interaction: True)

    cog = LobbyCommands(make_bot(channel=fake_channel), lobby_service, FakePlayerService())
    await invoke_kick(cog, interaction, kicked_player)

    # Lobby message should be edited to reflect updated embed
    assert fake_message.edits, "Lobby embed should be refreshed after kick"
    assert fake_channel.fetched == []
    assert fake_channel.partial == [12345, 12345]
    # Confirmation message should be sent to the admin, naming the lobby
    assert interaction.followup.messages
    assert interaction.followup.messages[0]["content"] == (
        f"✅ Kicked <@42> from {LobbyKind.OPEN.label}."
    )


@pytest.mark.asyncio
async def test_creator_who_left_lobby_can_no_longer_kick(monkeypatch):
    _, lobby_service = make_lobby_service()
    lobby = lobby_service.get_or_create_lobby(creator_id=7, guild_id=9000)
    lobby.add_player(42)
    interaction = FakeInteraction(user_id=7, guild_id=9000)
    kicked_player = SimpleNamespace(id=42, mention="<@42>", display_name="Target")

    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.lobby.has_admin_permission", lambda _interaction: False)
    cog = LobbyCommands(make_bot(), lobby_service, FakePlayerService())
    cog.eject_lobby_player = AsyncMock(return_value=True)

    await invoke_kick(cog, interaction, kicked_player)

    assert 42 in lobby.players
    cog.eject_lobby_player.assert_not_awaited()
    assert interaction.followup.messages[-1]["ephemeral"] is True
    assert "Permission denied" in interaction.followup.messages[-1]["content"]


@pytest.mark.asyncio
async def test_creator_kick_audits_optional_reason_without_posting_it_publicly(
    monkeypatch,
):
    _, lobby_service = make_lobby_service()
    lobby = lobby_service.get_or_create_lobby(creator_id=7, guild_id=9000)
    lobby.add_player(7)
    lobby.add_player(42)
    moderation_service = MagicMock()
    interaction = FakeInteraction(user_id=7, guild_id=9000)
    kicked_player = SimpleNamespace(
        id=42, mention="<@42>", display_name="Target", send=AsyncMock()
    )

    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.lobby.has_admin_permission", lambda _interaction: False)
    cog = LobbyCommands(
        make_bot(moderation_service=moderation_service),
        lobby_service,
        FakePlayerService(),
    )
    cog.eject_lobby_player = AsyncMock(return_value=True)

    await invoke_kick(
        cog,
        interaction,
        kicked_player,
        reason="repeated ready-check griefing",
    )

    cog.eject_lobby_player.assert_awaited_once()
    ejection = cog.eject_lobby_player.await_args.kwargs
    assert "repeated ready-check griefing" not in ejection["public_message"]
    kicked_player.send.assert_awaited_once()
    assert "repeated ready-check griefing" in kicked_player.send.await_args.args[0]
    moderation_service.record_kick.assert_called_once_with(
        42,
        9000,
        actor_id=7,
        lobby_kind="open",
        reason="repeated ready-check griefing",
        source="creator",
    )
    assert interaction.followup.messages[-1]["ephemeral"] is True
    assert "Kicked <@42>" in interaction.followup.messages[-1]["content"]


@pytest.mark.asyncio
async def test_admin_kick_clears_both_lobbies_with_a_single_dm(monkeypatch):
    """A double-queued target loses both seats, audited once per lobby kind."""
    lobby_manager, lobby_service = make_lobby_service()
    open_lobby = lobby_service.get_or_create_lobby(creator_id=99, guild_id=9000)
    open_lobby.add_player(42)
    seat_in_lowskill(lobby_manager, 42, creator_id=99)

    moderation_service = MagicMock()
    interaction = FakeInteraction(user_id=1, guild_id=9000)
    kicked_player = SimpleNamespace(
        id=42, mention="<@42>", display_name="Target", send=AsyncMock()
    )

    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.lobby.has_admin_permission", lambda _interaction: True)
    cog = LobbyCommands(
        make_bot(moderation_service=moderation_service),
        lobby_service,
        FakePlayerService(),
    )
    cog.eject_lobby_player = AsyncMock(return_value=True)

    await invoke_kick(cog, interaction, kicked_player, reason="queue griefing")

    ejections = [call.kwargs for call in cog.eject_lobby_player.await_args_list]
    assert [ejection["lobby_kind"] for ejection in ejections] == [
        LobbyKind.OPEN,
        LobbyKind.LOWSKILL,
    ]
    # One DM covers the whole kick, naming every lobby it actually reached —
    # the per-lobby ejections must not each fire their own.
    assert all(ejection["dm_message"] is None for ejection in ejections)
    kicked_player.send.assert_awaited_once()
    dm_message = kicked_player.send.await_args.args[0]
    assert "queue griefing" in dm_message
    assert LobbyKind.OPEN.label in dm_message
    assert LobbyKind.LOWSKILL.label in dm_message

    audits = moderation_service.record_kick.call_args_list
    assert [audit.kwargs["lobby_kind"] for audit in audits] == ["open", "lowskill"]
    assert all(audit.args == (42, 9000) for audit in audits)
    assert all(audit.kwargs["source"] == "admin" for audit in audits)


@pytest.mark.asyncio
async def test_creator_kick_reaches_only_the_lobby_they_opened(monkeypatch):
    """Lobby ownership is not authority over the other queue's roster."""
    lobby_manager, lobby_service = make_lobby_service()
    open_lobby = lobby_service.get_or_create_lobby(creator_id=7, guild_id=9000)
    open_lobby.add_player(7)
    open_lobby.add_player(42)
    lowskill_lobby = seat_in_lowskill(lobby_manager, 42, creator_id=99)

    moderation_service = MagicMock()
    interaction = FakeInteraction(user_id=7, guild_id=9000)
    kicked_player = SimpleNamespace(
        id=42,
        mention="<@42>",
        display_name="Target",
        send=AsyncMock(),
    )

    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.lobby.has_admin_permission", lambda _interaction: False)
    cog = LobbyCommands(
        make_bot(moderation_service=moderation_service),
        lobby_service,
        FakePlayerService(),
    )

    await invoke_kick(cog, interaction, kicked_player)

    assert 42 not in open_lobby.players
    assert 42 in lowskill_lobby.players
    moderation_service.record_kick.assert_called_once_with(
        42,
        9000,
        actor_id=7,
        lobby_kind="open",
        reason=None,
        source="creator",
    )
    content = interaction.followup.messages[-1]["content"]
    assert LobbyKind.OPEN.label in content
    assert LobbyKind.LOWSKILL.label not in content


@pytest.mark.asyncio
async def test_non_creator_cannot_kick_from_either_lobby(monkeypatch):
    """Sitting in both lobbies without owning either grants no kick rights."""
    lobby_manager, lobby_service = make_lobby_service()
    open_lobby = lobby_service.get_or_create_lobby(creator_id=99, guild_id=9000)
    open_lobby.add_player(7)
    open_lobby.add_player(42)
    lowskill_lobby = seat_in_lowskill(lobby_manager, 42, creator_id=99)
    lowskill_lobby.add_player(7)

    interaction = FakeInteraction(user_id=7, guild_id=9000)
    kicked_player = SimpleNamespace(id=42, mention="<@42>", display_name="Target")

    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.lobby.has_admin_permission", lambda _interaction: False)
    cog = LobbyCommands(make_bot(), lobby_service, FakePlayerService())
    cog.eject_lobby_player = AsyncMock(return_value=True)

    await invoke_kick(cog, interaction, kicked_player)

    cog.eject_lobby_player.assert_not_awaited()
    assert 42 in open_lobby.players
    assert 42 in lowskill_lobby.players
    assert interaction.followup.messages[-1]["ephemeral"] is True
    assert "Permission denied" in interaction.followup.messages[-1]["content"]
