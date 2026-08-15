"""Integration test: /lobby join is blocked while a player is inside their curfew.

The wall-clock check itself is covered by tests/test_curfew.py; here we
monkeypatch commands.lobby.is_within_curfew so the join-command wiring is
verified without depending on what time the suite happens to run.
"""

import pytest

from domain.models.lobby import LobbyKind
from tests.conftest import TEST_GUILD_ID
from tests.test_lobby_commands_guild_id import (
    FakeBot,
    FakeInteraction,
    make_services,
    monkeypatch_safe_defer,  # noqa: F401  (fixture)
)


@pytest.mark.asyncio
async def test_join_blocked_during_curfew_window(monkeypatch, monkeypatch_safe_defer):  # noqa: F811
    from commands.lobby import LobbyCommands

    monkeypatch.setattr("commands.lobby.is_within_curfew", lambda player: True)

    _, lobby_service, player_service, player_repo = make_services()
    lobby_service.get_or_create_lobby(
        creator_id=99, guild_id=TEST_GUILD_ID, lobby_kind=LobbyKind.OPEN
    )
    player_repo.add_player(1, TEST_GUILD_ID)

    interaction = FakeInteraction(user_id=1, guild_id=TEST_GUILD_ID)
    cog = LobbyCommands(FakeBot(), lobby_service, player_service)

    await cog.join.callback(cog, interaction, None)

    assert 1 not in lobby_service.get_lobby(
        guild_id=TEST_GUILD_ID, lobby_kind=LobbyKind.OPEN
    ).players
    message = interaction.followup.messages[-1]["content"]
    assert "bedtime" in message.lower()


@pytest.mark.asyncio
async def test_join_allowed_outside_curfew_window(monkeypatch, monkeypatch_safe_defer):  # noqa: F811
    from commands.lobby import LobbyCommands

    monkeypatch.setattr("commands.lobby.is_within_curfew", lambda player: False)

    _, lobby_service, player_service, player_repo = make_services()
    lobby_service.get_or_create_lobby(
        creator_id=99, guild_id=TEST_GUILD_ID, lobby_kind=LobbyKind.OPEN
    )
    player_repo.add_player(1, TEST_GUILD_ID)

    interaction = FakeInteraction(user_id=1, guild_id=TEST_GUILD_ID)
    cog = LobbyCommands(FakeBot(), lobby_service, player_service)

    await cog.join.callback(cog, interaction, None)

    assert 1 in lobby_service.get_lobby(
        guild_id=TEST_GUILD_ID, lobby_kind=LobbyKind.OPEN
    ).players
