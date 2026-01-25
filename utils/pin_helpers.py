"""Pin management utilities for Discord messages."""

import logging

import discord

logger = logging.getLogger("cama_bot.utils.pin_helpers")


async def safe_unpin_all_bot_messages(
    channel: discord.abc.Messageable | None,
    bot_user: discord.User,
) -> int:
    """
    Unpin all pinned messages authored by the bot in the channel.

    This ensures cleanup of any orphaned pinned lobby messages, not just
    the tracked one, in case previous unpins failed (crashes, restarts, etc.).

    Args:
        channel: The channel to unpin messages from (TextChannel or Thread)
        bot_user: The bot's user object to identify bot-authored messages

    Returns:
        The count of messages successfully unpinned
    """
    if not channel or not hasattr(channel, "pins"):
        return 0

    try:
        pinned_messages = await channel.pins()
    except discord.DiscordException as exc:
        logger.warning(f"Failed to fetch pins: {exc}")
        return 0

    unpinned = 0
    for msg in pinned_messages:
        if msg.author.id == bot_user.id:
            try:
                await msg.unpin(reason="Cama lobby closed")
                unpinned += 1
            except discord.Forbidden:
                logger.warning("Cannot unpin: missing Manage Messages permission")
            except discord.DiscordException as exc:
                logger.warning(f"Failed to unpin message {msg.id}: {exc}")

    return unpinned
