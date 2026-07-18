"""Shared economy scaling helpers for generated minigame JC deltas."""

import math

import config

DEFLATIONARY_MINIGAME_JC_DELTA_MULTIPLIER = 1.10


def scale_minigame_jc_delta(amount: int | float) -> int:
    """Scale a generated minigame JC delta with half-up rounding."""
    value = float(amount)
    if value == 0:
        return 0

    magnitude = math.floor(abs(value) * config.MINIGAME_JC_DELTA_SCALE + 0.5)
    magnitude = max(1, magnitude)
    return magnitude if value > 0 else -magnitude


def adjust_generated_jc_reward(
    amount: int | float,
    *,
    guild_id: int | None,
    economy_event_service=None,
) -> int:
    """Apply the structural scale, then today's reward policy, exactly once.

    This helper is only for newly generated JC. Transfers, returned stakes,
    refunds, Reserve disbursements, and loan principal must keep using their
    original amount so moving existing liquidity cannot destroy coins.

    Negative values retain the central minigame scale but intentionally skip
    the generic reward policy. Loss-bearing surfaces such as the wheel have
    their own daily loss multiplier.
    """
    adjusted = scale_minigame_jc_delta(amount)
    if adjusted <= 0 or economy_event_service is None:
        return adjusted
    return int(economy_event_service.adjust_reward(guild_id, adjusted))


def scale_deflationary_minigame_jc_delta(amount: int | float) -> int:
    """Scale a minigame JC burn/loss with the global deflation pressure bump."""
    base = scale_minigame_jc_delta(amount)
    stronger = scale_minigame_jc_delta(
        float(amount) * DEFLATIONARY_MINIGAME_JC_DELTA_MULTIPLIER
    )
    if abs(base) < 4:
        return stronger
    if base > 0 and stronger <= base:
        return base + 1
    if base < 0 and stronger >= base:
        return base - 1
    return stronger
