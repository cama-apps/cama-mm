"""Focused command tests for temporary lobby moderation."""

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest
from discord import app_commands

from commands.admin import AdminCommands
from domain.models.lobby import LobbyKind
from utils.lobby_selection import format_lobby_labels


def _interaction(*, admin_id: int = 900, guild_id: int = 77):
    return SimpleNamespace(
        user=SimpleNamespace(id=admin_id, mention=f"<@{admin_id}>"),
        guild=SimpleNamespace(id=guild_id),
        response=SimpleNamespace(send_message=AsyncMock()),
    )


def _target(*, discord_id: int = 42):
    return SimpleNamespace(
        id=discord_id,
        mention=f"<@{discord_id}>",
        display_name="Target Player",
        bot=False,
        send=AsyncMock(),
    )


def _state(**overrides):
    values = {
        "discord_id": 42,
        "scope": "all",
        "completion": "time",
        "expires_at": 8_200,
        "matches_required": None,
        "matches_remaining": None,
        "reason": "repeat abandonment",
        "actor_id": 900,
        "created_at": 1_000,
    }
    values.update(overrides)
    return SimpleNamespace(**values)


def _commands(lobby_cog=None):
    moderation_service = MagicMock()
    lobby_service = MagicMock()
    lobby_service.get_lobby_kinds_for_player.return_value = []
    bot = SimpleNamespace(get_cog=MagicMock(return_value=lobby_cog))
    commands = AdminCommands(
        bot=bot,
        lobby_service=lobby_service,
        player_service=MagicMock(),
        moderation_service=moderation_service,
    )
    return commands, moderation_service, lobby_service


def test_moderation_commands_require_manage_guild_by_default():
    for command in (
        AdminCommands.moderation_suspend,
        AdminCommands.moderation_lift,
        AdminCommands.moderation_status,
        AdminCommands.moderation_list,
        AdminCommands.moderation_history,
    ):
        assert command.default_permissions.manage_guild is True


def test_suspend_exposes_bounded_terms_and_required_reason():
    parameters = {
        parameter.name: parameter
        for parameter in AdminCommands.moderation_suspend.parameters
    }

    assert parameters["reason"].required is True
    assert parameters["reason"].min_value == 3
    assert parameters["reason"].max_value == 300
    assert parameters["matches"].min_value == 1
    assert parameters["matches"].max_value == 20


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("command_name", "takes_player", "kwargs"),
    [
        ("moderation_suspend", True, {"reason": "abuse", "matches": 2}),
        ("moderation_lift", True, {"reason": "appeal accepted"}),
        ("moderation_status", True, {}),
        ("moderation_list", False, {}),
        ("moderation_history", False, {}),
    ],
)
async def test_moderation_runtime_permission_blocks_service_access(
    monkeypatch,
    command_name,
    takes_player,
    kwargs,
):
    commands, moderation_service, lobby_service = _commands()
    interaction = _interaction()
    monkeypatch.setattr("commands.admin.has_admin_permission", lambda _interaction: False)

    args = [_target()] if takes_player else []
    command = getattr(commands, command_name)
    await command.callback(commands, interaction, *args, **kwargs)

    moderation_service.assert_not_called()
    assert moderation_service.mock_calls == []
    assert lobby_service.mock_calls == []
    interaction.response.send_message.assert_awaited_once()
    assert interaction.response.send_message.await_args.kwargs["ephemeral"] is True
    assert "Admin only" in interaction.response.send_message.await_args.args[0]


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("kwargs", "expected"),
    [
        ({}, "at least one term"),
        ({"duration": "2h", "matches": 3}, "Choose whether"),
        (
            {
                "duration": "2h",
                "completion": app_commands.Choice(name="Either", value="either"),
            },
            "only used when both",
        ),
    ],
)
async def test_suspend_rejects_incoherent_terms_before_deferring(
    monkeypatch,
    kwargs,
    expected,
):
    commands, moderation_service, _lobby_service = _commands()
    interaction = _interaction()
    safe_defer = AsyncMock(return_value=True)
    monkeypatch.setattr("commands.admin.has_admin_permission", lambda _interaction: True)
    monkeypatch.setattr("commands.admin.safe_defer", safe_defer)

    await commands.moderation_suspend.callback(
        commands,
        interaction,
        _target(),
        reason="repeat abandonment",
        **kwargs,
    )

    safe_defer.assert_not_awaited()
    moderation_service.create_suspension.assert_not_called()
    moderation_service.replace_suspension.assert_not_called()
    message = interaction.response.send_message.await_args.args[0]
    assert expected in message
    assert interaction.response.send_message.await_args.kwargs["ephemeral"] is True


@pytest.mark.asyncio
async def test_suspend_surfaces_duration_validation_privately(monkeypatch):
    commands, moderation_service, _lobby_service = _commands()
    interaction = _interaction()
    moderation_service.parse_expiry.side_effect = ValueError(
        "Duration must be between 5 minutes and 365 days"
    )
    safe_defer = AsyncMock(return_value=True)
    monkeypatch.setattr("commands.admin.has_admin_permission", lambda _interaction: True)
    monkeypatch.setattr("commands.admin.safe_defer", safe_defer)

    await commands.moderation_suspend.callback(
        commands,
        interaction,
        _target(),
        reason="repeat abandonment",
        duration="forever",
    )

    moderation_service.parse_expiry.assert_called_once()
    safe_defer.assert_not_awaited()
    moderation_service.create_suspension.assert_not_called()
    message = interaction.response.send_message.await_args.args[0]
    assert "5 minutes and 365 days" in message
    assert interaction.response.send_message.await_args.kwargs["ephemeral"] is True


@pytest.mark.asyncio
async def test_suspend_creates_exact_time_scoped_state_and_notifies_privately(
    monkeypatch,
):
    commands, moderation_service, lobby_service = _commands()
    interaction = _interaction()
    player = _target()
    state = _state()
    moderation_service.parse_expiry.return_value = 8_200
    moderation_service.get_active_suspension.return_value = None
    moderation_service.create_suspension.return_value = state
    safe_defer = AsyncMock(return_value=True)
    safe_followup = AsyncMock()
    monkeypatch.setattr("commands.admin.has_admin_permission", lambda _interaction: True)
    monkeypatch.setattr("commands.admin.safe_defer", safe_defer)
    monkeypatch.setattr("commands.admin.safe_followup", safe_followup)
    monkeypatch.setattr("commands.admin.time.time", lambda: 1_000)

    await commands.moderation_suspend.callback(
        commands,
        interaction,
        player,
        reason="repeat abandonment",
        duration="2h",
    )

    moderation_service.parse_expiry.assert_called_once_with("2h", now=1_000)
    moderation_service.create_suspension.assert_called_once_with(
        42,
        77,
        actor_id=900,
        reason="repeat abandonment",
        scope="all",
        completion="time",
        expires_at=8_200,
        matches=None,
        source="admin",
        now=1_000,
    )
    moderation_service.replace_suspension.assert_not_called()
    lobby_service.get_lobby_kinds_for_player.assert_called_once_with(42, 77)
    player.send.assert_awaited_once()
    assert "repeat abandonment" in player.send.await_args.args[0]
    assert "<t:8200:F>" in player.send.await_args.args[0]
    safe_defer.assert_awaited_once_with(interaction, ephemeral=True)
    assert safe_followup.await_args.kwargs["ephemeral"] is True
    assert "Suspended <@42>" in safe_followup.await_args.kwargs["content"]


@pytest.mark.asyncio
async def test_all_scope_suspension_evicts_every_queued_lobby(monkeypatch):
    """A player queued in both lobbies loses both seats to an all-lobbies term."""
    lobby_cog = SimpleNamespace(eject_lobby_player=AsyncMock(return_value=True))
    commands, moderation_service, lobby_service = _commands(lobby_cog=lobby_cog)
    interaction = _interaction()
    player = _target()
    lobby_service.get_lobby_kinds_for_player.return_value = [
        LobbyKind.OPEN,
        LobbyKind.LOWSKILL,
    ]
    moderation_service.parse_expiry.return_value = 8_200
    moderation_service.create_suspension.return_value = _state()
    # An "all" scope is active against whichever lobby it is checked against.
    moderation_service.get_active_suspension.return_value = _state()
    safe_followup = AsyncMock()
    monkeypatch.setattr("commands.admin.has_admin_permission", lambda _interaction: True)
    monkeypatch.setattr("commands.admin.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.admin.safe_followup", safe_followup)
    monkeypatch.setattr("commands.admin.time.time", lambda: 1_000)

    await commands.moderation_suspend.callback(
        commands,
        interaction,
        player,
        reason="repeat abandonment",
        duration="2h",
    )

    evicted_kinds = [
        call.kwargs["lobby_kind"]
        for call in lobby_cog.eject_lobby_player.await_args_list
    ]
    assert evicted_kinds == [LobbyKind.OPEN, LobbyKind.LOWSKILL]
    lobby_service.leave_lobby.assert_not_called()
    # The confirmation names both lobbies it actually cleared.
    content = safe_followup.await_args.kwargs["content"]
    assert f"Removed from {format_lobby_labels(evicted_kinds)}." in content
    assert "already-starting match" not in content


@pytest.mark.asyncio
async def test_lowskill_scoped_suspension_leaves_the_open_seat_intact(monkeypatch):
    """A kind-scoped term clears its own lobby and only its own lobby."""
    lobby_cog = SimpleNamespace(eject_lobby_player=AsyncMock(return_value=True))
    commands, moderation_service, lobby_service = _commands(lobby_cog=lobby_cog)
    interaction = _interaction()
    player = _target()
    lobby_service.get_lobby_kinds_for_player.return_value = [
        LobbyKind.OPEN,
        LobbyKind.LOWSKILL,
    ]
    state = _state(scope="lowskill")
    moderation_service.parse_expiry.return_value = 8_200
    moderation_service.create_suspension.return_value = state
    moderation_service.get_active_suspension.side_effect = (
        lambda _discord_id, _guild_id, kind: state if kind is LobbyKind.LOWSKILL else None
    )
    safe_followup = AsyncMock()
    monkeypatch.setattr("commands.admin.has_admin_permission", lambda _interaction: True)
    monkeypatch.setattr("commands.admin.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.admin.safe_followup", safe_followup)
    monkeypatch.setattr("commands.admin.time.time", lambda: 1_000)

    await commands.moderation_suspend.callback(
        commands,
        interaction,
        player,
        reason="repeat abandonment",
        duration="2h",
        lobby=app_commands.Choice(name="Whine & Cheese", value="lowskill"),
    )

    moderation_service.create_suspension.assert_called_once()
    assert moderation_service.create_suspension.call_args.kwargs["scope"] == "lowskill"
    lobby_cog.eject_lobby_player.assert_awaited_once()
    assert (
        lobby_cog.eject_lobby_player.await_args.kwargs["lobby_kind"]
        is LobbyKind.LOWSKILL
    )
    content = safe_followup.await_args.kwargs["content"]
    assert f"Removed from {LobbyKind.LOWSKILL.label}." in content
    assert LobbyKind.OPEN.label not in content.split("Removed from")[-1]


@pytest.mark.asyncio
async def test_suspend_without_replace_flag_reports_existing_suspension(monkeypatch):
    """An active suspension is never silently overwritten: the admin gets the
    existing terms back and must explicitly pass ``replace: True``."""
    from domain.models.moderation import ActiveSuspensionExistsError

    commands, moderation_service, _lobby_service = _commands()
    interaction = _interaction()
    player = _target()
    existing = _state()
    moderation_service.create_suspension.side_effect = ActiveSuspensionExistsError(
        "Player already has an active lobby suspension"
    )
    moderation_service.get_active_suspension.return_value = existing
    safe_followup = AsyncMock()
    monkeypatch.setattr("commands.admin.has_admin_permission", lambda _interaction: True)
    monkeypatch.setattr("commands.admin.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.admin.safe_followup", safe_followup)
    monkeypatch.setattr("commands.admin.time.time", lambda: 2_000)

    await commands.moderation_suspend.callback(
        commands,
        interaction,
        player,
        reason="new exact term",
        matches=5,
        lobby=app_commands.Choice(name="All You Can Feed", value="open"),
    )

    moderation_service.create_suspension.assert_called_once()
    moderation_service.replace_suspension.assert_not_called()
    player.send.assert_not_awaited()
    content = safe_followup.await_args.kwargs["content"]
    assert "already has an active suspension" in content
    assert "<t:8200:F>" in content
    assert "repeat abandonment" in content
    assert "replace: True" in content
    assert safe_followup.await_args.kwargs["ephemeral"] is True


@pytest.mark.asyncio
async def test_suspend_replaces_active_state_only_with_explicit_flag(monkeypatch):
    commands, moderation_service, _lobby_service = _commands()
    interaction = _interaction()
    player = _target()
    existing = _state()
    replacement = _state(
        scope="open",
        completion="matches",
        expires_at=None,
        matches_required=5,
        matches_remaining=5,
    )
    moderation_service.get_active_suspension.return_value = existing
    moderation_service.replace_suspension.return_value = replacement
    safe_followup = AsyncMock()
    monkeypatch.setattr("commands.admin.has_admin_permission", lambda _interaction: True)
    monkeypatch.setattr("commands.admin.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.admin.safe_followup", safe_followup)
    monkeypatch.setattr("commands.admin.time.time", lambda: 2_000)

    await commands.moderation_suspend.callback(
        commands,
        interaction,
        player,
        reason="new exact term",
        matches=5,
        lobby=app_commands.Choice(name="All You Can Feed", value="open"),
        replace=True,
    )

    moderation_service.replace_suspension.assert_called_once_with(
        42,
        77,
        actor_id=900,
        reason="new exact term",
        scope="open",
        completion="matches",
        expires_at=None,
        matches=5,
        source="admin",
        now=2_000,
    )
    moderation_service.create_suspension.assert_not_called()
    assert "Replaced the active suspension" in safe_followup.await_args.kwargs[
        "content"
    ]
    assert "5 completed matches remaining" in safe_followup.await_args.kwargs[
        "content"
    ]
    assert safe_followup.await_args.kwargs["ephemeral"] is True


@pytest.mark.asyncio
async def test_moderation_lift_records_actor_reason_and_dms_target(monkeypatch):
    commands, moderation_service, _lobby_service = _commands()
    interaction = _interaction()
    player = _target()
    moderation_service.lift_suspension.return_value = _state()
    monkeypatch.setattr("commands.admin.has_admin_permission", lambda _interaction: True)

    await commands.moderation_lift.callback(
        commands,
        interaction,
        player,
        reason="appeal accepted",
    )

    moderation_service.lift_suspension.assert_called_once_with(
        42,
        77,
        actor_id=900,
        reason="appeal accepted",
        source="admin",
    )
    player.send.assert_awaited_once()
    assert "appeal accepted" in player.send.await_args.args[0]
    message = interaction.response.send_message.await_args.args[0]
    assert "Lifted <@42>" in message
    assert "appeal accepted" in message
    assert interaction.response.send_message.await_args.kwargs["ephemeral"] is True


@pytest.mark.asyncio
async def test_moderation_status_shows_private_scope_terms_and_audit_fields(monkeypatch):
    commands, moderation_service, _lobby_service = _commands()
    interaction = _interaction()
    moderation_service.get_active_suspension.return_value = _state(
        scope="lowskill",
        completion="both",
        expires_at=9_000,
        matches_required=4,
        matches_remaining=2,
    )
    monkeypatch.setattr("commands.admin.has_admin_permission", lambda _interaction: True)

    await commands.moderation_status.callback(
        commands,
        interaction,
        _target(),
    )

    moderation_service.get_active_suspension.assert_called_once_with(42, 77)
    message = interaction.response.send_message.await_args.args[0]
    assert "Whine & Cheese" in message
    assert "<t:9000:F>" in message
    assert "and 2 completed matches remaining" in message
    assert "repeat abandonment" in message
    assert "<@900>" in message
    assert "<t:1000:F>" in message
    assert interaction.response.send_message.await_args.kwargs["ephemeral"] is True
