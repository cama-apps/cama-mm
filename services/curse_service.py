"""Witch's Curse service.

Coordinates curse purchases and the per-engagement flame rolls. Hook sites
spawn ``maybe_flame_and_post`` as a background task; it returns silently when
the outcome is not a loss, the target is not cursed, the target is on cooldown,
or the roll fails — so callers never need to await it.
"""

from __future__ import annotations

import asyncio
import inspect
import logging
import random
import time
from typing import TYPE_CHECKING, Any

from config import (
    WITCHS_CURSE_COOLDOWN_SECONDS,
    WITCHS_CURSE_DURATION_DAYS,
    WITCHS_CURSE_LOSS_TRIGGER_PCT,
)
from repositories.interfaces import ICurseRepository

if TYPE_CHECKING:
    import discord

    from services.flavor_text_service import FlavorTextService

logger = logging.getLogger("cama_bot.services.curse")

WITCH_PREFIX = "🧙‍♀️"


def spawn_curse_flame(curse_service: Any, channel: Any, **kwargs: Any) -> None:
    """Fire-and-forget helper for hook sites.

    Tolerates ``curse_service is None``, ``channel is None``, MagicMock bot
    objects in tests (where ``curse_service.maybe_flame_and_post(...)`` returns
    a non-coroutine), and any unexpected error during spawn.
    """
    if curse_service is None or channel is None:
        return
    try:
        coro = curse_service.maybe_flame_and_post(channel=channel, **kwargs)
        if inspect.iscoroutine(coro):
            asyncio.create_task(coro)
    except Exception:
        logger.debug("spawn_curse_flame failed", exc_info=True)


class CurseService:
    """Owns the cursed-state read path and the witch flame fire-and-forget hook."""

    def __init__(
        self,
        curse_repo: ICurseRepository,
        flavor_text_service: FlavorTextService | None = None,
    ):
        self.curse_repo = curse_repo
        self.flavor_text_service = flavor_text_service
        # Per-target last-fire timestamps: (target_id, guild_id) -> epoch seconds.
        self._curse_cooldowns: dict[tuple[int, int], float] = {}

    async def cast_curse(
        self,
        caster_id: int,
        target_id: int,
        guild_id: int | None,
        days: int = WITCHS_CURSE_DURATION_DAYS,
    ) -> int:
        """Insert or extend a curse. Returns the resulting unix epoch expires_at."""
        return await asyncio.to_thread(
            self.curse_repo.cast_or_extend,
            guild_id,
            caster_id,
            target_id,
            days,
        )

    def _check_curse_cooldown(self, target_id: int, guild_id: int | None) -> bool:
        """True when the target is off cooldown (or the cooldown is disabled)."""
        if WITCHS_CURSE_COOLDOWN_SECONDS <= 0:
            return True
        last = self._curse_cooldowns.get((target_id, guild_id or 0))
        if last is None:
            return True
        return time.time() - last >= WITCHS_CURSE_COOLDOWN_SECONDS

    def _set_curse_cooldown(self, target_id: int, guild_id: int | None) -> None:
        self._curse_cooldowns[(target_id, guild_id or 0)] = time.time()

    async def maybe_flame_and_post(
        self,
        channel: discord.abc.Messageable | None,
        target_id: int,
        guild_id: int | None,
        system: str,
        outcome: str = "neutral",
        event_context: dict | None = None,
        target_display_name: str | None = None,
    ) -> None:
        """Roll the hex and post a witchfire GIF (with an optional LLM taunt).

        Designed to be wrapped in ``asyncio.create_task`` at hook sites. The GIF
        is pure PIL and fires regardless of AI; the LLM taunt is added only as a
        caption when a flavor service is configured and AI is enabled. Any
        failure path (not a loss, not cursed, on cooldown, roll miss, send
        error) is swallowed silently.
        """
        if channel is None:
            return
        # The hex only bites on bad outcomes; wins/neutral never fire. Cheap check
        # first so the common win/neutral events skip the DB read entirely.
        if outcome != "loss":
            return

        try:
            import discord

            from utils.neon_drawing import create_witch_curse_gif

            now = int(time.time())
            stack_count = await asyncio.to_thread(
                self.curse_repo.count_active_curses_for_target,
                target_id,
                guild_id,
                now,
            )
            if stack_count <= 0:
                return

            if not self._check_curse_cooldown(target_id, guild_id):
                return

            if random.randint(1, 100) > WITCHS_CURSE_LOSS_TRIGGER_PCT:
                return

            # Commit to firing now: set the cooldown before the first await so two
            # near-simultaneous loss tasks for the same target (e.g. losing a match
            # you also bet on) can't both pass the check and double-fire.
            self._set_curse_cooldown(target_id, guild_id)

            # Witchfire GIF — pure PIL, always fires (no AI dependency).
            display = (target_display_name or "the cursed soul").strip()
            buf = await asyncio.to_thread(
                create_witch_curse_gif, display, stack_count=stack_count
            )

            # Optional LLM taunt as the caption. generate_curse_flame self-gates on
            # the per-guild AI setting and returns None when AI is disabled.
            caption: str | None = None
            if self.flavor_text_service is not None:
                flame_text = await self.flavor_text_service.generate_curse_flame(
                    guild_id=guild_id,
                    target_id=target_id,
                    system=system,
                    outcome=outcome,
                    event_context=event_context or {},
                    stack_count=stack_count,
                    target_display_name=target_display_name,
                )
                if flame_text:
                    caption = f"{WITCH_PREFIX} *{flame_text.strip()}*"

            gif_file = discord.File(buf, filename="witch_curse.gif")
            if caption:
                await channel.send(caption, file=gif_file, delete_after=90)
            else:
                await channel.send(file=gif_file, delete_after=90)
        except Exception:
            logger.debug("maybe_flame_and_post failed silently", exc_info=True)
