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
        bet_seed_radiant=0,
        bet_seed_dire=0,
        bet_seed_bonus=0,
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


def _scoped_user_ids(send_mock):
    """Ids the send was allowed to mention, asserting it's a scoped user list and
    NOT a broad parse (everyone/roles off) — the content carries LLM output."""
    am = send_mock.call_args.kwargs["allowed_mentions"]
    assert am.everyone is False and am.roles is False
    return {obj.id for obj in am.users}


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
async def test_warning_reminder_includes_pool_seed_text():
    cog, channel = _make_cog()
    pending = cog.match_service.get_last_shuffle.return_value
    pending.bet_seed_radiant = 25
    pending.bet_seed_dire = 25

    await send_betting_reminder(
        cog, 1, reminder_type="warning", lock_until=LOCK_TS, pending_match_id=1
    )

    content = channel.send.call_args.args[0]
    assert "Seed: Radiant 25" in content
    assert "Dire 25" in content


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
async def test_closed_reminder_includes_house_bonus_text(monkeypatch):
    cog, channel = _make_cog()
    pending = cog.match_service.get_last_shuffle.return_value
    pending.betting_mode = "house"
    pending.bet_seed_bonus = 50
    cog.betting_service.get_pot_odds = MagicMock(return_value={"radiant": 10, "dire": 20})
    flip = AsyncMock()
    monkeypatch.setattr(bm, "update_shuffle_message_wagers", flip)

    await send_betting_reminder(
        cog, 1, reminder_type="closed", lock_until=LOCK_TS, pending_match_id=1
    )

    content = channel.send.call_args.args[0]
    assert "Final House (1:1) pool" in content
    assert "Winner bonus: 50" in content


@pytest.mark.asyncio
async def test_closed_reminder_flips_embed_to_locked(monkeypatch):
    cog, _ = _make_cog()
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
    assert _scoped_user_ids(channel.send) == {1001, 1002, 1003, 1004, 1005}


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
    assert _scoped_user_ids(channel.send) == {1001, 1002, 1003, 1004, 1005}


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
    assert _scoped_user_ids(channel.send) == {1001, 1002, 1003, 1004, 1005}


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
    # The allowed-mentions list itself must exclude the non-real ids, not just the text.
    assert _scoped_user_ids(channel.send) == {3001, 3002}


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


@pytest.mark.asyncio
async def test_final_warning_ping_cannot_mass_mention():
    """The ping carries an LLM-generated flavor line. Even if the model emits
    '@everyone', allowed_mentions must scope to the underdog ids only — a bare
    users=True would leave everyone/roles parseable and broadcast it."""
    cog, channel = _make_cog()
    cog.betting_service.get_pot_odds = MagicMock(return_value={"radiant": 100, "dire": 500})
    cog.flavor_text_service.generate_betting_warning = AsyncMock(
        return_value="@everyone @here the longshots need backers!"
    )

    await send_betting_reminder(
        cog, 1, reminder_type="warning", lock_until=LOCK_TS,
        pending_match_id=1, is_final_warning=True,
    )

    am = channel.send.call_args.kwargs["allowed_mentions"]
    assert am.everyone is False
    assert am.roles is False
    assert {obj.id for obj in am.users} == {1001, 1002, 1003, 1004, 1005}
    # The serialized payload parses nothing broad regardless of the text.
    assert am.to_dict().get("parse") == []


@pytest.mark.asyncio
async def test_final_warning_thread_reply_also_pings_scoped():
    """Reminders reply to the shuffle embed in its thread — the thread .reply
    must carry the same scoped ping, not fall back to a broad/none mention."""
    cog, channel = _make_cog()
    cog.betting_service.get_pot_odds = MagicMock(return_value={"radiant": 100, "dire": 500})
    cog.match_service.get_shuffle_message_info.return_value = {
        "channel_id": 111,
        "thread_message_id": 222,
        "thread_id": 333,
        "origin_channel_id": 111,
    }
    thread_message = MagicMock()
    thread_message.reply = AsyncMock()
    thread = MagicMock()
    thread.fetch_message = AsyncMock(
        side_effect=AssertionError("thread replies must not fetch the parent message")
    )
    thread.get_partial_message = MagicMock(return_value=thread_message)
    cog.bot.get_channel = MagicMock(side_effect=lambda cid: thread if cid == 333 else channel)

    await send_betting_reminder(
        cog, 1, reminder_type="warning", lock_until=LOCK_TS,
        pending_match_id=1, is_final_warning=True,
    )

    thread_message.reply.assert_awaited_once()
    thread.get_partial_message.assert_called_once_with(222)
    thread.fetch_message.assert_not_awaited()
    assert _scoped_user_ids(thread_message.reply) == {1001, 1002, 1003, 1004, 1005}
    assert "<@1001>" in thread_message.reply.call_args.args[0]


@pytest.mark.asyncio
async def test_final_warning_pings_even_without_flavor_service():
    """Ping and flavor are independent: a missing flavor service drops the flavor
    line but must NOT suppress the underdog ping."""
    cog, channel = _make_cog()
    cog.betting_service.get_pot_odds = MagicMock(return_value={"radiant": 100, "dire": 500})
    cog.flavor_text_service = None

    await send_betting_reminder(
        cog, 1, reminder_type="warning", lock_until=LOCK_TS,
        pending_match_id=1, is_final_warning=True,
    )

    content = channel.send.call_args.args[0]
    assert "WARNING LINE" not in content  # no flavor line
    assert "<@1001>" in content  # ping still fires
    assert _scoped_user_ids(channel.send) == {1001, 1002, 1003, 1004, 1005}
