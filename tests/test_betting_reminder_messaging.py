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
        radiant_team_ids=[1001, 1002, 1003, 1004, 1005],
        dire_team_ids=[2001, 2002, 2003, 2004, 2005],
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
    cog.flavor_text_service.generate_betting_warning = AsyncMock(return_value="WARNING LINE")
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


# --- Final-warning tier (5-min by default): persona flavor + underdog ping ---


@pytest.mark.asyncio
async def test_non_final_warning_stays_terse():
    cog, channel = _make_cog()
    await send_betting_reminder(
        cog, 1, reminder_type="warning", lock_until=LOCK_TS,
        pending_match_id=1, is_final_warning=False,
    )

    cog.flavor_text_service.generate_betting_warning.assert_not_called()
    content = channel.send.call_args.args[0]
    assert "WARNING LINE" not in content
    assert "<@" not in content
    assert channel.send.call_args.kwargs["allowed_mentions"].users is False


@pytest.mark.asyncio
async def test_final_warning_balanced_pool_adds_flavor_no_ping():
    cog, channel = _make_cog()  # 100 vs 200 -> 2:1, below the 4:1 threshold
    await send_betting_reminder(
        cog, 1, reminder_type="warning", lock_until=LOCK_TS,
        pending_match_id=1, is_final_warning=True,
    )

    cog.flavor_text_service.generate_betting_warning.assert_awaited_once()
    assert cog.flavor_text_service.generate_betting_warning.call_args.kwargs["underdog_side"] is None
    content = channel.send.call_args.args[0]
    assert "WARNING LINE" in content
    assert "<@" not in content
    assert channel.send.call_args.kwargs["allowed_mentions"].users is False


@pytest.mark.asyncio
async def test_final_warning_lopsided_pings_underdog_team():
    cog, channel = _make_cog()
    cog.betting_service.get_pot_odds = MagicMock(return_value={"radiant": 100, "dire": 500})

    await send_betting_reminder(
        cog, 1, reminder_type="warning", lock_until=LOCK_TS,
        pending_match_id=1, is_final_warning=True,
    )

    # Radiant is the under-bet side at 5:1 -> its players get pinged, favorite does not.
    assert cog.flavor_text_service.generate_betting_warning.call_args.kwargs["underdog_side"] == "radiant"
    content = channel.send.call_args.args[0]
    for pid in (1001, 1002, 1003, 1004, 1005):
        assert f"<@{pid}>" in content
    assert "<@2001>" not in content
    assert channel.send.call_args.kwargs["allowed_mentions"].users is True


@pytest.mark.asyncio
async def test_final_warning_just_below_threshold_no_ping():
    cog, channel = _make_cog()
    cog.betting_service.get_pot_odds = MagicMock(return_value={"radiant": 100, "dire": 399})

    await send_betting_reminder(
        cog, 1, reminder_type="warning", lock_until=LOCK_TS,
        pending_match_id=1, is_final_warning=True,
    )

    assert cog.flavor_text_service.generate_betting_warning.call_args.kwargs["underdog_side"] is None
    content = channel.send.call_args.args[0]
    assert "<@" not in content
    assert channel.send.call_args.kwargs["allowed_mentions"].users is False


@pytest.mark.asyncio
async def test_final_warning_exactly_at_threshold_pings():
    cog, channel = _make_cog()
    cog.betting_service.get_pot_odds = MagicMock(return_value={"radiant": 100, "dire": 400})

    await send_betting_reminder(
        cog, 1, reminder_type="warning", lock_until=LOCK_TS,
        pending_match_id=1, is_final_warning=True,
    )

    assert cog.flavor_text_service.generate_betting_warning.call_args.kwargs["underdog_side"] == "radiant"
    assert channel.send.call_args.kwargs["allowed_mentions"].users is True


@pytest.mark.asyncio
async def test_final_warning_one_sided_pool_pings_empty_side():
    cog, channel = _make_cog()
    cog.betting_service.get_pot_odds = MagicMock(return_value={"radiant": 0, "dire": 300})

    await send_betting_reminder(
        cog, 1, reminder_type="warning", lock_until=LOCK_TS,
        pending_match_id=1, is_final_warning=True,
    )

    assert cog.flavor_text_service.generate_betting_warning.call_args.kwargs["underdog_side"] == "radiant"
    content = channel.send.call_args.args[0]
    assert "<@1001>" in content
    assert channel.send.call_args.kwargs["allowed_mentions"].users is True


@pytest.mark.asyncio
async def test_final_warning_empty_pool_flavors_without_ping():
    cog, channel = _make_cog()
    cog.betting_service.get_pot_odds = MagicMock(return_value={"radiant": 0, "dire": 0})

    await send_betting_reminder(
        cog, 1, reminder_type="warning", lock_until=LOCK_TS,
        pending_match_id=1, is_final_warning=True,
    )

    cog.flavor_text_service.generate_betting_warning.assert_awaited_once()
    assert cog.flavor_text_service.generate_betting_warning.call_args.kwargs["underdog_side"] is None
    content = channel.send.call_args.args[0]
    assert "WARNING LINE" in content
    assert "<@" not in content
    assert channel.send.call_args.kwargs["allowed_mentions"].users is False


@pytest.mark.asyncio
async def test_final_warning_filters_non_real_player_ids():
    cog, channel = _make_cog()
    cog.betting_service.get_pot_odds = MagicMock(return_value={"radiant": 100, "dire": 500})
    # Bots / placeholders carry non-positive ids and must not be pinged.
    cog.match_service.get_last_shuffle.return_value.radiant_team_ids = [3001, 0, -5, 3002]

    await send_betting_reminder(
        cog, 1, reminder_type="warning", lock_until=LOCK_TS,
        pending_match_id=1, is_final_warning=True,
    )

    content = channel.send.call_args.args[0]
    assert "<@3001>" in content
    assert "<@3002>" in content
    assert "<@0>" not in content
    assert "<@-5>" not in content


@pytest.mark.asyncio
async def test_last_call_unchanged_even_when_lopsided():
    """The 1-minute last call must stay byte-for-byte the old behavior: no
    underdog ping, no warning-flavor path, mentions suppressed."""
    cog, channel = _make_cog()
    cog.betting_service.get_pot_odds = MagicMock(return_value={"radiant": 100, "dire": 500})

    await send_betting_reminder(
        cog, 1, reminder_type="last_call", lock_until=LOCK_TS, pending_match_id=1,
    )

    cog.flavor_text_service.generate_betting_last_call.assert_awaited_once()
    cog.flavor_text_service.generate_betting_warning.assert_not_called()
    content = channel.send.call_args.args[0]
    assert "Last call" in content
    assert "ANNOUNCER LINE" in content
    assert "<@" not in content
    assert channel.send.call_args.kwargs["allowed_mentions"].users is False
