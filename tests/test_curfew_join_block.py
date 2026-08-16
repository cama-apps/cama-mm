"""Integration test: /lobby join is blocked while a player is inside an active curfew window."""

from types import SimpleNamespace

import pytest

from domain.models.lobby import LobbyKind
from tests.conftest import TEST_GUILD_ID
from tests.test_lobby_commands_guild_id import (
    FakeBot,
    FakeInteraction,
    make_services,
    monkeypatch_safe_defer,  # noqa: F401  (fixture)
)


class FakeCurfewService:
    def __init__(self, active_window=None):
        self._active_window = active_window

    def active_window(self, discord_id, guild_id):
        return self._active_window


@pytest.mark.asyncio
async def test_join_blocked_during_active_curfew_window(monkeypatch_safe_defer):  # noqa: F811
    from commands.lobby import LobbyCommands

    _, lobby_service, player_service, player_repo = make_services()
    lobby_service.get_or_create_lobby(
        creator_id=99, guild_id=TEST_GUILD_ID, lobby_kind=LobbyKind.OPEN
    )
    player_repo.add_player(1, TEST_GUILD_ID)

    window = SimpleNamespace(
        name="sleep",
        start_hour=22,
        start_minute=0,
        end_hour=6,
        end_minute=0,
        timezone="America/New_York",
    )
    interaction = FakeInteraction(user_id=1, guild_id=TEST_GUILD_ID)
    cog = LobbyCommands(FakeBot(), lobby_service, player_service, FakeCurfewService(window))

    await cog.join.callback(cog, interaction, None)

    assert 1 not in lobby_service.get_lobby(
        guild_id=TEST_GUILD_ID, lobby_kind=LobbyKind.OPEN
    ).players
    message = interaction.followup.messages[-1]["content"]
    assert "sleep" in message.lower()


@pytest.mark.asyncio
async def test_join_allowed_outside_curfew_window(monkeypatch_safe_defer):  # noqa: F811
    from commands.lobby import LobbyCommands

    _, lobby_service, player_service, player_repo = make_services()
    lobby_service.get_or_create_lobby(
        creator_id=99, guild_id=TEST_GUILD_ID, lobby_kind=LobbyKind.OPEN
    )
    player_repo.add_player(1, TEST_GUILD_ID)

    interaction = FakeInteraction(user_id=1, guild_id=TEST_GUILD_ID)
    cog = LobbyCommands(FakeBot(), lobby_service, player_service, FakeCurfewService(None))

    await cog.join.callback(cog, interaction, None)

    assert 1 in lobby_service.get_lobby(
        guild_id=TEST_GUILD_ID, lobby_kind=LobbyKind.OPEN
    ).players


@pytest.mark.asyncio
async def test_join_allowed_when_curfew_service_unwired(monkeypatch_safe_defer):  # noqa: F811
    """No curfew_service wired at all — join must not crash, just skip the check."""
    from commands.lobby import LobbyCommands

    _, lobby_service, player_service, player_repo = make_services()
    lobby_service.get_or_create_lobby(
        creator_id=99, guild_id=TEST_GUILD_ID, lobby_kind=LobbyKind.OPEN
    )
    player_repo.add_player(1, TEST_GUILD_ID)

    interaction = FakeInteraction(user_id=1, guild_id=TEST_GUILD_ID)
    cog = LobbyCommands(FakeBot(), lobby_service, player_service, None)

    await cog.join.callback(cog, interaction, None)

    assert 1 in lobby_service.get_lobby(
        guild_id=TEST_GUILD_ID, lobby_kind=LobbyKind.OPEN
    ).players
