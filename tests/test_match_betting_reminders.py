import asyncio
import time
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from commands.match import MatchCommands


class _StubMatchService:
    """Minimal stub to satisfy MatchCommands reminder helpers."""

    def _normalize_guild_id(self, guild_id):
        return guild_id if guild_id is not None else 0


@pytest.mark.asyncio
async def test_schedule_betting_reminders_skips_warning_when_less_than_five_minutes(monkeypatch):
    bot = MagicMock()
    # Provide a BettingCommands stub so get_cog succeeds
    bot.get_cog.return_value = SimpleNamespace(_send_betting_reminder=True)

    match_service = _StubMatchService()
    commands = MatchCommands(bot, MagicMock(), match_service, MagicMock())

    # Avoid running real tasks; capture calls
    commands._run_bet_reminder_after_delay = AsyncMock()
    created_tasks = []
    monkeypatch.setattr(asyncio, "create_task", lambda coro: created_tasks.append(coro) or coro)

    now = 1_000_000
    monkeypatch.setattr(time, "time", lambda: now)

    bet_lock_until = now + 200  # < 5 minutes remaining
    await commands._schedule_betting_reminders(guild_id=1, bet_lock_until=bet_lock_until)

    # Only the close reminder should be scheduled
    assert commands._run_bet_reminder_after_delay.call_count == 1
    kwargs = commands._run_bet_reminder_after_delay.call_args.kwargs
    assert kwargs["reminder_type"] == "closed"
    assert kwargs["delay_seconds"] == 200


@pytest.mark.asyncio
async def test_schedule_betting_reminders_includes_warning_when_enough_time(monkeypatch):
    bot = MagicMock()
    bot.get_cog.return_value = SimpleNamespace(_send_betting_reminder=True)

    match_service = _StubMatchService()
    commands = MatchCommands(bot, MagicMock(), match_service, MagicMock())

    commands._run_bet_reminder_after_delay = AsyncMock()
    created_tasks = []
    monkeypatch.setattr(asyncio, "create_task", lambda coro: created_tasks.append(coro) or coro)

    now = 2_000_000
    monkeypatch.setattr(time, "time", lambda: now)

    bet_lock_until = now + 900  # 15 minutes remaining
    await commands._schedule_betting_reminders(guild_id=5, bet_lock_until=bet_lock_until)

    # Should schedule both warning and close reminders
    assert commands._run_bet_reminder_after_delay.call_count == 2
    calls = commands._run_bet_reminder_after_delay.call_args_list
    reminder_types = {call.kwargs["reminder_type"] for call in calls}
    delays = {call.kwargs["delay_seconds"] for call in calls}
    assert reminder_types == {"warning", "closed"}
    assert delays == {900, 600}  # close at 900s, warning at 600s before close
