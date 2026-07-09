import logging
from types import SimpleNamespace
from unittest.mock import Mock

import pytest

from commands.dig import DigCommands


@pytest.mark.asyncio
async def test_ready_lookup_failure_does_not_interrupt_dig_and_warns(caplog):
    def fail_ready_lookup(user_id, guild_id):
        raise RuntimeError("ready lookup failed")

    schedule_dig_reminder = Mock()
    bot = SimpleNamespace(
        reminder_service=SimpleNamespace(
            schedule_dig_reminder=schedule_dig_reminder,
        )
    )
    cog = DigCommands(
        bot,
        SimpleNamespace(get_free_dig_ready_at=fail_ready_lookup),
    )

    with caplog.at_level(logging.WARNING, logger="cama_bot.commands.dig"):
        await cog._schedule_dig_reminder(10001, 12345)

    schedule_dig_reminder.assert_not_called()
    assert any(
        record.levelno == logging.WARNING
        and record.getMessage()
        == "dig reminder scheduling failed for user 10001 in guild 12345"
        for record in caplog.records
    )
