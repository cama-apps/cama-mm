"""Admin-only low-priority command behavior."""

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from commands.admin import AdminCommands


def _interaction(*, admin_id: int = 900, guild_id: int = 77):
    return SimpleNamespace(
        user=SimpleNamespace(id=admin_id, mention=f"<@{admin_id}>"),
        guild=SimpleNamespace(id=guild_id),
        response=SimpleNamespace(send_message=AsyncMock()),
    )


def _commands(*, player_exists: bool = True):
    player_service = MagicMock()
    player_service.get_player.return_value = object() if player_exists else None
    repo = MagicMock()
    repo.REQUIRED_WINS = 3
    return (
        AdminCommands(
            bot=None,
            lobby_service=None,
            player_service=player_service,
            low_priority_repo=repo,
        ),
        player_service,
        repo,
    )


def test_low_priority_commands_require_manage_guild_by_default():
    for command in (
        AdminCommands.lowprio_add,
        AdminCommands.lowprio_remove,
        AdminCommands.lowprio_status,
        AdminCommands.lowprio_list,
    ):
        assert command.default_permissions.manage_guild is True


@pytest.mark.asyncio
async def test_lowprio_add_sets_three_win_state_ephemerally(monkeypatch):
    commands, player_service, repo = _commands()
    interaction = _interaction()
    target = SimpleNamespace(id=42, mention="<@42>")
    repo.set_low_priority.return_value = SimpleNamespace(wins_remaining=3)
    monkeypatch.setattr("commands.admin.has_admin_permission", lambda _interaction: True)

    await commands.lowprio_add.callback(
        commands,
        interaction,
        target,
        reason="internal",
    )

    player_service.get_player.assert_called_once_with(42, 77)
    repo.set_low_priority.assert_called_once_with(
        42,
        77,
        set_by=900,
        reason="internal",
    )
    interaction.response.send_message.assert_awaited_once()
    assert "3 wins required" in interaction.response.send_message.call_args.args[0]
    assert interaction.response.send_message.call_args.kwargs["ephemeral"] is True


@pytest.mark.asyncio
async def test_lowprio_runtime_admin_check_blocks_repository_access(monkeypatch):
    commands, player_service, repo = _commands()
    interaction = _interaction()
    target = SimpleNamespace(id=42, mention="<@42>")
    monkeypatch.setattr("commands.admin.has_admin_permission", lambda _interaction: False)

    await commands.lowprio_add.callback(commands, interaction, target)

    player_service.get_player.assert_not_called()
    repo.set_low_priority.assert_not_called()
    assert interaction.response.send_message.call_args.kwargs["ephemeral"] is True


@pytest.mark.asyncio
async def test_lowprio_status_reports_progress_ephemerally(monkeypatch):
    commands, _player_service, repo = _commands()
    interaction = _interaction()
    target = SimpleNamespace(id=42, mention="<@42>")
    repo.get_state.return_value = SimpleNamespace(active=True, wins_remaining=2)
    monkeypatch.setattr("commands.admin.has_admin_permission", lambda _interaction: True)

    await commands.lowprio_status.callback(commands, interaction, target)

    repo.get_state.assert_called_once_with(42, 77)
    message = interaction.response.send_message.call_args.args[0]
    assert "1/3 wins completed (2 remaining)" in message
    assert interaction.response.send_message.call_args.kwargs["ephemeral"] is True
