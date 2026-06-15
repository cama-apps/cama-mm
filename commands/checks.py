"""Shared pre-command checks for slash commands."""

from __future__ import annotations

import asyncio
import functools
import types

import discord


def require_guild(handler):
    """Guard a slash command handler against DM context.

    Wraps a method ``async def handler(self, interaction, *args, **kwargs)``.
    If ``interaction.guild`` is ``None`` (DM), responds with an ephemeral
    error and returns early without invoking the wrapped handler. Otherwise
    delegates to the handler unchanged — handlers can safely read
    ``interaction.guild.id`` without a None-check inside the body.

    Must run before ``interaction.response`` is used (i.e., before any
    deferral); the wrapper sends the error via ``response.send_message``.

    The returned wrapper is rebound to the handler's ``__globals__`` so
    discord.py's annotation resolution (which uses
    ``callback.__globals__``) sees the symbols imported by the handler's
    own module rather than this module's.
    """

    async def _impl(self, interaction: discord.Interaction, *args, **kwargs):
        if interaction.guild is None:
            await interaction.response.send_message(
                "This command can only be used in a server.",
                ephemeral=True,
            )
            return
        return await handler(self, interaction, *args, **kwargs)

    wrapper = types.FunctionType(
        _impl.__code__,
        handler.__globals__,
        name=handler.__name__,
        argdefs=_impl.__defaults__,
        closure=_impl.__closure__,
    )
    functools.update_wrapper(wrapper, handler)
    return wrapper


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


async def require_mafia_channel(interaction: discord.Interaction) -> bool:
    """Gate /mafia commands to the configured MAFIA_CHANNEL_ID.

    Threads under the mafia channel inherit (parent.id check). If the
    configured channel can't be resolved in this guild, fall back to
    require_gamba_channel so other guilds and misconfigured deploys keep
    working. Wrong channel charges 1 JC and sends an ephemeral pointer. Must
    be called before deferring.
    """
    from config import MAFIA_CHANNEL_ID

    guild = interaction.guild
    if guild is None or guild.get_channel(MAFIA_CHANNEL_ID) is None:
        return await require_gamba_channel(interaction)

    channel = interaction.channel
    if getattr(channel, "id", None) == MAFIA_CHANNEL_ID:
        return True
    parent = getattr(channel, "parent", None)
    if parent is not None and getattr(parent, "id", None) == MAFIA_CHANNEL_ID:
        return True

    user_id = interaction.user.id
    guild_id = guild.id
    player_service = interaction.client.player_service  # type: ignore[union-attr]
    await asyncio.to_thread(player_service.adjust_balance, user_id, guild_id, -1)

    await interaction.response.send_message(
        f"The case is being worked elsewhere — take it to <#{MAFIA_CHANNEL_ID}>. "
        "A single jopacoin slips into an informant's pocket as penance.",
        ephemeral=True,
    )
    return False
