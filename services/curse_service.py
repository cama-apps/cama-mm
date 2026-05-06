"""Witch's Curse service.

Coordinates curse purchases and the per-engagement flame rolls. Hook sites
spawn ``maybe_flame_and_post`` as a background task; it returns silently when
the target is not cursed, AI is disabled, the roll fails, or the LLM returns
nothing — so callers never need to await it.
"""

from __future__ import annotations

import asyncio
import inspect
import logging
import random
import time
from typing import TYPE_CHECKING, Any

from config import (
    WITCHS_CURSE_DURATION_DAYS,
    WITCHS_CURSE_LOSS_TRIGGER_PCT,
    WITCHS_CURSE_WIN_TRIGGER_PCT,
)
from repositories.interfaces import ICurseRepository, IGuildConfigRepository

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
        guild_config_repo: IGuildConfigRepository | None = None,
    ):
        self.curse_repo = curse_repo
        self.flavor_text_service = flavor_text_service
        self.guild_config_repo = guild_config_repo

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

    def _trigger_pct(self, outcome: str) -> int:
        if outcome == "loss":
            return WITCHS_CURSE_LOSS_TRIGGER_PCT
        # win and neutral both get the lower rate
        return WITCHS_CURSE_WIN_TRIGGER_PCT

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
        """Run the cursed-check + roll + LLM call + post the flame.

        Designed to be wrapped in ``asyncio.create_task`` at hook sites. Any
        failure path (not cursed, AI disabled, roll miss, LLM None, send error)
        is swallowed silently.
        """
        if channel is None or self.flavor_text_service is None:
            return

        try:
            now = int(time.time())
            stack_count = await asyncio.to_thread(
                self.curse_repo.count_active_curses_for_target,
                target_id,
                guild_id,
                now,
            )
            if stack_count <= 0:
                return

            # Per-guild AI gate. Treat None guild as "default to disabled" the same
            # way FlavorTextService.generate_event_flavor does.
            if guild_id is not None and self.guild_config_repo is not None:
                ai_enabled = await asyncio.to_thread(
                    self.guild_config_repo.get_ai_enabled, guild_id
                )
                if not ai_enabled:
                    return

            pct = self._trigger_pct(outcome)
            if random.randint(1, 100) > pct:
                return

            flame_text = await self.flavor_text_service.generate_curse_flame(
                guild_id=guild_id,
                target_id=target_id,
                system=system,
                outcome=outcome,
                event_context=event_context or {},
                stack_count=stack_count,
                target_display_name=target_display_name,
            )
            if not flame_text:
                return

            formatted = f"{WITCH_PREFIX} *{flame_text.strip()}*"
            await channel.send(formatted)
        except Exception:
            logger.debug("maybe_flame_and_post failed silently", exc_info=True)
