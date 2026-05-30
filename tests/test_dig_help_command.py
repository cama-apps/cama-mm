from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock

import pytest

import commands.dig as dig_commands


def _interaction() -> SimpleNamespace:
    return SimpleNamespace(
        guild=SimpleNamespace(id=12345),
        user=SimpleNamespace(id=10001),
        channel=SimpleNamespace(id=999),
    )


@pytest.mark.asyncio
async def test_dig_help_stops_when_defer_fails(monkeypatch):
    monkeypatch.setattr(
        dig_commands, "require_dig_channel", AsyncMock(return_value=True)
    )
    safe_defer = AsyncMock(return_value=False)
    safe_followup = AsyncMock()
    monkeypatch.setattr(dig_commands, "safe_defer", safe_defer)
    monkeypatch.setattr(dig_commands, "safe_followup", safe_followup)

    player_service = SimpleNamespace(get_player=Mock(return_value=object()))
    dig_service = SimpleNamespace(help_tunnel=Mock(return_value={"success": True}))
    cog = dig_commands.DigCommands(
        SimpleNamespace(player_service=player_service), dig_service
    )

    await cog.dig_help.callback(
        cog,
        _interaction(),
        SimpleNamespace(id=10002, display_name="Target"),
    )

    safe_defer.assert_awaited_once()
    player_service.get_player.assert_not_called()
    dig_service.help_tunnel.assert_not_called()
    safe_followup.assert_not_called()


@pytest.mark.asyncio
async def test_dig_help_registration_error_uses_deferred_followup(monkeypatch):
    monkeypatch.setattr(
        dig_commands, "require_dig_channel", AsyncMock(return_value=True)
    )
    safe_defer = AsyncMock(return_value=True)
    safe_followup = AsyncMock()
    monkeypatch.setattr(dig_commands, "safe_defer", safe_defer)
    monkeypatch.setattr(dig_commands, "safe_followup", safe_followup)

    player_service = SimpleNamespace(get_player=Mock(return_value=None))
    dig_service = SimpleNamespace(help_tunnel=Mock(return_value={"success": True}))
    cog = dig_commands.DigCommands(
        SimpleNamespace(player_service=player_service), dig_service
    )

    await cog.dig_help.callback(
        cog,
        _interaction(),
        SimpleNamespace(id=10002, display_name="Target"),
    )

    safe_defer.assert_awaited_once()
    player_service.get_player.assert_called_once_with(10001, 12345)
    dig_service.help_tunnel.assert_not_called()
    safe_followup.assert_awaited_once()
    assert safe_followup.await_args.kwargs["ephemeral"] is True
