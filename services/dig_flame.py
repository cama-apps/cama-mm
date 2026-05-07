"""Atmospheric channel-post helper for the dig minigame.

Provides a single fire-and-forget post primitive plus a small library of
atmospheric flavor-line pools that callers can draw from. Catastrophic
cave-ins, marquee events, and cross-player splash announcements all
post via this helper to keep the tone consistent — short, italicized,
no proper nouns, no mechanics exposition.
"""

from __future__ import annotations

import asyncio
import inspect
import logging
import random
from typing import Any

logger = logging.getLogger("cama_bot.services.dig_flame")

CATASTROPHIC_PREFIX = "💥"
SPLASH_PREFIX = "🩸"
GUILD_PREFIX = "🔔"

CATASTROPHIC_LINES: tuple[str, ...] = (
    "Far below, a tunnel folds shut. Stone settles for a long time.",
    "A great groan rolls through the rock. Somewhere in the dark, a miner is buried.",
    "Beams snap. Lanterns wink out. The earth swallows a tunnel whole.",
    "The deep gives a sound like a closing door. Then silence.",
    "Dust climbs out of a shaft that no longer goes anywhere.",
    "A pickaxe rings once, then stops. The ceiling has come for it.",
    "The hill shifts. Birds lift from leagues away.",
    "Below the bones of the world, a chamber that was now isn't.",
)


def pick_catastrophic_line() -> str:
    return random.choice(CATASTROPHIC_LINES)


def _spawn(coro: Any) -> None:
    if inspect.iscoroutine(coro):
        try:
            asyncio.create_task(coro)
        except RuntimeError:
            # No running loop; ignore (tests often hit this).
            logger.debug("dig_flame: no running loop, dropping post", exc_info=True)


def post_atmospheric(channel: Any, prefix: str, line: str) -> None:
    """Fire-and-forget atmospheric channel post.

    Tolerates ``channel is None``, MagicMock channels in tests, missing
    ``send`` coroutines, and any send-time error.
    """
    if channel is None or not line:
        return
    formatted = f"{prefix} *{line.strip()}*"
    try:
        send = getattr(channel, "send", None)
        if send is None:
            return
        _spawn(send(formatted))
    except Exception:
        logger.debug("post_atmospheric failed", exc_info=True)


def post_catastrophic(channel: Any) -> None:
    """Post a randomized catastrophic cave-in flame line."""
    post_atmospheric(channel, CATASTROPHIC_PREFIX, pick_catastrophic_line())
