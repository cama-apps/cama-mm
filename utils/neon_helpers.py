"""
Shared helpers for NeonDegenService interaction in commands.

Consolidates the duplicated neon result sending and service lookup
patterns from match, betting, and draft command cogs.
"""

import asyncio
import logging

import discord

logger = logging.getLogger("cama_bot.utils.neon_helpers")


def get_neon_service(bot):
    """Get the NeonDegenService from the bot, or None if unavailable."""
    from services.neon_degen_service import NeonDegenService

    svc = getattr(bot, "neon_degen_service", None)
    return svc if isinstance(svc, NeonDegenService) else None


async def send_neon_result(
    interaction: discord.Interaction, neon_result, delete_after: float = 60
) -> None:
    """Send a NeonResult to the channel, auto-deleting after a delay.

    Args:
        interaction: Discord interaction for channel access.
        neon_result: NeonResult object (or None, which is a no-op).
        delete_after: Seconds before auto-deleting the message.
    """
    try:
        if neon_result is None:
            return
        msg = None
        if neon_result.gif_file:
            gif_file = discord.File(
                neon_result.gif_file, filename="jopat_terminal.gif"
            )
            if neon_result.text_block:
                msg = await interaction.channel.send(
                    neon_result.text_block, file=gif_file
                )
            else:
                msg = await interaction.channel.send(file=gif_file)
        elif neon_result.text_block:
            msg = await interaction.channel.send(neon_result.text_block)
        elif neon_result.footer_text:
            msg = await interaction.channel.send(neon_result.footer_text)
        if msg:
            asyncio.create_task(_delete_after(msg, delete_after))
    except Exception as exc:
        logger.debug(f"Failed to send neon result: {exc}")


async def _delete_after(msg, delay: float) -> None:
    """Delete a message after a delay, ignoring errors."""
    try:
        await asyncio.sleep(delay)
        await msg.delete()
    except Exception:
        pass
