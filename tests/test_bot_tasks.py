"""
Tests for bot.py background-task plumbing: ``_supervised_loop`` and
``_log_task_exit`` ensure no prediction-market task can die silently;
``_next_digest_run`` schedules the twice-daily market digest.
"""

import asyncio
import datetime as dt
import inspect
import logging
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock, call, patch

import pytest


@pytest.fixture
def bot_module():
    import bot as bot_module

    bot_module._reminder_recovery_task = None
    bot_module._duel_challenge_task = None
    bot_module._economy_event_task = None
    with patch.object(bot_module.bot, "is_closed", return_value=False):
        yield bot_module

    for attr in (
        "_reminder_recovery_task",
        "_duel_challenge_task",
        "_economy_event_task",
    ):
        task = getattr(bot_module, attr)
        if task is not None:
            if task.done() and not task.cancelled():
                task.exception()
            elif not task.done():
                task.cancel()
        setattr(bot_module, attr, None)


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


async def test_duel_loop_catches_up_immediately_and_isolates_delivery_failures(
    bot_module, caplog
):
    """One failed due delivery cannot prevent later durable claims."""
    service = MagicMock()
    service.get_due_challenge_ids.return_value = [(7, 42), (8, 42)]
    cog = SimpleNamespace(
        process_due_challenge=AsyncMock(side_effect=[RuntimeError("boom"), None])
    )

    with (
        patch.object(bot_module.time, "time", return_value=1_700_000_000),
        patch.object(bot_module.bot, "duel_service", service, create=True),
        patch.object(bot_module.bot, "get_cog", return_value=cog),
        patch.object(bot_module.bot, "wait_until_ready", AsyncMock()),
        patch.object(bot_module.bot, "is_closed", side_effect=[False, True]),
        patch.object(bot_module.asyncio, "sleep", AsyncMock()) as sleep,
        caplog.at_level(logging.ERROR, logger="cama_bot"),
    ):
        await bot_module._duel_challenge_loop()

    service.get_due_challenge_ids.assert_called_once_with(1_700_000_000)
    assert cog.process_due_challenge.await_args_list == [
        call(7, 42, 1_700_000_000),
        call(8, 42, 1_700_000_000),
    ]
    sleep.assert_awaited_once_with(bot_module.DUEL_WORKER_WAKE_SECONDS)
    assert any("duel due delivery failed challenge=7" in rec.message for rec in caplog.records)


async def test_on_ready_retains_one_supervised_duel_worker(bot_module):
    """Reconnect-ready events reuse the live duel worker and observe its exit."""
    running = MagicMock()
    running.done.return_value = False
    bot_module._prediction_refresh_task = running
    bot_module._prediction_digest_task = running
    bot_module._manashop_debt_task = running
    bot_module._economy_event_task = running
    bot_module._duel_challenge_task = None

    duel_task = MagicMock()
    duel_task.done.return_value = False
    warm_tasks: list[MagicMock] = []

    def create_task(awaitable):
        if inspect.iscoroutine(awaitable):
            awaitable.close()
        if awaitable is supervised_result:
            return duel_task
        task = MagicMock()
        warm_tasks.append(task)
        return task

    supervised_result = object()
    supervisor = MagicMock(return_value=supervised_result)
    exit_callback = object()
    fake_loop = SimpleNamespace(create_task=MagicMock(side_effect=create_task))

    with (
        patch.object(bot_module.bot.tree, "walk_commands", return_value=[]),
        patch.object(bot_module.bot.tree, "sync", AsyncMock()),
        patch.object(bot_module.bot, "loop", fake_loop, create=True),
        patch.object(type(bot_module.bot), "guilds", new_callable=lambda: property(lambda _self: [])),
        patch.object(bot_module.bot, "player_service", None, create=True),
        patch.object(bot_module.bot, "reminder_service", None, create=True),
        patch.object(
            bot_module,
            "_reconcile_persisted_lobby_messages",
            AsyncMock(),
        ) as reconcile_lobbies,
        patch.object(bot_module, "_supervised_loop", supervisor),
        patch.object(bot_module, "_log_task_exit", return_value=exit_callback) as log_exit,
    ):
        await bot_module.on_ready()
        await bot_module.on_ready()

    supervisor.assert_called_once_with("duel_challenges", bot_module._duel_challenge_loop)
    log_exit.assert_called_once_with("duel_challenges")
    duel_task.add_done_callback.assert_called_once_with(exit_callback)
    assert bot_module._duel_challenge_task is duel_task
    assert reconcile_lobbies.await_count == 2


async def test_reconcile_persisted_lobby_message_removes_legacy_frogling_reaction(
    bot_module,
):
    lobby = SimpleNamespace(status="open")
    lobby_manager = SimpleNamespace(lobbies={42: lobby})
    lobby_service = MagicMock(lobby_manager=lobby_manager)
    lobby_service.get_lobby_message_id.return_value = 100
    lobby_service.get_lobby_channel_id.return_value = 200

    frogling_emoji = SimpleNamespace(id=1463270458848842003, name="frogling")
    message = SimpleNamespace(
        reactions=[SimpleNamespace(emoji=frogling_emoji)],
        clear_reaction=AsyncMock(),
    )
    channel = SimpleNamespace(fetch_message=AsyncMock(return_value=message))

    with (
        patch.object(bot_module.bot, "lobby_service", lobby_service, create=True),
        patch.object(bot_module.bot, "get_channel", return_value=channel),
        patch.object(bot_module, "update_lobby_message", AsyncMock()) as update_message,
    ):
        await bot_module._reconcile_persisted_lobby_messages()

    update_message.assert_awaited_once_with(message, lobby, 42)
    message.clear_reaction.assert_awaited_once_with(frogling_emoji)


async def test_update_lobby_message_reports_failed_edit(bot_module):
    lobby = SimpleNamespace(get_player_count=MagicMock(return_value=1))
    lobby_service = MagicMock()
    lobby_service.build_lobby_embed.return_value = object()
    message = SimpleNamespace(edit=AsyncMock(side_effect=RuntimeError("temporary")))

    with (
        patch.object(bot_module, "_init_services"),
        patch.object(bot_module.bot, "lobby_service", lobby_service, create=True),
    ):
        updated = await bot_module.update_lobby_message(message, lobby, 42)

    assert updated is False


async def test_reconcile_persisted_lobby_message_retries_failed_edit(bot_module):
    lobby = SimpleNamespace(status="open")
    lobby_manager = SimpleNamespace(lobbies={42: lobby})
    lobby_service = MagicMock(lobby_manager=lobby_manager)
    lobby_service.get_lobby_message_id.return_value = 100
    lobby_service.get_lobby_channel_id.return_value = 200
    message = SimpleNamespace(reactions=[])
    channel = SimpleNamespace(fetch_message=AsyncMock(return_value=message))

    with (
        patch.object(bot_module.bot, "lobby_service", lobby_service, create=True),
        patch.object(bot_module.bot, "get_channel", return_value=channel),
        patch.object(
            bot_module,
            "update_lobby_message",
            AsyncMock(side_effect=[False, True]),
        ) as update_message,
        patch.object(bot_module.asyncio, "sleep", AsyncMock()) as sleep,
    ):
        await bot_module._reconcile_persisted_lobby_messages()

    assert update_message.await_count == 2
    sleep.assert_awaited_once()


async def test_reconcile_persisted_lobby_message_retries_failed_reaction_clear(
    bot_module,
):
    lobby = SimpleNamespace(status="open")
    lobby_manager = SimpleNamespace(lobbies={42: lobby})
    lobby_service = MagicMock(lobby_manager=lobby_manager)
    lobby_service.get_lobby_message_id.return_value = 100
    lobby_service.get_lobby_channel_id.return_value = 200
    frogling_emoji = SimpleNamespace(id=1463270458848842003, name="frogling")
    message = SimpleNamespace(
        reactions=[SimpleNamespace(emoji=frogling_emoji)],
        clear_reaction=AsyncMock(side_effect=[RuntimeError("temporary"), None]),
    )
    channel = SimpleNamespace(fetch_message=AsyncMock(return_value=message))

    with (
        patch.object(bot_module.bot, "lobby_service", lobby_service, create=True),
        patch.object(bot_module.bot, "get_channel", return_value=channel),
        patch.object(bot_module, "update_lobby_message", AsyncMock(return_value=True)),
        patch.object(bot_module.asyncio, "sleep", AsyncMock()) as sleep,
    ):
        await bot_module._reconcile_persisted_lobby_messages()

    assert message.clear_reaction.await_count == 2
    sleep.assert_awaited_once()


async def test_retired_frogling_reaction_is_removed_from_active_lobby_message(
    bot_module,
):
    payload = SimpleNamespace(
        user_id=123,
        guild_id=42,
        channel_id=200,
        message_id=100,
        emoji=SimpleNamespace(id=1463270458848842003, name="frogling"),
    )
    lobby_service = MagicMock()
    lobby_service.get_lobby_message_id.return_value = 100
    message = SimpleNamespace(id=100, remove_reaction=AsyncMock())
    channel = SimpleNamespace(
        fetch_message=AsyncMock(),
        get_partial_message=MagicMock(return_value=message),
    )
    bot_user = SimpleNamespace(id=999)

    with (
        patch.object(
            type(bot_module.bot),
            "user",
            new_callable=lambda: property(lambda _self: bot_user),
        ),
        patch.object(bot_module, "_init_services"),
        patch.object(bot_module.bot, "lobby_service", lobby_service, create=True),
        patch.object(bot_module.bot, "get_channel", return_value=channel),
    ):
        await bot_module.on_raw_reaction_add(payload)

    removed_emoji, removed_user = message.remove_reaction.await_args.args
    assert removed_emoji is payload.emoji
    assert removed_user.id == payload.user_id
    channel.fetch_message.assert_not_awaited()
    channel.get_partial_message.assert_called_once_with(payload.message_id)
    lobby_service.join_lobby.assert_not_called()
    lobby_service.join_lobby_conditional.assert_not_called()


async def test_failed_readycheck_shortcut_removes_reaction_without_message_fetch(
    bot_module,
):
    payload = SimpleNamespace(
        user_id=123,
        guild_id=42,
        channel_id=200,
        message_id=100,
        emoji=SimpleNamespace(id=None, name="🔔"),
    )
    lobby_service = MagicMock()
    lobby_service.get_lobby_message_id.return_value = payload.message_id
    message = SimpleNamespace(remove_reaction=AsyncMock())
    channel = SimpleNamespace(
        fetch_message=AsyncMock(),
        get_partial_message=MagicMock(return_value=message),
    )
    user = SimpleNamespace(id=payload.user_id)
    cog = SimpleNamespace(
        _execute_readycheck=AsyncMock(return_value=("error", {})),
    )
    bot_user = SimpleNamespace(id=999)

    with (
        patch.object(
            type(bot_module.bot),
            "user",
            new_callable=lambda: property(lambda _self: bot_user),
        ),
        patch.object(bot_module, "_init_services"),
        patch.object(bot_module.bot, "lobby_service", lobby_service, create=True),
        patch.object(bot_module.bot, "get_cog", return_value=cog),
        patch.object(bot_module.bot, "get_guild", return_value=SimpleNamespace()),
        patch.object(bot_module.bot, "get_channel", return_value=channel),
        patch.object(bot_module.bot, "fetch_user", AsyncMock(return_value=user)),
    ):
        await bot_module.on_raw_reaction_add(payload)

    channel.fetch_message.assert_not_awaited()
    channel.get_partial_message.assert_called_once_with(payload.message_id)
    message.remove_reaction.assert_awaited_once_with("🔔", user)


async def test_lobby_ready_notification_recommends_readycheck_before_shuffle(
    bot_module,
):
    channel = SimpleNamespace(
        id=200,
        guild=SimpleNamespace(id=42),
        send=AsyncMock(),
    )
    lobby_service = MagicMock()
    lobby_service.ready_threshold = 10
    lobby_service.get_lobby.return_value = SimpleNamespace(
        status="open",
        get_player_count=lambda: 10,
    )
    lobby_service.get_lobby_message_id.return_value = 123
    lobby_service.get_lobby_channel_id.return_value = 200
    lobby_service.get_origin_channel_id.return_value = None

    with (
        patch.object(bot_module.bot, "lobby_service", lobby_service, create=True),
        patch.object(bot_module, "_lobby_ready_cooldowns", {}),
    ):
        await bot_module.notify_lobby_ready(channel, guild_id=42)

    embed = channel.send.await_args.kwargs["embed"]
    next_step = next(field for field in embed.fields if field.name == "Next Step")
    assert next_step.value == (
        "Run `/readycheck` before `/shuffle` to confirm everyone is ready."
    )


async def test_lobby_ready_notification_skips_lobbies_above_threshold(bot_module):
    channel = SimpleNamespace(
        id=200,
        guild=SimpleNamespace(id=42),
        send=AsyncMock(),
    )
    lobby_service = MagicMock()
    lobby_service.ready_threshold = 10
    lobby_service.get_lobby.return_value = SimpleNamespace(
        status="open",
        get_player_count=lambda: 11,
    )
    lobby_service.get_lobby_message_id.return_value = 123
    lobby_service.get_lobby_channel_id.return_value = 200
    lobby_service.get_origin_channel_id.return_value = None

    with (
        patch.object(bot_module.bot, "lobby_service", lobby_service, create=True),
        patch.object(bot_module, "_lobby_ready_cooldowns", {}),
    ):
        await bot_module.notify_lobby_ready(channel, guild_id=42)

    channel.send.assert_not_awaited()


async def test_lobby_ready_notification_skips_replaced_lobby_generation(bot_module):
    channel = SimpleNamespace(
        id=200,
        guild=SimpleNamespace(id=42),
        send=AsyncMock(),
    )
    lobby_service = MagicMock()
    lobby_service.ready_threshold = 10
    lobby_service.get_lobby.return_value = SimpleNamespace(
        status="open",
        get_player_count=lambda: 10,
    )
    lobby_service.get_lobby_message_id.side_effect = [123, 999]
    lobby_service.get_lobby_channel_id.return_value = 200
    lobby_service.get_origin_channel_id.return_value = None

    with (
        patch.object(bot_module.bot, "lobby_service", lobby_service, create=True),
        patch.object(bot_module, "_lobby_ready_cooldowns", {}),
    ):
        await bot_module.notify_lobby_ready(channel, guild_id=42)

    channel.send.assert_not_awaited()


async def test_lobby_ready_notification_skips_if_lobby_grows_during_delivery(bot_module):
    channel = SimpleNamespace(
        id=200,
        guild=SimpleNamespace(id=42),
        send=AsyncMock(),
    )
    lobby_service = MagicMock()
    lobby_service.ready_threshold = 10
    lobby_service.get_lobby.side_effect = [
        SimpleNamespace(status="open", get_player_count=lambda: 10),
        SimpleNamespace(status="open", get_player_count=lambda: 11),
    ]
    lobby_service.get_lobby_message_id.return_value = 123
    lobby_service.get_lobby_channel_id.return_value = 200
    lobby_service.get_origin_channel_id.return_value = None

    with (
        patch.object(bot_module.bot, "lobby_service", lobby_service, create=True),
        patch.object(bot_module, "_lobby_ready_cooldowns", {}),
    ):
        await bot_module.notify_lobby_ready(channel, guild_id=42)

    channel.send.assert_not_awaited()


async def test_concurrent_lobby_ready_notifications_send_once(bot_module):
    first_send_started = asyncio.Event()
    release_first_send = asyncio.Event()

    async def send(*, embed):
        first_send_started.set()
        await release_first_send.wait()

    channel = SimpleNamespace(
        id=200,
        guild=SimpleNamespace(id=42),
        send=AsyncMock(side_effect=send),
    )
    lobby_service = MagicMock()
    lobby_service.ready_threshold = 10
    lobby_service.get_lobby.return_value = SimpleNamespace(
        status="open",
        get_player_count=lambda: 10,
    )
    lobby_service.get_lobby_message_id.return_value = 123
    lobby_service.get_lobby_channel_id.return_value = 200
    lobby_service.get_origin_channel_id.return_value = None

    with (
        patch.object(bot_module.bot, "lobby_service", lobby_service, create=True),
        patch.object(bot_module, "_lobby_ready_cooldowns", {}),
    ):
        first = asyncio.create_task(bot_module.notify_lobby_ready(channel, guild_id=42))
        await first_send_started.wait()
        second = asyncio.create_task(bot_module.notify_lobby_ready(channel, guild_id=42))
        await second
        release_first_send.set()
        await first

    assert channel.send.await_count == 1


async def test_tenth_readycheck_confirmation_recommends_shuffle_once_in_origin_channel(
    bot_module,
):
    from commands.lobby import LobbyCommands

    payload = SimpleNamespace(
        user_id=9,
        guild_id=42,
        channel_id=200,
        message_id=500,
        emoji=SimpleNamespace(id=None, name="✅"),
    )
    lobby_service = MagicMock()
    lobby_service.ready_threshold = 10
    lobby_service.get_readycheck_message_id.return_value = 500
    lobby_service.add_readycheck_reaction.return_value = True
    lobby_service.remove_readycheck_reaction.return_value = True
    lobby_service.get_readycheck_reaction_count_for_message.side_effect = [9, 10, 10, 10]
    lobby_service.get_origin_channel_id.return_value = 300

    readycheck_message = SimpleNamespace(
        edit=AsyncMock(
            side_effect=[
                None,
                RuntimeError("readycheck embed edit failed"),
                None,
                None,
            ]
        )
    )
    readycheck_channel = SimpleNamespace(
        id=200,
        fetch_message=AsyncMock(),
        get_partial_message=MagicMock(return_value=readycheck_message),
    )
    origin_channel = SimpleNamespace(id=300, send=AsyncMock())
    cog = LobbyCommands(bot_module.bot, lobby_service, MagicMock())
    cog.rebuild_readycheck_embed = MagicMock(return_value=object())
    bot_user = SimpleNamespace(id=999)

    def get_channel(channel_id):
        return {200: readycheck_channel, 300: origin_channel}.get(channel_id)

    with (
        patch.object(
            type(bot_module.bot),
            "user",
            new_callable=lambda: property(lambda _self: bot_user),
        ),
        patch.object(bot_module, "_init_services"),
        patch.object(bot_module.bot, "lobby_service", lobby_service, create=True),
        patch.object(bot_module.bot, "get_cog", return_value=cog),
        patch.object(bot_module.bot, "get_channel", side_effect=get_channel),
    ):
        await bot_module.on_raw_reaction_add(payload)
        origin_channel.send.assert_not_awaited()

        payload.user_id = 10
        await bot_module.on_raw_reaction_add(payload)
        origin_channel.send.assert_awaited_once()

        await bot_module.on_raw_reaction_remove(payload)
        await bot_module.on_raw_reaction_add(payload)

    origin_channel.send.assert_awaited_once_with(
        "✅ **10 players confirmed ready.** Anyone can use `/shuffle` now. "
        "Only players who confirmed ready will be included; everyone else will sit out."
    )
    readycheck_channel.fetch_message.assert_not_awaited()
    assert readycheck_channel.get_partial_message.call_count == 4
    readycheck_channel.get_partial_message.assert_called_with(payload.message_id)


async def test_readycheck_completion_falls_back_when_origin_channel_fetch_fails(
    bot_module,
):
    from commands.lobby import LobbyCommands

    readycheck_channel = SimpleNamespace(id=200, send=AsyncMock())
    lobby_service = MagicMock()
    lobby_service.get_origin_channel_id.return_value = 300
    cog = LobbyCommands(bot_module.bot, lobby_service, MagicMock())

    with (
        patch.object(bot_module.bot, "lobby_service", lobby_service, create=True),
        patch.object(bot_module.bot, "get_channel", return_value=None),
        patch.object(
            bot_module.bot,
            "fetch_channel",
            AsyncMock(side_effect=RuntimeError("origin channel unavailable")),
        ),
    ):
        sent = await cog.notify_readycheck_complete(
            42,
            500,
            fallback_channel=readycheck_channel,
        )

    assert sent is True
    readycheck_channel.send.assert_awaited_once_with(
        "✅ **10 players confirmed ready.** Anyone can use `/shuffle` now. "
        "Only players who confirmed ready will be included; everyone else will sit out."
    )


async def test_readycheck_completion_falls_back_when_origin_lookup_fails(
    bot_module,
):
    from commands.lobby import LobbyCommands

    readycheck_channel = SimpleNamespace(id=200, send=AsyncMock())
    lobby_service = MagicMock()
    lobby_service.get_origin_channel_id.side_effect = RuntimeError(
        "origin lookup unavailable"
    )
    cog = LobbyCommands(bot_module.bot, lobby_service, MagicMock())

    with patch.object(bot_module.bot, "lobby_service", lobby_service, create=True):
        sent = await cog.notify_readycheck_complete(
            42,
            500,
            fallback_channel=readycheck_channel,
        )

    assert sent is True
    readycheck_channel.send.assert_awaited_once_with(
        "✅ **10 players confirmed ready.** Anyone can use `/shuffle` now. "
        "Only players who confirmed ready will be included; everyone else will sit out."
    )


async def test_readycheck_completion_uses_origin_when_source_channel_fetch_fails(
    bot_module,
):
    from commands.lobby import LobbyCommands

    payload = SimpleNamespace(
        user_id=10,
        guild_id=42,
        channel_id=200,
        message_id=500,
        emoji=SimpleNamespace(id=None, name="✅"),
    )
    lobby_service = MagicMock()
    lobby_service.ready_threshold = 10
    lobby_service.get_readycheck_message_id.return_value = 500
    lobby_service.add_readycheck_reaction.return_value = True
    lobby_service.get_readycheck_reaction_count_for_message.return_value = 10
    lobby_service.get_origin_channel_id.return_value = 300
    origin_channel = SimpleNamespace(id=300, send=AsyncMock())
    cog = LobbyCommands(bot_module.bot, lobby_service, MagicMock())
    cog.rebuild_readycheck_embed = MagicMock(return_value=object())
    bot_user = SimpleNamespace(id=999)

    def get_channel(channel_id):
        return origin_channel if channel_id == 300 else None

    async def fetch_channel(channel_id):
        if channel_id == 200:
            raise RuntimeError("readycheck channel unavailable")
        return origin_channel

    with (
        patch.object(
            type(bot_module.bot),
            "user",
            new_callable=lambda: property(lambda _self: bot_user),
        ),
        patch.object(bot_module, "_init_services"),
        patch.object(bot_module.bot, "lobby_service", lobby_service, create=True),
        patch.object(bot_module.bot, "get_cog", return_value=cog),
        patch.object(bot_module.bot, "get_channel", side_effect=get_channel),
        patch.object(bot_module.bot, "fetch_channel", side_effect=fetch_channel),
    ):
        await bot_module.on_raw_reaction_add(payload)

    origin_channel.send.assert_awaited_once_with(
        "✅ **10 players confirmed ready.** Anyone can use `/shuffle` now. "
        "Only players who confirmed ready will be included; everyone else will sit out."
    )


async def test_replaced_readycheck_is_revalidated_before_completion_send(bot_module):
    from commands.lobby import LobbyCommands

    origin_channel = SimpleNamespace(id=300, send=AsyncMock())
    fallback_channel = SimpleNamespace(id=200, send=AsyncMock())
    lobby_service = MagicMock()
    lobby_service.ready_threshold = 10
    lobby_service.get_readycheck_reaction_count_for_message.side_effect = [10, None]
    lobby_service.get_origin_channel_id.return_value = 300
    cog = LobbyCommands(bot_module.bot, lobby_service, MagicMock())

    with (
        patch.object(bot_module.bot, "get_channel", return_value=None),
        patch.object(
            bot_module.bot,
            "fetch_channel",
            AsyncMock(return_value=origin_channel),
        ),
    ):
        sent = await cog.notify_readycheck_completion_if_ready(
            42,
            500,
            fallback_channel=fallback_channel,
        )

    assert sent is False
    origin_channel.send.assert_not_awaited()
    fallback_channel.send.assert_not_awaited()


async def test_reaction_from_replaced_readycheck_does_not_confirm_new_check(
    bot_module,
):
    from services.lobby_manager_service import LobbyManagerService
    from services.lobby_service import LobbyService
    from tests.fakes.lobby_repo import FakeLobbyRepo

    payload = SimpleNamespace(
        user_id=9,
        guild_id=42,
        channel_id=200,
        message_id=500,
        emoji=SimpleNamespace(id=None, name="✅"),
    )
    manager = LobbyManagerService(FakeLobbyRepo())
    manager.set_readycheck_state(500, 200, {9}, {}, guild_id=42)
    lobby_service = LobbyService(manager, MagicMock(), ready_threshold=10)
    original_add = lobby_service.add_readycheck_reaction

    def replace_check_then_add(discord_id, tag, **kwargs):
        manager.set_readycheck_state(600, 200, {9}, {}, guild_id=42)
        return original_add(discord_id, tag, **kwargs)

    readycheck_channel = SimpleNamespace(
        id=200,
        fetch_message=AsyncMock(),
    )
    cog = SimpleNamespace(rebuild_readycheck_embed=MagicMock(return_value=None))
    bot_user = SimpleNamespace(id=999)

    with (
        patch.object(
            type(bot_module.bot),
            "user",
            new_callable=lambda: property(lambda _self: bot_user),
        ),
        patch.object(bot_module, "_init_services"),
        patch.object(bot_module.bot, "lobby_service", lobby_service, create=True),
        patch.object(
            lobby_service,
            "add_readycheck_reaction",
            side_effect=replace_check_then_add,
        ),
        patch.object(bot_module.bot, "get_cog", return_value=cog),
        patch.object(bot_module.bot, "get_channel", return_value=readycheck_channel),
    ):
        await bot_module.on_raw_reaction_add(payload)

    assert lobby_service.get_readycheck_message_id(guild_id=42) == 600
    assert lobby_service.get_readycheck_reacted(guild_id=42) == {}


async def test_removal_from_replaced_readycheck_does_not_unconfirm_new_check(
    bot_module,
):
    from services.lobby_manager_service import LobbyManagerService
    from services.lobby_service import LobbyService
    from tests.fakes.lobby_repo import FakeLobbyRepo

    payload = SimpleNamespace(
        user_id=9,
        guild_id=42,
        channel_id=200,
        message_id=500,
        emoji=SimpleNamespace(id=None, name="✅"),
    )
    manager = LobbyManagerService(FakeLobbyRepo())
    manager.set_readycheck_state(500, 200, {9}, {}, guild_id=42)
    manager.add_readycheck_reaction(9, "<@9>", guild_id=42)
    lobby_service = LobbyService(manager, MagicMock(), ready_threshold=10)
    original_remove = lobby_service.remove_readycheck_reaction

    def replace_check_then_remove(discord_id, **kwargs):
        manager.set_readycheck_state(600, 200, {9}, {}, guild_id=42)
        manager.add_readycheck_reaction(9, "<@9>", guild_id=42)
        return original_remove(discord_id, **kwargs)

    cog = SimpleNamespace(rebuild_readycheck_embed=MagicMock(return_value=None))
    bot_user = SimpleNamespace(id=999)

    with (
        patch.object(
            type(bot_module.bot),
            "user",
            new_callable=lambda: property(lambda _self: bot_user),
        ),
        patch.object(bot_module, "_init_services"),
        patch.object(bot_module.bot, "lobby_service", lobby_service, create=True),
        patch.object(
            lobby_service,
            "remove_readycheck_reaction",
            side_effect=replace_check_then_remove,
        ),
        patch.object(bot_module.bot, "get_cog", return_value=cog),
    ):
        await bot_module.on_raw_reaction_remove(payload)

    assert lobby_service.get_readycheck_message_id(guild_id=42) == 600
    assert lobby_service.get_readycheck_reacted(guild_id=42) == {9: "<@9>"}


async def test_concurrent_readycheck_completion_retries_after_failed_delivery(
    bot_module,
):
    from commands.lobby import LobbyCommands

    first_send_started = asyncio.Event()
    release_first_send = asyncio.Event()
    send_attempts = 0

    async def send(_content):
        nonlocal send_attempts
        send_attempts += 1
        if send_attempts == 1:
            first_send_started.set()
            await release_first_send.wait()
            raise RuntimeError("transient send failure")

    readycheck_channel = SimpleNamespace(
        id=200,
        send=AsyncMock(side_effect=send),
    )
    lobby_service = MagicMock()
    lobby_service.get_origin_channel_id.return_value = None
    cog = LobbyCommands(bot_module.bot, lobby_service, MagicMock())

    with patch.object(bot_module.bot, "lobby_service", lobby_service, create=True):
        first = asyncio.create_task(
            cog.notify_readycheck_complete(
                42,
                500,
                fallback_channel=readycheck_channel,
            )
        )
        await first_send_started.wait()
        second = asyncio.create_task(
            cog.notify_readycheck_complete(
                42,
                500,
                fallback_channel=readycheck_channel,
            )
        )
        await asyncio.sleep(0)
        release_first_send.set()
        results = await asyncio.gather(first, second)

    assert results == [False, True]
    assert send_attempts == 2


async def test_economy_event_loop_enforces_moratorium_before_activation(bot_module):
    """Each wake unlocks recovery ballots before sizing the day's event."""
    guild = SimpleNamespace(id=42)
    order: list[str] = []
    disburse_service = MagicMock()
    economy_service = MagicMock()

    def enforce(guild_id):
        assert guild_id == guild.id
        order.append("moratorium")
        return {"cancelled": False}

    def ensure(guild_id):
        assert guild_id == guild.id
        assert order == ["moratorium"]
        order.append("event")
        return ({"name": "Ravage", "direction": "deflationary"}, True)

    disburse_service.enforce_voting_moratorium.side_effect = enforce
    economy_service.ensure_daily_event.side_effect = ensure
    economy_service.seconds_until_next_trigger.return_value = 900

    with (
        patch.object(bot_module.bot, "wait_until_ready", AsyncMock()),
        patch.object(bot_module.bot, "is_closed", side_effect=[False, True]),
        patch.object(
            type(bot_module.bot),
            "guilds",
            new_callable=lambda: property(lambda _self: [guild]),
        ),
        patch.object(
            bot_module.bot,
            "disburse_service",
            disburse_service,
            create=True,
        ),
        patch.object(
            bot_module.bot,
            "economy_event_service",
            economy_service,
            create=True,
        ),
        patch.object(bot_module, "ECONOMY_RECOVERY_MODE", True),
        patch.object(bot_module, "_announce_economy_event", AsyncMock()) as announce,
        patch.object(bot_module.asyncio, "sleep", AsyncMock()) as sleep,
    ):
        await bot_module._economy_event_loop()

    assert order == ["moratorium", "event"]
    announce.assert_awaited_once_with(
        guild, {"name": "Ravage", "direction": "deflationary"}
    )
    sleep.assert_awaited_once_with(900)


@pytest.mark.parametrize(
    ("configured_wake", "seconds_until_trigger", "expected_sleep"),
    [
        (3600, 75, 75),
        (60, 3600, 60),
    ],
)
async def test_economy_event_loop_wakes_at_interval_or_trigger_whichever_is_first(
    bot_module,
    configured_wake,
    seconds_until_trigger,
    expected_sleep,
):
    """Startup drift cannot leave the worker asleep past the 10 AM trigger."""
    guild = SimpleNamespace(id=42)
    economy_service = MagicMock()
    economy_service.ensure_daily_event.return_value = (None, False)
    economy_service.seconds_until_next_trigger.return_value = seconds_until_trigger

    with (
        patch.object(bot_module.bot, "wait_until_ready", AsyncMock()),
        patch.object(bot_module.bot, "is_closed", side_effect=[False, True]),
        patch.object(
            type(bot_module.bot),
            "guilds",
            new_callable=lambda: property(lambda _self: [guild]),
        ),
        patch.object(bot_module.bot, "economy_event_service", economy_service, create=True),
        patch.object(bot_module, "ECONOMY_RECOVERY_MODE", False),
        patch.object(bot_module, "ECONOMY_EVENT_WAKE_SECONDS", configured_wake),
        patch.object(bot_module.asyncio, "sleep", AsyncMock()) as sleep,
    ):
        await bot_module._economy_event_loop()

    economy_service.seconds_until_next_trigger.assert_called_once_with()
    sleep.assert_awaited_once_with(expected_sleep)


async def test_economy_event_announcement_uses_shared_public_embed(bot_module):
    guild = SimpleNamespace(id=42)
    cog = SimpleNamespace(announce_to_gamba=AsyncMock())
    event = {
        "name": "Ravage",
        "severity": 3,
        "direction": "deflationary",
        "announcement": "A tidal shock tears through the Jopacoin economy.",
        "effects": {"reward_multiplier": 0.76, "reserve_burn_jc": 300},
        "ends_at": 1_752_943_600,
    }
    icon_url = "https://cdn.example/ravage.png"

    def get_cog(name):
        assert name == "PredictionCommands"
        return cog

    with (
        patch.object(bot_module.bot, "get_cog", side_effect=get_cog),
        patch.object(
            bot_module.trivia_data,
            "get_ability_icon_url_by_name",
            return_value=icon_url,
            create=True,
        ) as icon_lookup,
    ):
        await bot_module._announce_economy_event(guild, event)

    embed = cog.announce_to_gamba.await_args.kwargs["embed"]
    icon_lookup.assert_called_once_with("Ravage")
    assert embed.to_dict() == bot_module.build_public_economy_event_embed(
        event,
        icon_url=icon_url,
    ).to_dict()
    assert embed.thumbnail.url == icon_url
    assert embed.title == "🌑 Ravage — Level III"
    assert "24% lower" in embed.fields[0].value
    assert "300 JC" in embed.fields[1].value
    assert embed.footer.text == "The treasury watches. The edict endures."


async def test_economy_event_announcement_logs_when_prediction_cog_missing(
    bot_module, caplog
):
    guild = SimpleNamespace(id=42)

    with (
        patch.object(bot_module.bot, "get_cog", return_value=None) as get_cog,
        caplog.at_level(logging.WARNING, logger="cama_bot"),
    ):
        await bot_module._announce_economy_event(
            guild, {"direction": "deflationary"}
        )

    get_cog.assert_called_once_with("PredictionCommands")
    assert any(
        "PredictionCommands cog not loaded" in record.message
        for record in caplog.records
    )


async def test_economy_event_announcement_survives_icon_lookup_failure(
    bot_module, caplog
):
    guild = SimpleNamespace(id=42)
    cog = SimpleNamespace(announce_to_gamba=AsyncMock())

    with (
        patch.object(bot_module.bot, "get_cog", return_value=cog),
        patch.object(
            bot_module.trivia_data,
            "get_ability_icon_url_by_name",
            side_effect=RuntimeError("dotabase unavailable"),
            create=True,
        ),
        caplog.at_level(logging.WARNING, logger="cama_bot"),
    ):
        await bot_module._announce_economy_event(
            guild, {"name": "Ravage", "direction": "deflationary"}
        )

    cog.announce_to_gamba.assert_awaited_once()
    embed = cog.announce_to_gamba.await_args.kwargs["embed"]
    assert not embed.thumbnail
    assert any(
        "economy event icon lookup failed for event=Ravage guild=42"
        in record.message
        for record in caplog.records
    )

# --------------------------------------------------------------------------- #
# reconnect recovery sweeps — retained single-flight lifecycle
# --------------------------------------------------------------------------- #


async def test_reminder_recovery_is_single_flight_and_runs_after_success(bot_module):
    """Reconnects share a running sweep; a later reconnect gets a fresh sweep."""
    loop = asyncio.get_running_loop()
    starts = [asyncio.Event(), asyncio.Event()]
    releases = [loop.create_future(), loop.create_future()]
    calls: list[tuple[object, list[int]]] = []

    class ReminderService:
        async def reschedule_all(self, passed_bot, guild_ids):
            call_index = len(calls)
            calls.append((passed_bot, list(guild_ids)))
            starts[call_index].set()
            await releases[call_index]

    service = ReminderService()
    guild_ids = [11, 22]
    first = bot_module._start_reminder_recovery(service, guild_ids)
    guild_ids.append(33)
    await starts[0].wait()

    overlapping = bot_module._start_reminder_recovery(service, [99])
    assert overlapping is first
    assert calls == [(bot_module.bot, [11, 22])]

    releases[0].set_result(None)
    await first
    await asyncio.sleep(0)
    assert bot_module._reminder_recovery_task is None

    second = bot_module._start_reminder_recovery(service, [44])
    assert second is not first
    await starts[1].wait()
    assert calls == [
        (bot_module.bot, [11, 22]),
        (bot_module.bot, [44]),
    ]

    releases[1].set_result(None)
    await second
    await asyncio.sleep(0)
    assert bot_module._reminder_recovery_task is None


async def test_reminder_recovery_failure_is_logged_and_retryable(bot_module, caplog):
    """A failed sweep clears only its own handle so the next ready can retry."""
    loop = asyncio.get_running_loop()
    starts = [asyncio.Event(), asyncio.Event()]
    outcomes = [loop.create_future(), loop.create_future()]
    calls = 0

    class ReminderService:
        async def reschedule_all(self, _bot, _guild_ids):
            nonlocal calls
            call_index = calls
            calls += 1
            starts[call_index].set()
            await outcomes[call_index]

    service = ReminderService()
    with caplog.at_level(logging.WARNING, logger="cama_bot"):
        failed = bot_module._start_reminder_recovery(service, [7])
        await starts[0].wait()
        outcomes[0].set_exception(RuntimeError("reminder unavailable"))
        with pytest.raises(RuntimeError, match="reminder unavailable"):
            await failed
        await asyncio.sleep(0)

    assert bot_module._reminder_recovery_task is None
    assert any("Reminder recovery sweep failed" in record.message for record in caplog.records)

    retried = bot_module._start_reminder_recovery(service, [7])
    await starts[1].wait()
    outcomes[1].set_result(None)
    await retried
    await asyncio.sleep(0)
    assert calls == 2
    assert bot_module._reminder_recovery_task is None


def test_stale_recovery_callbacks_do_not_clear_newer_handles(bot_module):
    """A delayed callback cannot erase or complete a replacement sweep."""
    loop = asyncio.new_event_loop()
    try:
        old_reminder = loop.create_future()
        old_reminder.set_result(None)
        newer_reminder = loop.create_future()
        bot_module._reminder_recovery_task = newer_reminder

        bot_module._reminder_recovery_done(old_reminder)

        assert bot_module._reminder_recovery_task is newer_reminder

        newer_reminder.cancel()
    finally:
        loop.close()


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
    """Archived-thread stand-in.

    Discord semantics: sends auto-unarchive an *unlocked* thread, but message
    edits fail (50083). The guard under test revives the thread explicitly —
    its observable value is the revival plus the re-widened archive window.
    """

    def __init__(self):
        self.archived = True
        self.auto_archive_duration = 1440
        self.sent: list[str] = []
        self.archived_at_send: list[bool] = []

    async def edit(self, *, archived: bool, auto_archive_duration: int | None = None):
        # Keyword-only, like the real Thread.edit.
        self.archived = archived
        if auto_archive_duration is not None:
            self.auto_archive_duration = auto_archive_duration

    async def send(self, content):
        self.archived_at_send.append(self.archived)
        self.sent.append(content)


def _refresh_summary():
    return {
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


def _refresh_env(bot_module, thread, *, status="open"):
    """Wire bot globals to fakes for a _process_one_refresh run.

    get_channel returns None to mirror production, where discord.py evicts
    archived threads from the cache and only fetch_channel can resolve them.
    """
    from unittest.mock import AsyncMock, MagicMock

    svc = MagicMock()
    svc.refresh_market = MagicMock(return_value=_refresh_summary())
    svc.prediction_repo.get_prediction = MagicMock(return_value={"status": status})
    cog = MagicMock()
    cog.refresh_market_embed = AsyncMock()

    return cog, [
        patch.object(bot_module.bot, "prediction_service", svc, create=True),
        patch.object(bot_module.bot, "get_cog", return_value=cog),
        patch.object(bot_module.bot, "get_channel", return_value=None),
        patch.object(bot_module.bot, "fetch_channel", AsyncMock(return_value=thread)),
    ]


async def test_process_one_refresh_revives_thread_for_daily_summary(bot_module):
    """The daily summary is the only message keeping market threads alive; the
    guard must revive an archived thread (covers locked threads) and re-widen
    its auto-archive window so pre-fix threads stop re-archiving daily."""
    import contextlib

    thread = _ArchivedThread()
    cog, patches = _refresh_env(bot_module, thread)

    with contextlib.ExitStack() as stack:
        for p in patches:
            stack.enter_context(p)
        await bot_module._process_one_refresh({"prediction_id": 1, "thread_id": 999})

    cog.refresh_market_embed.assert_awaited_once_with(1)
    assert thread.archived is False
    assert thread.auto_archive_duration == 10080
    assert thread.sent and "Daily refresh" in thread.sent[0]
    assert thread.archived_at_send == [False]


async def test_process_one_refresh_skips_summary_when_market_no_longer_open(bot_module):
    """A market resolved between refresh_market and the summary post must not
    get its just-archived thread revived or a 'Daily refresh' message."""
    import contextlib

    thread = _ArchivedThread()
    cog, patches = _refresh_env(bot_module, thread, status="resolved")

    with contextlib.ExitStack() as stack:
        for p in patches:
            stack.enter_context(p)
        await bot_module._process_one_refresh({"prediction_id": 1, "thread_id": 999})

    cog.refresh_market_embed.assert_awaited_once_with(1)
    assert thread.archived is True
    assert thread.sent == []
