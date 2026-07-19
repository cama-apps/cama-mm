from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from commands.match import MatchCommands
from tests.conftest import TEST_GUILD_ID


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
        "❌ Only admins or players in the current lobby can shuffle.",
        ephemeral=True,
    )


@pytest.mark.asyncio
async def test_shuffle_preconditions_allow_admin_outside_lobby(monkeypatch):
    lobby = _ready_lobby(players=range(1, 11))
    cog = _make_cog(lobby, admin=True, monkeypatch=monkeypatch)
    interaction = _make_interaction(42)

    result = await cog._validate_shuffle_preconditions(interaction, TEST_GUILD_ID)

    assert result is lobby
    interaction.followup.send.assert_not_called()


@pytest.mark.asyncio
async def test_shuffle_preconditions_reject_non_admin_outside_lobby(monkeypatch):
    lobby = _ready_lobby(players=range(1, 11))
    cog = _make_cog(lobby, monkeypatch=monkeypatch)
    interaction = _make_interaction(42)

    result = await cog._validate_shuffle_preconditions(interaction, TEST_GUILD_ID)

    assert result is None
    interaction.followup.send.assert_awaited_once_with(
        "❌ Only admins or players in the current lobby can shuffle.",
        ephemeral=True,
    )
