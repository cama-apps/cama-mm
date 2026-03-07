"""Shared pre-command checks for slash commands."""

from __future__ import annotations

import asyncio

import discord


async def require_gamba_channel(interaction: discord.Interaction) -> bool:
    """Return True if the channel name contains 'gamba'.

    Otherwise charge 1 JC and send a cryptic ephemeral error.
    Must be called **before** deferring so we can use response.send_message.
    """
    channel = interaction.channel
    channel_name = getattr(channel, "name", "") or ""
    if "gamba" in channel_name.lower():
        return True

    # Charge 1 JC
    user_id = interaction.user.id
    guild_id = interaction.guild.id if interaction.guild else None
    player_service = interaction.client.player_service  # type: ignore[union-attr]
    await asyncio.to_thread(player_service.adjust_balance, user_id, guild_id, -1)

    await interaction.response.send_message(
        "The ancient spirits reject your offering... this ground is not consecrated. "
        "A single jopacoin dissolves into the ether as penance.",
        ephemeral=True,
    )
    return False
