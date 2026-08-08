import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest
from discord import app_commands

from commands.match import MatchCommands
from domain.models.lobby import LobbyKind
from domain.models.pending_match_state import PendingMatchState
from tests.conftest import TEST_GUILD_ID
from utils.lobby_selection import AMBIGUOUS_LOBBY_MESSAGE


def _make_interaction(user_id: int):
    return SimpleNamespace(
        user=SimpleNamespace(id=user_id),
        guild=None,
        followup=SimpleNamespace(send=AsyncMock()),
    )


def _make_cog(lobby, *, admin: bool = False, monkeypatch):
    bot = MagicMock()
    bot.draft_state_manager = None

    lobby_service = MagicMock()
    lobby_service.get_lobby.return_value = lobby

    match_service = MagicMock()
    match_service.state_service.get_pending_match_for_player.return_value = None

    monkeypatch.setattr("commands.match.has_admin_permission", lambda _interaction: admin)

    return MatchCommands(bot, lobby_service, match_service, MagicMock())


def _ready_lobby(*, players=None, conditional_players=None):
    players = set(players or [])
    conditional_players = set(conditional_players or [])
    return SimpleNamespace(
        kind="open",
        players=players,
        conditional_players=conditional_players,
        get_player_count=lambda: len(players),
        get_conditional_count=lambda: len(conditional_players),
        get_total_count=lambda: len(players) + len(conditional_players),
    )


@pytest.mark.asyncio
async def test_shuffle_preconditions_allow_regular_lobby_member(monkeypatch):
    lobby = _ready_lobby(players=range(1, 11))
    cog = _make_cog(lobby, monkeypatch=monkeypatch)
    interaction = _make_interaction(1)

    result = await cog._validate_shuffle_preconditions(interaction, TEST_GUILD_ID)

    assert result is lobby
    interaction.followup.send.assert_not_called()


@pytest.mark.asyncio
async def test_shuffle_preconditions_reject_legacy_conditional_lobby_member(monkeypatch):
    lobby = _ready_lobby(players=range(1, 10), conditional_players={99})
    cog = _make_cog(lobby, monkeypatch=monkeypatch)
    interaction = _make_interaction(99)

    result = await cog._validate_shuffle_preconditions(interaction, TEST_GUILD_ID)

    assert result is None
    interaction.followup.send.assert_awaited_once_with(
        "❌ Join 🍽️ All You Can Feed before using `/shuffle`.",
        ephemeral=True,
    )


@pytest.mark.asyncio
async def test_shuffle_preconditions_reject_admin_outside_lobby(monkeypatch):
    lobby = _ready_lobby(players=range(1, 11))
    cog = _make_cog(lobby, admin=True, monkeypatch=monkeypatch)
    interaction = _make_interaction(42)

    result = await cog._validate_shuffle_preconditions(interaction, TEST_GUILD_ID)

    assert result is None
    interaction.followup.send.assert_awaited_once_with(
        "❌ Join 🍽️ All You Can Feed before using `/shuffle`.",
        ephemeral=True,
    )


@pytest.mark.asyncio
async def test_shuffle_preconditions_reject_non_admin_outside_lobby(monkeypatch):
    lobby = _ready_lobby(players=range(1, 11))
    cog = _make_cog(lobby, monkeypatch=monkeypatch)
    interaction = _make_interaction(42)

    result = await cog._validate_shuffle_preconditions(interaction, TEST_GUILD_ID)

    assert result is None
    interaction.followup.send.assert_awaited_once_with(
        "❌ Join 🍽️ All You Can Feed before using `/shuffle`.",
        ephemeral=True,
    )


@pytest.mark.asyncio
async def test_shuffle_preconditions_reject_player_already_in_pending_match(monkeypatch):
    """The pending-match branch reads a PendingMatchState, which has no .get()."""
    lobby = _ready_lobby(players=range(1, 11))
    cog = _make_cog(lobby, monkeypatch=monkeypatch)
    cog.match_service.state_service.get_pending_match_for_player.return_value = (
        PendingMatchState(
            pending_match_id=77,
            shuffle_message_jump_url="https://discord.com/channels/1/2/3",
        )
    )
    interaction = _make_interaction(1)

    result = await cog._validate_shuffle_preconditions(interaction, TEST_GUILD_ID)

    assert result is None
    interaction.followup.send.assert_awaited_once_with(
        "❌ You're already in a pending match (Match #77)! "
        "[View your match](https://discord.com/channels/1/2/3) "
        "and use `/record` to complete it first.",
        ephemeral=True,
    )


@pytest.mark.asyncio
async def test_active_draft_only_blocks_its_source_lobby(monkeypatch):
    lobby = _ready_lobby(players=range(1, 11))
    cog = _make_cog(lobby, monkeypatch=monkeypatch)
    cog.bot.draft_state_manager = SimpleNamespace(
        has_active_draft=MagicMock(return_value=True),
        get_state=MagicMock(
            return_value=SimpleNamespace(
                lobby_kind=LobbyKind.OPEN,
                captain1_id=50,
                captain2_id=51,
            )
        ),
    )
    interaction = _make_interaction(1)

    result = await cog._validate_shuffle_preconditions(
        interaction,
        TEST_GUILD_ID,
        lobby_kind=LobbyKind.LOWSKILL,
    )

    assert result is lobby
    interaction.followup.send.assert_not_called()
    cog.lobby_service.get_lobby.assert_called_once_with(
        guild_id=TEST_GUILD_ID,
        lobby_kind=LobbyKind.LOWSKILL,
    )


def _command_interaction(user_id: int):
    """An interaction complete enough to drive the `/shuffle` callback itself."""
    return SimpleNamespace(
        user=SimpleNamespace(id=user_id),
        guild=SimpleNamespace(id=TEST_GUILD_ID),
        response=SimpleNamespace(send_message=AsyncMock()),
        followup=SimpleNamespace(send=AsyncMock()),
    )


def _command_cog(member_kinds):
    lobby_service = MagicMock()
    lobby_service.get_lobby_kinds_for_player.return_value = member_kinds
    lobby_service.lobby_manager.get_shuffle_lock.return_value = asyncio.Lock()
    cog = MatchCommands(MagicMock(), lobby_service, MagicMock(), MagicMock())
    cog._execute_shuffle = AsyncMock()
    return cog


@pytest.mark.asyncio
async def test_shuffle_command_infers_lowskill_membership(monkeypatch):
    interaction = _command_interaction(1)
    cog = _command_cog([LobbyKind.LOWSKILL])
    monkeypatch.setattr("commands.match.safe_defer", AsyncMock(return_value=True))

    await cog.shuffle.callback(cog, interaction)

    cog._execute_shuffle.assert_awaited_once_with(
        interaction,
        interaction.guild,
        TEST_GUILD_ID,
        None,
        None,
        lobby_kind=LobbyKind.LOWSKILL,
    )


@pytest.mark.asyncio
async def test_shuffle_command_requires_a_lobby_when_queued_in_both(monkeypatch):
    """Ambiguity is refused before the lock, so neither lobby is pinned by it."""
    interaction = _command_interaction(2)
    cog = _command_cog([LobbyKind.OPEN, LobbyKind.LOWSKILL])
    monkeypatch.setattr("commands.match.safe_defer", AsyncMock(return_value=True))

    await cog.shuffle.callback(cog, interaction)

    interaction.followup.send.assert_awaited_once_with(
        AMBIGUOUS_LOBBY_MESSAGE,
        ephemeral=True,
    )
    cog.lobby_service.lobby_manager.get_shuffle_lock.assert_not_called()
    cog._execute_shuffle.assert_not_awaited()


@pytest.mark.asyncio
async def test_shuffle_command_honours_explicit_lobby_when_queued_in_both(monkeypatch):
    interaction = _command_interaction(3)
    cog = _command_cog([LobbyKind.OPEN, LobbyKind.LOWSKILL])
    monkeypatch.setattr("commands.match.safe_defer", AsyncMock(return_value=True))

    await cog.shuffle.callback(
        cog,
        interaction,
        lobby=app_commands.Choice(name="Whine & Cheese", value="lowskill"),
    )

    interaction.followup.send.assert_not_called()
    cog._execute_shuffle.assert_awaited_once_with(
        interaction,
        interaction.guild,
        TEST_GUILD_ID,
        None,
        None,
        lobby_kind=LobbyKind.LOWSKILL,
    )


@pytest.mark.asyncio
async def test_active_draft_admin_hint_uses_central_admin_permission(monkeypatch):
    """Allowlisted admins get the /draft restart hint during an active draft.

    The check must go through services.permissions.has_admin_permission —
    bot.admin_user_ids is never set on the bot object, so the old hand-rolled
    check silently treated ADMIN_USER_IDS/manage_guild admins as non-admins.
    """
    lobby = _ready_lobby(players=range(1, 11))
    cog = _make_cog(lobby, admin=True, monkeypatch=monkeypatch)
    cog.bot.draft_state_manager = SimpleNamespace(
        has_active_draft=MagicMock(return_value=True),
        get_state=MagicMock(
            return_value=SimpleNamespace(
                lobby_kind=LobbyKind.OPEN,
                captain1_id=50,
                captain2_id=51,
            )
        ),
    )
    interaction = _make_interaction(42)

    result = await cog._validate_shuffle_preconditions(interaction, TEST_GUILD_ID)

    assert result is None
    (message,), _kwargs = interaction.followup.send.call_args
    assert "/draft restart" in message
