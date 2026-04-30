"""Shared pre-command checks for slash commands."""

from __future__ import annotations

import asyncio

import discord


async def require_gamba_channel(
    interaction: discord.Interaction,
    *,
    extra_allowed_channel_ids: tuple[int, ...] = (),
) -> bool:
    """Return True if the channel passes the gamba gate.

    Pass-conditions:
    - channel name (or its parent, for threads) contains 'gamba'
    - channel id (or its parent's id) is in ``extra_allowed_channel_ids``

    Threads inherit their parent's pass-state — a button clicked inside a
    market thread under #gamba should pass even though the thread's own name
    doesn't contain 'gamba'. The ``extra_allowed_channel_ids`` hook lets a
    designated channel (e.g. a dedicated dig channel) authorize commands
    without needing 'gamba' in its name. Otherwise charge 1 JC and send a
    cryptic ephemeral error. Must be called **before** deferring so we can
    use response.send_message.
    """
    channel = interaction.channel
    channel_name = (getattr(channel, "name", "") or "").lower()
    if "gamba" in channel_name:
        return True
    parent = getattr(channel, "parent", None)
    if parent is not None:
        parent_name = (getattr(parent, "name", "") or "").lower()
        if "gamba" in parent_name:
            return True

    if extra_allowed_channel_ids:
        channel_id = getattr(channel, "id", None)
        if channel_id is not None and channel_id in extra_allowed_channel_ids:
            return True
        parent_id = getattr(parent, "id", None) if parent is not None else None
        if parent_id is not None and parent_id in extra_allowed_channel_ids:
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


async def require_dig_channel(interaction: discord.Interaction) -> bool:
    """Gate /dig commands to the configured DIG_CHANNEL_ID.

    Threads under the dig channel inherit (parent.id check). If
    DIG_CHANNEL_ID is unset, or the configured channel can't be resolved in
    this guild, fall back to require_gamba_channel so other guilds and
    misconfigured deploys keep working. Wrong channel charges 1 JC and sends
    an ephemeral pointer. Must be called before deferring.
    """
    from config import DIG_CHANNEL_ID

    if DIG_CHANNEL_ID is None:
        return await require_gamba_channel(interaction)

    guild = interaction.guild
    if guild is None or guild.get_channel(DIG_CHANNEL_ID) is None:
        return await require_gamba_channel(interaction)

    channel = interaction.channel
    if getattr(channel, "id", None) == DIG_CHANNEL_ID:
        return True
    parent = getattr(channel, "parent", None)
    if parent is not None and getattr(parent, "id", None) == DIG_CHANNEL_ID:
        return True

    user_id = interaction.user.id
    guild_id = guild.id
    player_service = interaction.client.player_service  # type: ignore[union-attr]
    await asyncio.to_thread(player_service.adjust_balance, user_id, guild_id, -1)

    await interaction.response.send_message(
        f"The earth here is silent. Your tools belong in <#{DIG_CHANNEL_ID}> — "
        "a single jopacoin dissolves into the ether as penance.",
        ephemeral=True,
    )
    return False
