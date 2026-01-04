"""
Utilities for safely interacting with Discord responses/followups.
"""

import logging

import discord

logger = logging.getLogger("cama_bot.utils.interaction_safety")

from utils.debug_logging import debug_log as _dbg


async def safe_defer(interaction: discord.Interaction, *, ephemeral: bool = False) -> bool:
    """
    Defer the interaction if it is still valid.

    Returns True when the defer succeeded (or the response already exists),
    False when the interaction is no longer valid.
    """
    try:
        await interaction.response.defer(ephemeral=ephemeral)
        return True
    except (discord.NotFound, discord.InteractionResponded, discord.HTTPException) as exc:
        logger.warning("Unable to defer interaction: %s", exc)
        return False


async def safe_followup(
    interaction: discord.Interaction,
    *,
    content: str | None = None,
    embed: discord.Embed | None = None,
    ephemeral: bool = False,
    allowed_mentions: discord.AllowedMentions | None = None,
) -> discord.Message | None:
    """
    Send a followup via the interaction if possible; otherwise post directly in the channel.

    Returns None if the interaction was already responded to (to prevent duplicate messages).
    """
    # region agent log
    _dbg(
        "H7",
        "utils/interaction_safety.py:safe_followup:enter",
        "safe_followup called",
        {
            "has_content": content is not None,
            "has_embed": embed is not None,
            "ephemeral": ephemeral,
            "interaction_id": getattr(interaction, "id", None),
        },
        run_id="run3",
    )
    # endregion agent log
    try:
        msg = await interaction.followup.send(
            content=content,
            embed=embed,
            ephemeral=ephemeral,
            allowed_mentions=allowed_mentions,
        )
        # region agent log
        _dbg(
            "H7",
            "utils/interaction_safety.py:safe_followup:sent",
            "followup sent",
            {
                "has_content": content is not None,
                "has_embed": embed is not None,
                "ephemeral": ephemeral,
                "interaction_id": getattr(interaction, "id", None),
            },
            run_id="run3",
        )
        # endregion agent log
        return msg
    except (discord.NotFound, discord.InteractionResponded, discord.HTTPException) as exc:
        # Check if interaction was already responded to - if so, don't send fallback
        # This prevents duplicate non-ephemeral messages when multiple handlers process the same interaction
        if interaction.response.is_done():
            # region agent log
            _dbg(
                "H7",
                "utils/interaction_safety.py:safe_followup:already_responded",
                "followup failed; already responded",
                {
                    "interaction_id": getattr(interaction, "id", None),
                    "ephemeral": ephemeral,
                    "exc_type": type(exc).__name__,
                    "exc": str(exc),
                },
                run_id="run3",
            )
            # endregion agent log
            logger.warning(
                f"Followup failed and interaction already responded to (likely duplicate handler). "
                f"Not sending fallback message to prevent duplicates. Error: {exc}"
            )
            return None

        # Only send fallback if interaction wasn't already responded to
        logger.warning("Followup failed, sending to channel instead: %s", exc)
        channel = interaction.channel
        if not channel:
            raise
        msg = await channel.send(content=content, embed=embed, allowed_mentions=allowed_mentions)
        # region agent log
        _dbg(
            "H7",
            "utils/interaction_safety.py:safe_followup:fallback_sent",
            "fallback sent to channel",
            {
                "has_content": content is not None,
                "has_embed": embed is not None,
                "ephemeral": ephemeral,
                "interaction_id": getattr(interaction, "id", None),
            },
            run_id="run3",
        )
        # endregion agent log
        return msg
