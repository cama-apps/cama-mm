"""Tests for the betting reminder messaging helpers: terse warnings, the
last-call flavor line, and the closed-state embed flip."""

from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

import commands.betting_helpers.bet_messaging as bm
from commands.betting_helpers.bet_messaging import send_betting_reminder

LOCK_TS = 1_700_000_000


def _make_cog():
    cog = MagicMock()
    channel = MagicMock()
    channel.send = AsyncMock()
    cog.bot.get_channel = MagicMock(return_value=channel)
    cog.bot.fetch_channel = AsyncMock(return_value=channel)

    pending_state = SimpleNamespace(
        betting_mode="pool",
        bet_lock_until=LOCK_TS,
        pending_match_id=1,
        thread_shuffle_message_id=None,
        thread_shuffle_thread_id=None,
    )
    cog.match_service.get_last_shuffle = MagicMock(return_value=pending_state)
    cog.match_service.get_shuffle_message_info = MagicMock(
        return_value={
            "channel_id": 111,
            "thread_message_id": None,
            "thread_id": None,
            "origin_channel_id": 111,
        }
    )
    cog.betting_service.get_pot_odds = MagicMock(return_value={"radiant": 100, "dire": 200})
    cog.betting_service.get_top_voluntary_bettor = MagicMock(return_value=None)
    cog.flavor_text_service.generate_betting_last_call = AsyncMock(return_value="ANNOUNCER LINE")
    return cog, channel


@pytest.mark.asyncio
async def test_warning_reminder_is_terse_and_skips_flavor():
    cog, channel = _make_cog()
    await send_betting_reminder(
        cog, 1, reminder_type="warning", lock_until=LOCK_TS, pending_match_id=1
    )

    cog.flavor_text_service.generate_betting_last_call.assert_not_called()
    channel.send.assert_awaited_once()
    content = channel.send.call_args.args[0]
    assert f"Betting closes <t:{LOCK_TS}:R>" in content
    assert "ANNOUNCER LINE" not in content


@pytest.mark.asyncio
async def test_last_call_reminder_includes_flavor_line():
    cog, channel = _make_cog()
    await send_betting_reminder(
        cog, 1, reminder_type="last_call", lock_until=LOCK_TS, pending_match_id=1
    )

    cog.flavor_text_service.generate_betting_last_call.assert_awaited_once()
    content = channel.send.call_args.args[0]
    assert "Last call" in content
    assert "ANNOUNCER LINE" in content


@pytest.mark.asyncio
async def test_closed_reminder_flips_embed_to_locked(monkeypatch):
    cog, channel = _make_cog()
    flip = AsyncMock()
    monkeypatch.setattr(bm, "update_shuffle_message_wagers", flip)

    await send_betting_reminder(
        cog, 1, reminder_type="closed", lock_until=LOCK_TS, pending_match_id=1
    )

    flip.assert_awaited_once()
    assert flip.call_args.kwargs.get("locked") is True
