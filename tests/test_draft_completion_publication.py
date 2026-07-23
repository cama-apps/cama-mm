"""Concurrency and ordering tests for Immortal Draft completion publication."""

import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock, patch

import discord
import pytest

from commands.draft import DraftCommands


async def _run_sync(func, *args, **kwargs):
    """Deterministic stand-in for asyncio.to_thread in orchestration tests."""
    return func(*args, **kwargs)


def _make_cog(*, bot=None, match_service=None):
    cog = DraftCommands.__new__(DraftCommands)
    cog.bot = bot or MagicMock()
    cog.match_service = match_service or MagicMock()
    return cog


@pytest.mark.asyncio
async def test_distinct_completion_channel_copies_start_together_after_edit():
    lobby_started = asyncio.Event()
    origin_started = asyncio.Event()
    release = asyncio.Event()
    draft_channel = SimpleNamespace(id=10)
    draft_message = SimpleNamespace(id=11, channel=draft_channel)
    lobby_message = SimpleNamespace(id=20, channel=SimpleNamespace(id=77))
    origin_message = SimpleNamespace(id=30, channel=SimpleNamespace(id=88))
    cog = _make_cog()

    async def edit_message(interaction, *, embed, view):
        assert view is None
        return draft_message

    async def post_completion(embed, channel_id):
        if channel_id == 77:
            lobby_started.set()
            message = lobby_message
        else:
            assert channel_id == 88
            origin_started.set()
            message = origin_message
        await release.wait()
        return message

    cog._edit_interaction_message = AsyncMock(side_effect=edit_message)
    cog._post_completed_draft = AsyncMock(side_effect=post_completion)
    embed = discord.Embed(title="Draft Complete")

    publication = asyncio.create_task(
        cog._publish_draft_completion(
            SimpleNamespace(message=draft_message),
            embed,
            77,
            88,
            draft_channel.id,
        )
    )
    await asyncio.wait_for(
        asyncio.gather(lobby_started.wait(), origin_started.wait()),
        timeout=1,
    )

    assert not publication.done()
    release.set()
    assert await publication == (draft_message, lobby_message, origin_message)


@pytest.mark.asyncio
async def test_required_edit_failure_skips_completion_channel_copies():
    cog = _make_cog()

    async def fail_edit(*args, **kwargs):
        raise RuntimeError("interaction edit failed")

    cog._edit_interaction_message = AsyncMock(side_effect=fail_edit)
    cog._post_completed_draft = AsyncMock()

    with pytest.raises(RuntimeError, match="interaction edit failed"):
        await cog._publish_draft_completion(
            SimpleNamespace(),
            discord.Embed(title="Draft Complete"),
            77,
            88,
            10,
        )

    cog._post_completed_draft.assert_not_awaited()


@pytest.mark.asyncio
async def test_thread_rename_overlaps_ordered_posts_and_lock_waits():
    order = []
    rename_started = asyncio.Event()
    embed_started = asyncio.Event()
    release_rename = asyncio.Event()
    thread_message = SimpleNamespace(id=321)

    async def edit_thread(**kwargs):
        if "name" in kwargs:
            order.append("rename_started")
            rename_started.set()
            await release_rename.wait()
            order.append("rename_done")
            return

        assert kwargs == {"locked": True}
        order.append("locked")

    async def send_to_thread(content=None, **kwargs):
        if "embed" in kwargs:
            order.append("embed")
            embed_started.set()
            return thread_message

        order.append("ping")
        assert content == "<@10> <@20>\nPlayers, please take your starting positions!"
        return SimpleNamespace(id=322)

    thread = SimpleNamespace(
        edit=AsyncMock(side_effect=edit_thread),
        send=AsyncMock(side_effect=send_to_thread),
    )
    bot = MagicMock()
    bot.get_channel.return_value = thread
    match_service = MagicMock()
    match_service.set_shuffle_message_info.side_effect = (
        lambda *args, **kwargs: order.append("metadata")
    )
    cog = _make_cog(bot=bot, match_service=match_service)
    state = SimpleNamespace(
        guild_id=7,
        radiant_player_ids=[10, -1],
        dire_player_ids=[20],
    )

    with patch("commands.draft.asyncio.to_thread", new=_run_sync):
        publication = asyncio.create_task(
            cog._post_to_match_thread(
                state,
                discord.Embed(title="Draft Complete"),
                thread_id=42,
                pending_match_id=99,
            )
        )
        await asyncio.wait_for(
            asyncio.gather(rename_started.wait(), embed_started.wait()),
            timeout=1,
        )
        await asyncio.sleep(0)

        assert "ping" in order
        assert "metadata" in order
        assert "locked" not in order
        assert not publication.done()

        release_rename.set()
        await publication

    assert order.index("embed") < order.index("ping") < order.index("locked")
    assert order.index("rename_done") < order.index("locked")
    match_service.set_shuffle_message_info.assert_called_once_with(
        7,
        message_id=None,
        channel_id=None,
        thread_message_id=321,
        pending_match_id=99,
    )
