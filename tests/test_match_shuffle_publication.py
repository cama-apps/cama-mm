"""Concurrency and ordering tests for successful shuffle publication."""

import asyncio
import logging
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock, patch

import discord
import pytest

from commands.match import MatchCommands


async def _run_sync(func, *args, **kwargs):
    """Deterministic stand-in for asyncio.to_thread in orchestration tests."""
    return func(*args, **kwargs)


def _make_cog(
    *,
    lobby_channel,
    command_channel,
    confirmation_send,
    pending_state=None,
):
    bot = MagicMock()
    bot.user = SimpleNamespace(id=999)
    bot.get_channel.return_value = lobby_channel
    bot.fetch_channel = AsyncMock()
    bot.get_cog.return_value = None

    lobby_service = MagicMock()
    lobby_service.get_lobby_channel_id.return_value = lobby_channel.id
    lobby_service.get_origin_channel_id.return_value = command_channel.id

    match_service = MagicMock()
    match_service.get_last_shuffle.return_value = pending_state

    interaction = SimpleNamespace(
        channel=command_channel,
        followup=SimpleNamespace(send=confirmation_send),
    )
    cog = MatchCommands(bot, lobby_service, match_service, MagicMock())
    return cog, bot, lobby_service, match_service, interaction


@pytest.mark.asyncio
async def test_publication_sends_overlap_and_confirmation_error_waits_for_posts():
    lobby_started = asyncio.Event()
    command_started = asyncio.Event()
    confirmation_started = asyncio.Event()
    release_public_posts = asyncio.Event()

    lobby_channel = SimpleNamespace(id=100)
    command_channel = SimpleNamespace(id=200)

    async def send_lobby(*, embed):
        lobby_started.set()
        await release_public_posts.wait()
        return SimpleNamespace(
            id=10,
            channel=lobby_channel,
            jump_url="https://discord.test/lobby/10",
        )

    async def send_command(*, embed):
        command_started.set()
        await release_public_posts.wait()
        return SimpleNamespace(id=20, channel=command_channel)

    async def send_confirmation(*args, **kwargs):
        confirmation_started.set()
        raise RuntimeError("confirmation failed")

    lobby_channel.send = send_lobby
    command_channel.send = send_command
    cog, _, lobby_service, _, interaction = _make_cog(
        lobby_channel=lobby_channel,
        command_channel=command_channel,
        confirmation_send=send_confirmation,
    )

    lock_thread = AsyncMock()
    unpin = AsyncMock()
    fake_bot_module = SimpleNamespace(clear_lobby_rally_cooldowns=MagicMock())
    with (
        patch("commands.match.asyncio.to_thread", new=_run_sync),
        patch.object(cog, "_schedule_betting_reminders", new=AsyncMock()),
        patch.object(cog, "_lock_lobby_thread", new=lock_thread),
        patch("commands.match.safe_unpin_all_bot_messages", new=unpin),
        patch.dict("sys.modules", {"bot": fake_bot_module}),
    ):
        finalize_task = asyncio.create_task(
            cog._finalize_shuffle(
                interaction,
                guild_id=1,
                embed=discord.Embed(title="Teams"),
                pending_match_id=7,
            )
        )
        await asyncio.wait_for(
            asyncio.gather(
                lobby_started.wait(),
                command_started.wait(),
                confirmation_started.wait(),
            ),
            timeout=1,
        )

        # The confirmation has already failed, but both public posts are still
        # awaited rather than left running in the background.
        await asyncio.sleep(0)
        assert not finalize_task.done()

        release_public_posts.set()
        with pytest.raises(RuntimeError, match="confirmation failed"):
            await finalize_task

    lobby_service.reset_lobby.assert_not_called()
    lock_thread.assert_not_awaited()
    unpin.assert_not_awaited()


@pytest.mark.asyncio
async def test_public_send_failures_are_isolated_and_logged(caplog):
    lobby_channel = SimpleNamespace(
        id=100,
        send=AsyncMock(side_effect=RuntimeError("lobby unavailable")),
    )
    command_channel = SimpleNamespace(
        id=200,
        send=AsyncMock(side_effect=RuntimeError("command unavailable")),
    )
    confirmation_send = AsyncMock()
    cog, _, lobby_service, match_service, interaction = _make_cog(
        lobby_channel=lobby_channel,
        command_channel=command_channel,
        confirmation_send=confirmation_send,
    )

    lock_thread = AsyncMock()
    unpin = AsyncMock()
    fake_bot_module = SimpleNamespace(clear_lobby_rally_cooldowns=MagicMock())
    caplog.set_level(logging.WARNING, logger="cama_bot.commands.match")
    with (
        patch("commands.match.asyncio.to_thread", new=_run_sync),
        patch.object(cog, "_schedule_betting_reminders", new=AsyncMock()),
        patch.object(cog, "_lock_lobby_thread", new=lock_thread),
        patch("commands.match.safe_unpin_all_bot_messages", new=unpin),
        patch.dict("sys.modules", {"bot": fake_bot_module}),
    ):
        await cog._finalize_shuffle(
            interaction,
            guild_id=1,
            embed=discord.Embed(title="Teams"),
            pending_match_id=7,
        )

    confirmation_send.assert_awaited_once_with("✅ Teams shuffled!", ephemeral=True)
    assert "Failed to post shuffle to lobby channel: lobby unavailable" in caplog.text
    assert "Failed to post shuffle to command channel: command unavailable" in caplog.text
    match_service.set_shuffle_message_info.assert_not_called()
    lock_thread.assert_awaited_once()
    unpin.assert_awaited_once_with(lobby_channel, cog.bot.user)
    lobby_service.reset_lobby.assert_called_once_with(1)


@pytest.mark.asyncio
async def test_thread_and_pin_cleanup_overlap_before_reset():
    order = []
    thread_started = asyncio.Event()
    pins_started = asyncio.Event()
    release_maintenance = asyncio.Event()

    lobby_channel = SimpleNamespace(id=100)
    command_channel = SimpleNamespace(id=200)
    lobby_channel.send = AsyncMock(
        return_value=SimpleNamespace(
            id=10,
            channel=lobby_channel,
            jump_url="https://discord.test/lobby/10",
        )
    )
    command_channel.send = AsyncMock(
        return_value=SimpleNamespace(id=20, channel=command_channel)
    )
    pending_state = SimpleNamespace(
        bet_lock_until=None,
        radiant_team_ids=[1, 2],
        dire_team_ids=[3, 4],
    )
    cog, bot, lobby_service, match_service, interaction = _make_cog(
        lobby_channel=lobby_channel,
        command_channel=command_channel,
        confirmation_send=AsyncMock(),
        pending_state=pending_state,
    )
    match_service.set_shuffle_message_info.side_effect = (
        lambda *args, **kwargs: order.append("public_metadata")
    )
    lobby_service.reset_lobby.side_effect = lambda guild_id: order.append("reset")

    async def lock_thread(*args, **kwargs):
        order.append("thread_started")
        thread_started.set()
        await release_maintenance.wait()
        order.append("thread_metadata_done")

    async def clean_pins(channel, bot_user):
        assert channel is lobby_channel
        order.append("pins_started")
        pins_started.set()
        await release_maintenance.wait()
        order.append("pins_done")
        return 1

    fake_bot_module = SimpleNamespace(clear_lobby_rally_cooldowns=MagicMock())
    with (
        patch("commands.match.asyncio.to_thread", new=_run_sync),
        patch.object(cog, "_schedule_betting_reminders", new=AsyncMock()),
        patch.object(cog, "_lock_lobby_thread", new=AsyncMock(side_effect=lock_thread)),
        patch("commands.match.safe_unpin_all_bot_messages", new=AsyncMock(side_effect=clean_pins)),
        patch.dict("sys.modules", {"bot": fake_bot_module}),
    ):
        finalize_task = asyncio.create_task(
            cog._finalize_shuffle(
                interaction,
                guild_id=1,
                embed=discord.Embed(title="Teams"),
                pending_match_id=7,
            )
        )
        await asyncio.wait_for(
            asyncio.gather(thread_started.wait(), pins_started.wait()),
            timeout=1,
        )

        assert "public_metadata" in order
        assert "reset" not in order
        assert not finalize_task.done()

        release_maintenance.set()
        await finalize_task

    assert order.index("public_metadata") < order.index("reset")
    assert order.index("thread_metadata_done") < order.index("reset")
    assert order.index("pins_done") < order.index("reset")
    bot.fetch_channel.assert_not_awaited()
    bot.get_channel.assert_called_once_with(lobby_channel.id)
    lobby_service.reset_lobby.assert_called_once_with(1)


@pytest.mark.asyncio
async def test_thread_rename_overlaps_ordered_posts_and_lock_waits_for_both():
    order = []
    rename_started = asyncio.Event()
    embed_sent = asyncio.Event()
    release_rename = asyncio.Event()

    thread = MagicMock()

    async def edit_thread(**kwargs):
        if "name" in kwargs:
            order.append("rename_started")
            rename_started.set()
            await release_rename.wait()
            order.append("rename_done")
        else:
            assert kwargs == {"locked": True}
            order.append("locked")

    async def send_to_thread(*args, **kwargs):
        if "embed" in kwargs:
            order.append("embed")
            embed_sent.set()
            return SimpleNamespace(id=321)

        order.append("ping")
        assert args == ("<@10> <@20>\nPlayers, take your starting positions",)
        return SimpleNamespace(id=322)

    thread.edit = AsyncMock(side_effect=edit_thread)
    thread.send = AsyncMock(side_effect=send_to_thread)

    bot = MagicMock()
    bot.get_channel.return_value = thread
    lobby_service = MagicMock()
    lobby_service.get_lobby_thread_id.return_value = 42
    match_service = MagicMock()
    match_service.set_shuffle_message_info.side_effect = (
        lambda *args, **kwargs: order.append("metadata")
    )
    cog = MatchCommands(bot, lobby_service, match_service, MagicMock())

    with patch("commands.match.asyncio.to_thread", new=_run_sync):
        lock_task = asyncio.create_task(
            cog._lock_lobby_thread(
                guild_id=1,
                shuffle_embed=discord.Embed(title="Teams"),
                included_player_ids=[10, -1, 20],
                pending_match_id=7,
            )
        )
        await asyncio.wait_for(
            asyncio.gather(rename_started.wait(), embed_sent.wait()),
            timeout=1,
        )

        # The embed branch started without waiting for the blocked rename.
        assert "ping" in order
        assert "locked" not in order
        assert not lock_task.done()

        release_rename.set()
        await lock_task

    assert order.index("embed") < order.index("ping") < order.index("locked")
    assert order.index("rename_done") < order.index("locked")
    assert order.index("locked") < order.index("metadata")
    match_service.set_shuffle_message_info.assert_called_once_with(
        1,
        message_id=None,
        channel_id=None,
        thread_message_id=321,
        thread_id=42,
        pending_match_id=7,
    )
