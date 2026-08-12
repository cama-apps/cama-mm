"""Admin-only low-priority command behavior."""

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import discord
import pytest
from discord import app_commands

from commands.admin import AdminCommands


def _interaction(*, admin_id: int = 900, guild_id: int = 77):
    return SimpleNamespace(
        user=SimpleNamespace(id=admin_id, mention=f"<@{admin_id}>"),
        guild=SimpleNamespace(id=guild_id),
        response=SimpleNamespace(send_message=AsyncMock()),
    )


def _commands(*, player_exists: bool = True, moderation_service=None):
    player_service = MagicMock()
    player_service.get_player.return_value = object() if player_exists else None
    repo = MagicMock()
    repo.get_state.return_value = None
    return (
        AdminCommands(
            bot=None,
            lobby_service=None,
            player_service=player_service,
            low_priority_repo=repo,
            moderation_service=moderation_service,
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


def test_admin_payload_requires_manage_guild_and_includes_lowprio_group():
    client = discord.Client(intents=discord.Intents.none())
    tree = app_commands.CommandTree(client)

    payload = AdminCommands.admin.to_dict(tree)

    assert str(payload["default_member_permissions"]) == str(
        discord.Permissions(manage_guild=True).value
    )
    assert any(option["name"] == "lowprio" for option in payload["options"])


def test_lowprio_add_exposes_bounded_configurable_win_count():
    parameters = {parameter.name: parameter for parameter in AdminCommands.lowprio_add.parameters}

    assert parameters["wins"].default == 3
    assert parameters["wins"].min_value == 1
    assert parameters["wins"].max_value == 20


@pytest.mark.asyncio
async def test_lowprio_add_sets_configurable_wins_and_pending_match_watermark_without_dm(
    monkeypatch,
):
    moderation_service = MagicMock()
    moderation_service.get_pending_match_watermark.return_value = 314
    commands, player_service, repo = _commands(
        moderation_service=moderation_service,
    )
    interaction = _interaction()
    target = SimpleNamespace(id=42, mention="<@42>", send=AsyncMock())
    repo.set_low_priority.return_value = SimpleNamespace(
        wins_required=7,
        wins_remaining=7,
    )
    monkeypatch.setattr("commands.admin.has_admin_permission", lambda _interaction: True)

    await commands.lowprio_add.callback(
        commands,
        interaction,
        target,
        reason="internal",
        wins=7,
    )

    player_service.get_player.assert_called_once_with(42, 77)
    moderation_service.get_pending_match_watermark.assert_called_once_with(77)
    repo.set_low_priority.assert_called_once_with(
        42,
        77,
        set_by=900,
        reason="internal",
        wins_required=7,
        start_pending_match_id=314,
    )
    moderation_service.record_low_priority_event.assert_called_once_with(
        42,
        77,
        event_type="lowprio_assign",
        wins_required=7,
        wins_remaining=7,
        pending_match_watermark=314,
        actor_id=900,
        reason="internal",
        source="admin",
    )
    target.send.assert_not_awaited()
    interaction.response.send_message.assert_awaited_once()
    assert "7 wins required" in interaction.response.send_message.call_args.args[0]
    assert interaction.response.send_message.call_args.kwargs["ephemeral"] is True


@pytest.mark.asyncio
async def test_lowprio_remove_clears_state_and_records_audit_without_dm(monkeypatch):
    moderation_service = MagicMock()
    commands, _player_service, repo = _commands(
        moderation_service=moderation_service,
    )
    interaction = _interaction()
    target = SimpleNamespace(id=42, mention="<@42>", send=AsyncMock())
    repo.clear_low_priority.return_value = True
    repo.get_state.return_value = SimpleNamespace(
        wins_required=7,
        start_pending_match_id=314,
    )
    monkeypatch.setattr("commands.admin.has_admin_permission", lambda _interaction: True)

    await commands.lowprio_remove.callback(
        commands,
        interaction,
        target,
        reason="internal",
    )

    repo.clear_low_priority.assert_called_once_with(
        42,
        77,
        removed_by=900,
        reason="internal",
    )
    moderation_service.record_low_priority_event.assert_called_once_with(
        42,
        77,
        event_type="lowprio_clear",
        wins_required=7,
        wins_remaining=0,
        actor_id=900,
        reason="internal",
        pending_match_watermark=314,
        source="admin",
    )
    target.send.assert_not_awaited()
    interaction.response.send_message.assert_awaited_once()
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
    repo.get_state.return_value = SimpleNamespace(
        active=True,
        wins_required=5,
        wins_remaining=2,
        reason="repeat abandonment",
        set_by=901,
        created_at="2026-08-05 12:34:56",
    )
    monkeypatch.setattr("commands.admin.has_admin_permission", lambda _interaction: True)

    await commands.lowprio_status.callback(commands, interaction, target)

    repo.get_state.assert_called_once_with(42, 77)
    message = interaction.response.send_message.call_args.args[0]
    assert "3/5 wins completed (2 remaining)" in message
    assert "repeat abandonment" in message
    assert "<@901>" in message
    assert "2026-08-05 12:34:56" in message
    assert interaction.response.send_message.call_args.kwargs["ephemeral"] is True
