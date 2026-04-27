"""
Tests for bot.py background-task plumbing: ``_supervised_loop`` and
``_log_task_exit`` ensure no prediction-market task can die silently.
"""

import asyncio
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
