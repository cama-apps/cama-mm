import asyncio
import time
from types import SimpleNamespace
from unittest.mock import MagicMock

import pytest

from commands.match import MatchCommands


class _StubMatchService:
    """Minimal stub to satisfy MatchCommands reminder helpers."""
    pass


def _make_commands(monkeypatch):
    bot = MagicMock()
    # Provide a BettingCommands stub so get_cog succeeds
    bot.get_cog.return_value = SimpleNamespace(_send_betting_reminder=True)
    # Disable the subscriber-DM path so it doesn't add stray create_task calls
    bot.reminder_service = None

    commands = MatchCommands(bot, MagicMock(), _StubMatchService(), MagicMock())
    commands._run_bet_reminder_after_delay = MagicMock()
    monkeypatch.setattr(asyncio, "create_task", lambda coro: coro)
    return commands


def _scheduled(commands):
    """Return {delay_seconds: reminder_type} for each scheduled reminder."""
    return {
        call.kwargs["delay_seconds"]: call.kwargs["reminder_type"]
        for call in commands._run_bet_reminder_after_delay.call_args_list
    }


@pytest.mark.asyncio
async def test_full_window_schedules_warnings_lastcall_and_close(monkeypatch):
    commands = _make_commands(monkeypatch)
    now = 2_000_000
    monkeypatch.setattr(time, "time", lambda: now)

    # Default offsets: warnings [600, 300], last call 60, close at lock.
    await commands._schedule_betting_reminders(guild_id=5, bet_lock_until=now + 900)

    # warnings at 900-600=300 and 900-300=600; last_call at 900-60=840; close at 900
    assert _scheduled(commands) == {
        300: "warning",
        600: "warning",
        840: "last_call",
        900: "closed",
    }


@pytest.mark.asyncio
async def test_short_window_skips_warnings_keeps_lastcall_and_close(monkeypatch):
    commands = _make_commands(monkeypatch)
    now = 1_000_000
    monkeypatch.setattr(time, "time", lambda: now)

    # 200s window: the 600/300 offsets are >= window (skipped); the 60 last call survives.
    await commands._schedule_betting_reminders(guild_id=1, bet_lock_until=now + 200)

    scheduled = _scheduled(commands)
    assert scheduled == {140: "last_call", 200: "closed"}
    assert "warning" not in scheduled.values()


@pytest.mark.asyncio
async def test_tiny_window_only_schedules_close(monkeypatch):
    commands = _make_commands(monkeypatch)
    now = 3_000_000
    monkeypatch.setattr(time, "time", lambda: now)

    # 30s window: every offset (600/300/60) is >= window, so only the close fires.
    await commands._schedule_betting_reminders(guild_id=1, bet_lock_until=now + 30)

    assert _scheduled(commands) == {30: "closed"}


@pytest.mark.asyncio
async def test_already_closed_window_schedules_nothing(monkeypatch):
    commands = _make_commands(monkeypatch)
    now = 4_000_000
    monkeypatch.setattr(time, "time", lambda: now)

    await commands._schedule_betting_reminders(guild_id=1, bet_lock_until=now - 5)

    assert commands._run_bet_reminder_after_delay.call_count == 0


@pytest.mark.asyncio
async def test_last_call_offset_overrides_warning_at_same_offset(monkeypatch):
    commands = _make_commands(monkeypatch)
    now = 5_000_000
    monkeypatch.setattr(time, "time", lambda: now)
    # Overlap the last-call offset with a configured warning offset.
    monkeypatch.setattr("commands.match.BET_REMINDER_OFFSETS", [60, 300])
    monkeypatch.setattr("commands.match.BET_LAST_CALL_OFFSET", 60)

    await commands._schedule_betting_reminders(guild_id=1, bet_lock_until=now + 900)

    # offset 60 -> last_call (not warning); offset 300 -> warning; close at lock.
    assert _scheduled(commands) == {840: "last_call", 600: "warning", 900: "closed"}
    # No two reminders share a delay (no duplicate at the overlapped offset).
    delays = [c.kwargs["delay_seconds"] for c in commands._run_bet_reminder_after_delay.call_args_list]
    assert len(delays) == len(set(delays))
