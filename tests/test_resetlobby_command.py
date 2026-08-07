"""
Tests for the /resetlobby command in LobbyCommands.
"""

import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from commands.lobby import LobbyCommands
from domain.models.lobby import LobbyKind
from services.draft_state_manager import DraftStateManager
from services.lobby_manager_service import LobbyManagerService as LobbyManager
from services.lobby_service import LobbyService
from tests.fakes.lobby_repo import FakeLobbyRepo


async def invoke_reset(cog: LobbyCommands, interaction, lobby=None):
    """Invoke the app command callback directly for testing."""
    return await cog.resetlobby.callback(cog, interaction, lobby)


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
    lobby_manager = LobbyManager(FakeLobbyRepo())
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
    _, lobby_service = make_lobby_service()
    lobby = lobby_service.get_or_create_lobby(creator_id=99, guild_id=123)
    lobby.add_player(42)
    lobby.add_player(1)
    interaction = FakeInteraction(user_id=1)

    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.lobby.has_admin_permission", lambda _interaction: True)

    cog = LobbyCommands(make_bot(match_service=None), lobby_service, FakePlayerService())
    await invoke_reset(cog, interaction)

    assert lobby_service.get_lobby(guild_id=123) is None
    assert interaction.followup.messages
    assert "All You Can Feed reset" in interaction.followup.messages[0]["content"]


@pytest.mark.asyncio
async def test_resetlobby_allows_creator(monkeypatch):
    _, lobby_service = make_lobby_service()
    lobby = lobby_service.get_or_create_lobby(creator_id=7, guild_id=123)
    lobby.add_player(7)
    interaction = FakeInteraction(user_id=7)

    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.lobby.has_admin_permission", lambda _interaction: False)

    cog = LobbyCommands(make_bot(match_service=None), lobby_service, FakePlayerService())
    await invoke_reset(cog, interaction)

    assert lobby_service.get_lobby(guild_id=123) is None
    assert interaction.followup.messages
    assert "All You Can Feed reset" in interaction.followup.messages[0]["content"]


@pytest.mark.asyncio
async def test_resetlobby_infers_creator_lowskill_membership(monkeypatch):
    lobby_manager, lobby_service = make_lobby_service()
    lobby_service.get_or_create_lobby(creator_id=99, guild_id=123)
    lowskill_lobby = lobby_manager.get_or_create_lobby(
        creator_id=7,
        guild_id=123,
        lobby_kind="lowskill",
    )
    lowskill_lobby.add_player(7)
    interaction = FakeInteraction(user_id=7)
    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.lobby.has_admin_permission", lambda _interaction: False)
    cog = LobbyCommands(make_bot(match_service=None), lobby_service, FakePlayerService())

    await invoke_reset(cog, interaction)

    assert lobby_service.get_lobby(guild_id=123, lobby_kind="lowskill") is None
    assert lobby_service.get_lobby(guild_id=123, lobby_kind="open") is not None


def _match_service_with_pending(*states):
    """A match_service double whose state_service reports these pending matches."""

    class FakeStateService:
        def get_all_pending_matches(self, _guild_id):
            return list(states)

    class PendingMatchService:
        state_service = FakeStateService()

    return PendingMatchService()


@pytest.mark.asyncio
async def test_resetlobby_blocks_pending_match(monkeypatch):
    from domain.models.pending_match_state import PendingMatchState

    _, lobby_service = make_lobby_service()
    lobby = lobby_service.get_or_create_lobby(creator_id=99, guild_id=123)
    lobby.add_player(1)
    interaction = FakeInteraction(user_id=1)

    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.lobby.has_admin_permission", lambda _interaction: True)

    match_service = _match_service_with_pending(
        PendingMatchState(shuffle_message_jump_url="http://example.com")
    )
    cog = LobbyCommands(
        make_bot(match_service=match_service), lobby_service, FakePlayerService()
    )
    await invoke_reset(cog, interaction)

    assert lobby_service.get_lobby(guild_id=123) is not None, "Should not reset when a match is pending"
    assert interaction.followup.messages
    assert "pending match" in interaction.followup.messages[0]["content"]


@pytest.mark.asyncio
async def test_resetlobby_blocks_when_multiple_matches_are_pending(monkeypatch):
    """The guard must hold for concurrent matches, not just a single one.

    It used to ask get_last_shuffle, which returns None when *multiple* pending
    matches exist — so the guard fell open in exactly the concurrent-match case,
    archiving the thread and wiping metadata /record and /abort rely on.
    """
    from domain.models.pending_match_state import PendingMatchState

    _, lobby_service = make_lobby_service()
    lobby = lobby_service.get_or_create_lobby(creator_id=99, guild_id=123)
    lobby.add_player(1)
    interaction = FakeInteraction(user_id=1)

    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.lobby.has_admin_permission", lambda _interaction: True)

    match_service = _match_service_with_pending(
        PendingMatchState(pending_match_id=1, shuffle_message_jump_url="http://example.com/1"),
        PendingMatchState(pending_match_id=2, shuffle_message_jump_url="http://example.com/2"),
    )
    cog = LobbyCommands(
        make_bot(match_service=match_service), lobby_service, FakePlayerService()
    )
    await invoke_reset(cog, interaction)

    assert lobby_service.get_lobby(guild_id=123) is not None, (
        "Should not reset while two matches are pending"
    )
    assert interaction.followup.messages
    assert "pending match" in interaction.followup.messages[0]["content"]


@pytest.mark.asyncio
async def test_resetlobby_ignores_pending_match_from_sibling_lobby(monkeypatch):
    from domain.models.pending_match_state import PendingMatchState

    lobby_manager, lobby_service = make_lobby_service()
    low_lobby = lobby_manager.get_or_create_lobby(
        creator_id=7,
        guild_id=123,
        lobby_kind=LobbyKind.LOWSKILL,
    )
    low_lobby.add_player(7)
    interaction = FakeInteraction(user_id=7)
    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.lobby.has_admin_permission", lambda _interaction: False)
    match_service = _match_service_with_pending(
        PendingMatchState(pending_match_id=1, lobby_kind=LobbyKind.OPEN.value)
    )
    cog = LobbyCommands(
        make_bot(match_service=match_service), lobby_service, FakePlayerService()
    )

    await invoke_reset(cog, interaction)

    assert lobby_service.get_lobby(123, LobbyKind.LOWSKILL) is None


@pytest.mark.asyncio
async def test_resetlobby_ignores_active_draft_from_sibling_lobby(monkeypatch):
    lobby_manager, lobby_service = make_lobby_service()
    low_lobby = lobby_manager.get_or_create_lobby(
        creator_id=7,
        guild_id=123,
        lobby_kind=LobbyKind.LOWSKILL,
    )
    low_lobby.add_player(7)
    draft_state_manager = DraftStateManager()
    draft_state_manager.create_draft(123, LobbyKind.OPEN)
    bot = make_bot(match_service=None)
    bot.draft_state_manager = draft_state_manager
    interaction = FakeInteraction(user_id=7)
    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.lobby.has_admin_permission", lambda _interaction: False)
    cog = LobbyCommands(bot, lobby_service, FakePlayerService())

    await invoke_reset(cog, interaction)

    assert lobby_service.get_lobby(123, LobbyKind.LOWSKILL) is None


@pytest.mark.asyncio
async def test_resetlobby_blocks_active_draft_from_same_lobby(monkeypatch):
    lobby_manager, lobby_service = make_lobby_service()
    low_lobby = lobby_manager.get_or_create_lobby(
        creator_id=7,
        guild_id=123,
        lobby_kind=LobbyKind.LOWSKILL,
    )
    low_lobby.add_player(7)
    draft_state_manager = DraftStateManager()
    draft_state_manager.create_draft(123, LobbyKind.LOWSKILL)
    bot = make_bot(match_service=None)
    bot.draft_state_manager = draft_state_manager
    interaction = FakeInteraction(user_id=7)
    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.lobby.has_admin_permission", lambda _interaction: False)
    cog = LobbyCommands(bot, lobby_service, FakePlayerService())

    await invoke_reset(cog, interaction)

    assert lobby_service.get_lobby(123, LobbyKind.LOWSKILL) is not None
    assert "active draft" in interaction.followup.messages[0]["content"]


@pytest.mark.asyncio
async def test_resetlobby_blocks_in_flight_shuffle_from_same_lobby(monkeypatch):
    lobby_manager, lobby_service = make_lobby_service()
    lobby = lobby_manager.get_or_create_lobby(
        creator_id=7,
        guild_id=123,
        lobby_kind=LobbyKind.OPEN,
    )
    lobby.add_player(7)
    assert lobby_service.reserve_lobby_players(
        [7],
        123,
        LobbyKind.OPEN,
    )
    interaction = FakeInteraction(user_id=7)
    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.lobby.has_admin_permission", lambda _interaction: False)
    cog = LobbyCommands(make_bot(match_service=None), lobby_service, FakePlayerService())

    await invoke_reset(cog, interaction)

    assert lobby_service.get_lobby(123, LobbyKind.OPEN) is not None
    assert "shuffle or draft" in interaction.followup.messages[0]["content"]


@pytest.mark.asyncio
async def test_resetlobby_serializes_discord_cleanup_with_match_start(monkeypatch):
    lobby_manager, lobby_service = make_lobby_service()
    lobby = lobby_manager.get_or_create_lobby(
        creator_id=7,
        guild_id=123,
        lobby_kind=LobbyKind.OPEN,
    )
    lobby.players.update({7, 8})
    interaction = FakeInteraction(user_id=7)
    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.lobby.has_admin_permission", lambda _interaction: False)
    cog = LobbyCommands(make_bot(match_service=None), lobby_service, FakePlayerService())

    cleanup_started = asyncio.Event()
    release_cleanup = asyncio.Event()

    async def blocking_close(*_args, **_kwargs):
        cleanup_started.set()
        await release_cleanup.wait()

    cog._update_channel_message_closed = blocking_close
    cog._archive_lobby_thread = AsyncMock()

    reset_task = asyncio.create_task(invoke_reset(cog, interaction))
    await asyncio.wait_for(cleanup_started.wait(), timeout=1)

    async def start_match():
        lock = lobby_manager.get_shuffle_lock(123, LobbyKind.OPEN)
        await lock.acquire()
        try:
            return lobby_service.reserve_lobby_players(
                [7, 8],
                123,
                LobbyKind.OPEN,
            )
        finally:
            lock.release()

    start_task = asyncio.create_task(start_match())
    await asyncio.sleep(0)
    assert not start_task.done()

    release_cleanup.set()
    await reset_task
    assert await start_task is False
    assert lobby_service.get_lobby(123, LobbyKind.OPEN) is None


@pytest.mark.asyncio
async def test_resetlobby_denies_non_admin_non_creator(monkeypatch):
    _, lobby_service = make_lobby_service()
    lobby = lobby_service.get_or_create_lobby(creator_id=5, guild_id=123)
    lobby.add_player(5)
    lobby.add_player(6)
    interaction = FakeInteraction(user_id=6)

    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.lobby.has_admin_permission", lambda _interaction: False)

    cog = LobbyCommands(make_bot(match_service=None), lobby_service, FakePlayerService())
    await invoke_reset(cog, interaction)

    assert lobby_service.get_lobby(guild_id=123) is not None, "Lobby should remain when permission denied"
    assert interaction.followup.messages
    assert "Permission denied" in interaction.followup.messages[0]["content"]


@pytest.mark.asyncio
async def test_resetlobby_new_lobby_is_empty(monkeypatch):
    """After reset, creating a new lobby should have zero players."""
    _, lobby_service = make_lobby_service()

    # Create lobby and add players
    lobby = lobby_service.get_or_create_lobby(creator_id=99, guild_id=123)
    lobby_service.join_lobby(1, 123)
    lobby_service.join_lobby(2, 123)
    lobby_service.join_lobby(3, 123)
    assert lobby.get_player_count() == 3

    interaction = FakeInteraction(user_id=1)
    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.lobby.has_admin_permission", lambda _interaction: True)

    cog = LobbyCommands(make_bot(match_service=None), lobby_service, FakePlayerService())
    await invoke_reset(cog, interaction)

    # After reset, get_lobby should be None
    assert lobby_service.get_lobby(guild_id=123) is None

    # Create a new lobby
    new_lobby = lobby_service.get_or_create_lobby(creator_id=99, guild_id=123)

    # New lobby should have 0 players, not the old 3
    assert new_lobby.get_player_count() == 0, "New lobby should be empty after reset"


@pytest.mark.asyncio
async def test_resetlobby_admin_can_reset_lobby_without_membership(monkeypatch):
    """A non-member admin resets via the explicit lobby choice."""
    _, lobby_service = make_lobby_service()
    lobby = lobby_service.get_or_create_lobby(creator_id=99, guild_id=123)
    lobby.add_player(42)
    interaction = FakeInteraction(user_id=1)  # not in the lobby

    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.lobby.has_admin_permission", lambda _interaction: True)

    cog = LobbyCommands(make_bot(match_service=None), lobby_service, FakePlayerService())
    await invoke_reset(cog, interaction, lobby=SimpleNamespace(value="open"))

    assert lobby_service.get_lobby(guild_id=123) is None
    assert "All You Can Feed reset" in interaction.followup.messages[0]["content"]


@pytest.mark.asyncio
async def test_resetlobby_departed_creator_can_reset_with_choice(monkeypatch):
    """A creator who already left the lobby can still reset it."""
    _, lobby_service = make_lobby_service()
    lobby = lobby_service.get_or_create_lobby(creator_id=7, guild_id=123)
    lobby.add_player(42)  # creator 7 is not a member
    interaction = FakeInteraction(user_id=7)

    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.lobby.has_admin_permission", lambda _interaction: False)

    cog = LobbyCommands(make_bot(match_service=None), lobby_service, FakePlayerService())
    await invoke_reset(cog, interaction, lobby=SimpleNamespace(value="open"))

    assert lobby_service.get_lobby(guild_id=123) is None


@pytest.mark.asyncio
async def test_resetlobby_choice_still_enforces_permissions(monkeypatch):
    """The explicit choice bypasses only membership, never the permission gate."""
    _, lobby_service = make_lobby_service()
    lobby = lobby_service.get_or_create_lobby(creator_id=99, guild_id=123)
    lobby.add_player(42)
    interaction = FakeInteraction(user_id=6)  # neither admin nor creator

    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.lobby.has_admin_permission", lambda _interaction: False)

    cog = LobbyCommands(make_bot(match_service=None), lobby_service, FakePlayerService())
    await invoke_reset(cog, interaction, lobby=SimpleNamespace(value="open"))

    assert lobby_service.get_lobby(guild_id=123) is not None
    assert "Permission denied" in interaction.followup.messages[0]["content"]


@pytest.mark.asyncio
async def test_resetlobby_non_member_without_choice_gets_hint(monkeypatch):
    _, lobby_service = make_lobby_service()
    lobby = lobby_service.get_or_create_lobby(creator_id=99, guild_id=123)
    lobby.add_player(42)
    interaction = FakeInteraction(user_id=1)

    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.lobby.has_admin_permission", lambda _interaction: True)

    cog = LobbyCommands(make_bot(match_service=None), lobby_service, FakePlayerService())
    await invoke_reset(cog, interaction)

    assert lobby_service.get_lobby(guild_id=123) is not None
    assert "`lobby` option" in interaction.followup.messages[0]["content"]
