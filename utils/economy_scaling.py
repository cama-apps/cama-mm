"""Shared economy scaling helpers for generated minigame JC deltas."""

import math

import config


def scale_minigame_jc_delta(amount: int | float) -> int:
    """Scale a generated minigame JC delta with half-up rounding."""
    value = float(amount)
    if value == 0:
        return 0

    magnitude = math.floor(abs(value) * config.MINIGAME_JC_DELTA_SCALE + 0.5)
    magnitude = max(1, magnitude)
    return magnitude if value > 0 else -magnitude
