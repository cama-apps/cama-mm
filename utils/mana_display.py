"""Helpers for rendering today's mana color in command embeds.

The badge is just an emoji — players see their color at a glance and can type
`/mana` for the full picture. Stays consistent across non-dig commands.
"""

from __future__ import annotations

import asyncio
from typing import TYPE_CHECKING

from services.mana_service import LAND_EMOJIS

if TYPE_CHECKING:
    from domain.models.mana_effects import ManaEffects


def format_mana_badge(effects: ManaEffects | None) -> str:
    """Return the land emoji for the player's current mana, or '' if unassigned."""
    if effects is None or effects.land is None:
        return ""
    return LAND_EMOJIS.get(effects.land, "")


async def resolve_mana_badge(bot, discord_id: int, guild_id: int | None) -> str:
    """Fetch the player's current mana effects via the bot's service container
    and return the emoji badge. Returns '' if the service or effects are
    unavailable. Intended for command embeds outside `/dig`."""
    mana_fx = getattr(bot, "mana_effects_service", None)
    if not mana_fx:
        return ""
    try:
        effects = await asyncio.to_thread(mana_fx.get_effects, discord_id, guild_id)
    except Exception:
        return ""
    return format_mana_badge(effects)
