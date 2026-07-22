import logging
from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock

import pytest

import commands.dig as dig_module
from commands.dig import DigCommands


@pytest.mark.asyncio
async def test_schedule_dig_reminder_delegates_to_reconciliation():
    reconcile_dig_reminder = AsyncMock()
    bot = SimpleNamespace(
        reminder_service=SimpleNamespace(
            reconcile_dig_reminder=reconcile_dig_reminder,
        )
    )
    cog = DigCommands(bot, SimpleNamespace())

    await cog._schedule_dig_reminder(10001, 12345)

    reconcile_dig_reminder.assert_awaited_once_with(bot, 10001, 12345)


@pytest.mark.asyncio
async def test_schedule_dig_reminder_reuses_ready_timestamp():
    reconcile_dig_reminder = AsyncMock()
    bot = SimpleNamespace(
        reminder_service=SimpleNamespace(
            reconcile_dig_reminder=reconcile_dig_reminder,
        )
    )
    cog = DigCommands(bot, SimpleNamespace())

    await cog._schedule_dig_reminder(10001, 12345, 1_000_500)

    reconcile_dig_reminder.assert_awaited_once_with(
        bot,
        10001,
        12345,
        ready_at=1_000_500,
    )


@pytest.mark.asyncio
async def test_run_dig_reuses_registration_check_and_embedded_notice():
    dig = Mock(
        return_value={
            "success": True,
            "relic_trim_notice": False,
            "next_free_dig_at": 1_000_500,
        }
    )
    dig_service = SimpleNamespace(
        dig=dig,
        pop_relic_trim_notice=Mock(),
    )
    cog = DigCommands(SimpleNamespace(), dig_service)

    result = await cog._run_dig(
        10001,
        12345,
        paid=True,
        player_verified=True,
    )

    assert result.next_free_dig_at == 1_000_500
    dig.assert_called_once_with(
        10001,
        12345,
        paid=True,
        player_verified=True,
    )
    dig_service.pop_relic_trim_notice.assert_not_called()


@pytest.mark.asyncio
async def test_schedule_dig_reminder_is_noop_without_service():
    cog = DigCommands(SimpleNamespace(), SimpleNamespace())

    await cog._schedule_dig_reminder(10001, 12345)


@pytest.mark.asyncio
async def test_reconciliation_failure_does_not_interrupt_dig_and_warns(caplog):
    reconcile_dig_reminder = AsyncMock(
        side_effect=RuntimeError("reconciliation failed"),
    )
    bot = SimpleNamespace(
        reminder_service=SimpleNamespace(
            reconcile_dig_reminder=reconcile_dig_reminder,
        )
    )
    cog = DigCommands(bot, SimpleNamespace())

    with caplog.at_level(logging.WARNING, logger="cama_bot.commands.dig"):
        await cog._schedule_dig_reminder(10001, 12345)

    reconcile_dig_reminder.assert_awaited_once_with(bot, 10001, 12345)
    assert any(
        record.levelno == logging.WARNING
        and record.getMessage()
        == "dig reminder scheduling failed for user 10001 in guild 12345"
        for record in caplog.records
    )


@pytest.mark.asyncio
async def test_boss_encounter_receives_reminder_callback(monkeypatch):
    captured = {}

    class FakeBossEncounterView:
        def __init__(self, *args, **kwargs):
            captured.update(kwargs)

    monkeypatch.setattr(dig_module, "BossEncounterView", FakeBossEncounterView)
    bot = SimpleNamespace()
    dig_service = SimpleNamespace(
        has_scout_lantern=lambda user_id, guild_id: False,
    )
    cog = DigCommands(bot, dig_service)
    cog._send_public_dig = AsyncMock(return_value=None)
    interaction = SimpleNamespace(user=SimpleNamespace(id=10001))
    result = SimpleNamespace(
        boss_info=SimpleNamespace(
            name="Test Boss",
            dialogue="Test dialogue",
            boundary=None,
            luminosity_display=None,
        )
    )

    await cog._handle_boss_encounter(interaction, 12345, result)

    callback = captured["on_boss_resolved"]
    assert callback.__self__ is cog
    assert callback.__func__ is DigCommands._schedule_dig_reminder
