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
