"""Private `/player lobby status` command behavior."""

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from commands.registration import RegistrationCommands


def _interaction(*, user_id: int = 42, guild_id: int = 77):
    return SimpleNamespace(
        user=SimpleNamespace(id=user_id, mention=f"<@{user_id}>"),
        guild=SimpleNamespace(id=guild_id),
        response=SimpleNamespace(send_message=AsyncMock()),
    )


def test_player_lobby_status_has_no_target_player_option():
    assert RegistrationCommands.lobby_status.parameters == []


@pytest.mark.asyncio
async def test_player_lobby_status_queries_caller_and_keeps_reasons_private(monkeypatch):
    moderation_service = MagicMock()
    moderation_service.get_active_suspension.return_value = SimpleNamespace(
        scope="open",
        completion="both",
        expires_at=9_000,
        matches_remaining=2,
        reason="repeat abandonment",
    )
    low_priority_repo = MagicMock()
    low_priority_repo.get_state.return_value = SimpleNamespace(
        active=True,
        wins_required=7,
        wins_remaining=3,
        reason="missed ready checks",
    )
    bot = SimpleNamespace(
        moderation_service=moderation_service,
        low_priority_repo=low_priority_repo,
    )
    commands = RegistrationCommands(bot=bot, player_service=MagicMock())
    interaction = _interaction()
    safe_defer = AsyncMock(return_value=True)
    safe_followup = AsyncMock()
    monkeypatch.setattr("commands.registration.safe_defer", safe_defer)
    monkeypatch.setattr("commands.registration.safe_followup", safe_followup)

    await commands.lobby_status.callback(commands, interaction)

    moderation_service.get_active_suspension.assert_called_once_with(42, 77)
    low_priority_repo.get_state.assert_called_once_with(42, 77)
    safe_defer.assert_awaited_once_with(interaction, ephemeral=True)
    safe_followup.assert_awaited_once()
    assert safe_followup.await_args.kwargs["ephemeral"] is True
    message = safe_followup.await_args.kwargs["content"]
    assert "Lobby suspension" in message
    assert "All You Can Feed" in message
    assert "<t:9000:F>" in message
    assert "and 2 completed matches remaining" in message
    assert "repeat abandonment" in message
    assert "Progress: 4/7 wins (3 remaining)" in message
    assert "missed ready checks" in message


@pytest.mark.asyncio
async def test_player_lobby_status_reports_clear_state_ephemerally(monkeypatch):
    moderation_service = MagicMock()
    moderation_service.get_active_suspension.return_value = None
    low_priority_repo = MagicMock()
    low_priority_repo.get_state.return_value = None
    bot = SimpleNamespace(
        moderation_service=moderation_service,
        low_priority_repo=low_priority_repo,
    )
    commands = RegistrationCommands(bot=bot, player_service=MagicMock())
    interaction = _interaction(user_id=88, guild_id=99)
    monkeypatch.setattr(
        "commands.registration.safe_defer",
        AsyncMock(return_value=True),
    )
    safe_followup = AsyncMock()
    monkeypatch.setattr("commands.registration.safe_followup", safe_followup)

    await commands.lobby_status.callback(commands, interaction)

    moderation_service.get_active_suspension.assert_called_once_with(88, 99)
    low_priority_repo.get_state.assert_called_once_with(88, 99)
    assert "no active lobby suspension or low-priority state" in (
        safe_followup.await_args.kwargs["content"]
    )
    assert safe_followup.await_args.kwargs["ephemeral"] is True
