"""Async command-layer adapter for the synchronous pet activity boundary."""

from __future__ import annotations

import asyncio

from domain.pet_evolution import PetActivity


async def record_pet_activity(
    bot,
    discord_id: int,
    guild_id: int | None,
    activity: PetActivity,
    source_key: str,
    *,
    occurred_at: int | None = None,
) -> int:
    service = getattr(bot, "pet_evolution_service", None)
    if service is None:
        return 0
    kwargs = {}
    if occurred_at is not None:
        kwargs["occurred_at"] = occurred_at
    return await asyncio.to_thread(
        service.record_activity,
        discord_id,
        guild_id,
        activity,
        source_key,
        **kwargs,
    )
