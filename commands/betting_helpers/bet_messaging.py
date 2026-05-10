"""Helpers that refresh shuffle wager fields and post betting reminders.

These touch ``cog.bot``, ``cog.match_service``, and ``cog.betting_service`` —
they exist as free coroutines so the cog file stays focused on command
handlers and the runtime contract for ``commands/match.py`` is preserved.
"""

from __future__ import annotations

import asyncio
import functools
import logging
from typing import TYPE_CHECKING

import discord

from utils.formatting import format_betting_display

if TYPE_CHECKING:
    from commands.betting import BettingCommands

logger = logging.getLogger("cama_bot.commands.betting")


async def update_shuffle_message_wagers(
    cog: BettingCommands,
    guild_id: int | None,
    pending_match_id: int | None = None,
) -> None:
    """Refresh the shuffle message's wager field with current totals.

    Updates both the main channel message and the thread copy.
    """
    pending_state = await asyncio.to_thread(
        cog.match_service.get_last_shuffle, guild_id, pending_match_id
    )
    if not pending_state:
        return

    # Get betting display info
    totals = await asyncio.to_thread(
        functools.partial(cog.betting_service.get_pot_odds, guild_id, pending_state=pending_state)
    )
    lock_until = pending_state.bet_lock_until
    betting_mode = pending_state.betting_mode
    field_name, field_value = format_betting_display(
        totals["radiant"], totals["dire"], betting_mode, lock_until
    )

    # Update main channel message (lobby channel)
    message_info = await asyncio.to_thread(
        cog.match_service.state_service.get_shuffle_message_info, guild_id, pending_match_id
    )
    message_id = message_info.get("message_id") if message_info else None
    channel_id = message_info.get("channel_id") if message_info else None
    if message_id and channel_id:
        await update_embed_betting_field(cog, channel_id, message_id, field_name, field_value)

    # Update command channel message if it exists (different from lobby channel)
    cmd_message_id = message_info.get("cmd_message_id") if message_info else None
    cmd_channel_id = message_info.get("cmd_channel_id") if message_info else None
    if cmd_message_id and cmd_channel_id:
        await update_embed_betting_field(cog, cmd_channel_id, cmd_message_id, field_name, field_value)

    # Update thread message if it exists
    thread_message_id = pending_state.thread_shuffle_message_id
    thread_id = pending_state.thread_shuffle_thread_id
    if thread_message_id and thread_id:
        await update_embed_betting_field(cog, thread_id, thread_message_id, field_name, field_value)


async def update_embed_betting_field(
    cog: BettingCommands,
    channel_id: int,
    message_id: int,
    field_name: str,
    field_value: str,
) -> None:
    """Helper to update the betting field in an embed message."""
    try:
        channel = cog.bot.get_channel(channel_id)
        if channel is None:
            channel = await cog.bot.fetch_channel(channel_id)
        if channel is None:
            return

        message = await channel.fetch_message(message_id)
        if not message or not message.embeds:
            return

        embed = message.embeds[0]
        embed_dict = embed.to_dict()
        fields = embed_dict.get("fields", [])

        # Known wager field names to look for
        wager_field_names = {"💰 Pool Betting", "💰 House Betting (1:1)", "💰 Betting"}

        # Find and update wager field, remove duplicates
        updated = False
        new_fields = []
        for field in fields:
            fname = field.get("name", "")
            if fname in wager_field_names:
                if not updated:
                    # Update the first matching wager field
                    field["name"] = field_name
                    field["value"] = field_value
                    new_fields.append(field)
                    updated = True
                # Skip duplicates (don't add them to new_fields)
            else:
                new_fields.append(field)

        if not updated:
            new_fields.append({"name": field_name, "value": field_value, "inline": False})
        embed_dict["fields"] = new_fields

        new_embed = discord.Embed.from_dict(embed_dict)
        await message.edit(embed=new_embed, allowed_mentions=discord.AllowedMentions.none())
    except Exception as exc:
        logger.warning(f"Failed to update shuffle wagers: {exc}", exc_info=True)


async def send_betting_reminder(
    cog: BettingCommands,
    guild_id: int | None,
    *,
    reminder_type: str,
    lock_until: int | None,
    pending_match_id: int | None = None,
) -> None:
    """Send a reminder message replying to the shuffle embed with current bet totals.

    reminder_type: "warning" (5 minutes left) or "closed" (betting closed).
    pending_match_id: Specific match ID for concurrent match support.
    """
    pending_state = await asyncio.to_thread(
        cog.match_service.get_last_shuffle, guild_id, pending_match_id=pending_match_id
    )
    if not pending_state:
        return

    message_info = await asyncio.to_thread(
        cog.match_service.get_shuffle_message_info, guild_id, pending_match_id=pending_match_id
    )
    channel_id = message_info.get("channel_id") if message_info else None
    thread_message_id = message_info.get("thread_message_id") if message_info else None
    thread_id = message_info.get("thread_id") if message_info else None

    totals = await asyncio.to_thread(
        functools.partial(cog.betting_service.get_pot_odds, guild_id, pending_state=pending_state)
    )
    betting_mode = pending_state.betting_mode

    # Format bets with odds for pool mode
    _, totals_text = format_betting_display(
        totals["radiant"], totals["dire"], betting_mode, lock_until=None
    )
    mode_label = "Pool" if betting_mode == "pool" else "House (1:1)"

    if reminder_type == "warning":
        if not lock_until:
            return
        content = (
            f"⏰ **5 minutes remaining until betting closes!** (<t:{int(lock_until)}:R>)\n"
            f"Mode: {mode_label}\n\n"
            f"Current bets:\n{totals_text}"
        )
    elif reminder_type == "closed":
        content = (
            f"🔒 **Betting is now closed!**\n"
            f"Mode: {mode_label}\n\n"
            f"Final bets:\n{totals_text}"
        )
    else:
        return

    # Post to origin channel (stored in shuffle message info, since reset_lobby clears it)
    try:
        # Get origin_channel_id from shuffle message info (lobby_service's is cleared by reset_lobby)
        origin_channel_id = message_info.get("origin_channel_id") if message_info else None
        target_channel_id = origin_channel_id if origin_channel_id else channel_id

        if target_channel_id:
            target_channel = cog.bot.get_channel(target_channel_id)
            if target_channel is None:
                target_channel = await cog.bot.fetch_channel(target_channel_id)
            if target_channel:
                await target_channel.send(content, allowed_mentions=discord.AllowedMentions.none())
    except Exception as exc:
        logger.warning(f"Failed to send betting reminder to channel: {exc}", exc_info=True)

    # Post to thread
    if thread_message_id and thread_id:
        try:
            thread = cog.bot.get_channel(thread_id)
            if thread is None:
                thread = await cog.bot.fetch_channel(thread_id)
            if thread:
                thread_message = await thread.fetch_message(thread_message_id)
                if thread_message:
                    await thread_message.reply(content, allowed_mentions=discord.AllowedMentions.none())
        except Exception as exc:
            logger.warning(f"Failed to send betting reminder to thread: {exc}", exc_info=True)
