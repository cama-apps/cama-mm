"""Pin management utilities for Discord messages."""

import logging

import discord

logger = logging.getLogger("cama_bot.utils.pin_helpers")


async def safe_unpin_message(
    channel: discord.abc.Messageable | None,
    message_id: int | None,
) -> bool:
    """Unpin one tracked lobby message without disturbing sibling lobbies."""
    if not channel or not message_id or not hasattr(channel, "get_partial_message"):
        return False

    try:
        message = channel.get_partial_message(message_id)
        await message.unpin(reason="Cama lobby closed")
        return True
    except discord.Forbidden:
        logger.warning("Cannot unpin message %s: missing Manage Messages permission", message_id)
    except discord.NotFound:
        logger.debug("Tracked lobby message %s no longer exists", message_id)
    except discord.DiscordException as exc:
        logger.warning("Failed to unpin message %s: %s", message_id, exc)
    return False


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
