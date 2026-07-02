"""
Tests for bot.py background-task plumbing: ``_supervised_loop`` and
``_log_task_exit`` ensure no prediction-market task can die silently;
``_next_digest_run`` schedules the twice-daily market digest.
"""

import asyncio
import datetime as dt
import logging
from unittest.mock import patch

import pytest


@pytest.fixture
def bot_module():
    import bot as bot_module

    with patch.object(bot_module.bot, "is_closed", return_value=False):
        yield bot_module


async def test_supervised_loop_returns_on_clean_exit(bot_module):
    """A body that returns cleanly ends the supervisor without a restart."""
    calls = 0

    async def body():
        nonlocal calls
        calls += 1

    await bot_module._supervised_loop("test", body)
    assert calls == 1


async def test_supervised_loop_restarts_after_crash(bot_module, caplog):
    """A crashing body is logged and restarted; clean return then ends the loop."""
    calls = 0

    async def body():
        nonlocal calls
        calls += 1
        if calls == 1:
            raise RuntimeError("boom")

    async def fake_sleep(_delay):
        return

    with patch.object(asyncio, "sleep", new=fake_sleep), caplog.at_level(
        logging.ERROR, logger="cama_bot"
    ):
        await bot_module._supervised_loop("test", body)

    assert calls == 2
    assert any("test crashed" in rec.message for rec in caplog.records)


async def test_supervised_loop_propagates_cancellation(bot_module):
    """task.cancel() on the supervisor surfaces as CancelledError."""
    started = asyncio.Event()

    async def body():
        started.set()
        await asyncio.sleep(60)  # would never finish

    task = asyncio.create_task(bot_module._supervised_loop("test", body))
    await started.wait()
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task


async def test_supervised_loop_backoff_caps_at_300s(bot_module):
    """Backoff doubles from 5s and saturates at 300s."""
    captured: list[float] = []
    calls = 0

    async def body():
        nonlocal calls
        calls += 1
        if calls <= 8:
            raise RuntimeError("boom")

    async def fake_sleep(delay):
        captured.append(delay)

    with patch.object(asyncio, "sleep", new=fake_sleep):
        await bot_module._supervised_loop("test", body)

    assert captured == [5, 10, 20, 40, 80, 160, 300, 300]


async def test_log_task_exit_logs_unexpected_exception(bot_module, caplog):
    """Done-callback emits a traceback when the task ended with an Exception."""
    cb = bot_module._log_task_exit("test")

    async def boom():
        raise RuntimeError("oops")

    task = asyncio.create_task(boom())
    with pytest.raises(RuntimeError):
        await task

    with caplog.at_level(logging.ERROR, logger="cama_bot"):
        cb(task)
    assert any("test exited unexpectedly" in rec.message for rec in caplog.records)


async def test_log_task_exit_silent_on_cancel(bot_module, caplog):
    """Cancellations during shutdown must not pollute the log."""
    cb = bot_module._log_task_exit("test")

    async def slow():
        await asyncio.sleep(60)

    task = asyncio.create_task(slow())
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task

    with caplog.at_level(logging.ERROR, logger="cama_bot"):
        cb(task)
    assert not any("exited unexpectedly" in rec.message for rec in caplog.records)


# --------------------------------------------------------------------------- #
# _next_digest_run — twice-daily digest scheduling (every 12h)
# --------------------------------------------------------------------------- #


def _utc(hour, minute=0):
    return dt.datetime(2026, 6, 22, hour, minute, tzinfo=dt.UTC)


def test_next_digest_run_picks_soonest_anchor_later_today(bot_module):
    """At 06:00 with anchors {0,12}, the next run is today's 12:00."""
    nxt = bot_module._next_digest_run(_utc(6), [0, 12])
    assert nxt == _utc(12)


def test_next_digest_run_rolls_to_tomorrow_after_last_anchor(bot_module):
    """At 13:00 both of today's anchors have passed, so roll to tomorrow 00:00."""
    nxt = bot_module._next_digest_run(_utc(13), [0, 12])
    assert nxt == _utc(0) + dt.timedelta(days=1)


def test_next_digest_run_anchors_are_twelve_hours_apart(bot_module):
    """From just after one anchor, the following anchor is exactly 12h away."""
    nxt = bot_module._next_digest_run(_utc(0, 1), [0, 12])
    assert nxt == _utc(12)
    assert (nxt - _utc(0)) == dt.timedelta(hours=12)


def test_next_digest_run_strictly_future_when_exactly_on_anchor(bot_module):
    """Exactly at an anchor, the run is the NEXT anchor (never fire twice)."""
    nxt = bot_module._next_digest_run(_utc(12), [0, 12])
    assert nxt == _utc(0) + dt.timedelta(days=1)


# --------------------------------------------------------------------------- #
# _post_daily_digest_all_guilds — charts the biggest mover into the digest
# --------------------------------------------------------------------------- #


def _open_market(pid, current, prev, volume=1):
    return {
        "prediction_id": pid,
        "question": f"q{pid}?",
        "current_price": current,
        "prev_price": prev,
        "guild_id": 7,
        "created_at": 100,
        "thread_id": None,
        "embed_message_id": None,
        "volume_recent": volume,
    }


def _digest_env(bot_module, markets, *, chart_filename="predict.png"):
    """Wire bot globals to fakes; return (cog, fake_file, patches) for the run."""
    from unittest.mock import AsyncMock, MagicMock, PropertyMock

    fake_file = MagicMock()
    fake_file.filename = chart_filename

    cog = MagicMock()
    cog.render_market_chart_file = AsyncMock(return_value=fake_file)
    cog.announce_to_gamba = AsyncMock()

    svc = MagicMock()
    svc.list_open_orderbook_markets = MagicMock(return_value=markets)
    svc.prediction_repo.pop_one_shot_flag = MagicMock(return_value=False)

    guild = MagicMock()
    guild.id = 7

    patches = [
        patch.object(bot_module.bot, "get_cog", return_value=cog),
        # bot.guilds is a read-only property, so patch it on the class.
        patch.object(
            type(bot_module.bot),
            "guilds",
            new_callable=PropertyMock,
            return_value=[guild],
        ),
        patch.object(bot_module.bot, "prediction_service", svc, create=True),
    ]
    return cog, fake_file, patches


async def test_post_digest_charts_biggest_mover_and_attaches_file(bot_module):
    """The market with the largest swing (#2, -8) is charted and its file sent.

    #1 is given the higher volume so it sorts first in the digest field list;
    the chart must still pick #2 by price swing, proving selection ignores the
    volume-based ordering.
    """
    markets = [_open_market(1, 52, 50, volume=9), _open_market(2, 30, 38, volume=1)]
    cog, fake_file, patches = _digest_env(bot_module, markets, chart_filename="predict_2.png")

    import contextlib

    with contextlib.ExitStack() as stack:
        for p in patches:
            stack.enter_context(p)
        await bot_module._post_daily_digest_all_guilds()

    # Chart rendered for #2 (the biggest mover), not #1 (top volume).
    cog.render_market_chart_file.assert_awaited_once()
    assert cog.render_market_chart_file.await_args.args[0]["prediction_id"] == 2

    # File forwarded to announce_to_gamba and wired into the embed image.
    cog.announce_to_gamba.assert_awaited_once()
    kwargs = cog.announce_to_gamba.await_args.kwargs
    assert kwargs["file"] is fake_file
    embed = kwargs["embed"]
    assert embed.image.url == "attachment://predict_2.png"
    assert "Biggest mover:** #2" in (embed.description or "")
    assert "↓8" in embed.description


async def test_post_digest_no_chart_when_nothing_moved(bot_module):
    """All markets flat → no chart rendered, file omitted from the announce call."""
    markets = [_open_market(1, 50, 50), _open_market(2, 17, 17)]
    cog, _fake_file, patches = _digest_env(bot_module, markets)

    import contextlib

    with contextlib.ExitStack() as stack:
        for p in patches:
            stack.enter_context(p)
        await bot_module._post_daily_digest_all_guilds()

    cog.render_market_chart_file.assert_not_awaited()
    cog.announce_to_gamba.assert_awaited_once()
    assert cog.announce_to_gamba.await_args.kwargs["file"] is None


# --------------------------------------------------------------------------- #
# _process_one_refresh — daily-summary post into an auto-archived thread
# --------------------------------------------------------------------------- #


class _ArchivedThread:
    """Thread stand-in: sends fail while archived (Discord error 50083)."""

    def __init__(self):
        self.archived = True
        self.sent: list[str] = []

    async def edit(self, archived: bool):
        self.archived = archived

    async def send(self, content):
        if self.archived:
            raise RuntimeError("50083: Thread is archived")
        self.sent.append(content)


async def test_process_one_refresh_unarchives_thread_for_daily_summary(bot_module):
    """The daily summary is the only message keeping market threads alive; if
    the thread auto-archived first, the send must revive it, not fail silently."""
    from unittest.mock import AsyncMock, MagicMock

    thread = _ArchivedThread()
    summary = {
        "skipped": False,
        "old_price": 50,
        "new_price": 55,
        "trade_summary": {
            "trade_count": 2,
            "total_volume": 7,
            "yes_volume": 4,
            "no_volume": 3,
            "biggest_trade": None,
        },
    }
    svc = MagicMock()
    svc.refresh_market = MagicMock(return_value=summary)
    cog = MagicMock()
    cog.refresh_market_embed = AsyncMock()

    with (
        patch.object(bot_module.bot, "prediction_service", svc, create=True),
        patch.object(bot_module.bot, "get_cog", return_value=cog),
        patch.object(bot_module.bot, "get_channel", return_value=thread),
    ):
        await bot_module._process_one_refresh({"prediction_id": 1, "thread_id": 999})

    cog.refresh_market_embed.assert_awaited_once_with(1)
    assert thread.archived is False
    assert len(thread.sent) == 1
    assert "Daily refresh" in thread.sent[0]
