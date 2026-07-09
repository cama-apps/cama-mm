"""Shared constants, pure helpers, and the logger for the dig service package.

These names lived at module scope in the former monolithic
``services.dig_service`` module. They are hoisted here so the focused mixin
modules can import them without forming an import cycle through
``services.dig_service`` itself. ``services.dig_service`` re-exports the
public names so existing ``from services.dig_service import ...`` callers and
tests keep working unchanged.
"""

import logging
import random

from services.dig_constants import (
    LUMINOSITY_BRIGHT,
    LUMINOSITY_DARK,
    LUMINOSITY_DARK_HIT_PENALTY,
    LUMINOSITY_DIM,
    LUMINOSITY_DIM_HIT_PENALTY,
    LUMINOSITY_PITCH_BOSS_DMG_BONUS,
    LUMINOSITY_PITCH_HIT_PENALTY,
    WIN_CHANCE_CAP,
    WIN_CHANCE_FLOOR,
)

logger = logging.getLogger("cama_bot.services.dig")

RARITY_WEIGHTS = {"common": 70, "uncommon": 20, "rare": 15, "legendary": 10}
DIG_STARTING_STAT_POINTS = 5
DIG_BOSS_STAT_POINT_BONUS = 1
MINER_BACKSTORY_MAX_LENGTH = 600
STRENGTH_MAX_ADVANCE_INTERVAL = 2
STRENGTH_MIN_ADVANCE_INTERVAL = 5
SMARTS_CAVE_IN_REDUCTION = 0.02
STAMINA_COOLDOWN_REDUCTION = 0.04
STAMINA_MAX_REDUCTION = 0.50


def _splash_trigger_matches(trigger: str, succeeded: bool) -> bool:
    """Does the event's splash config fire on this outcome?"""
    if trigger == "always":
        return True
    if trigger == "success":
        return bool(succeeded)
    # default "failure"
    return not succeeded


def _splash_to_dict(result) -> dict | None:
    """Serialize a :class:`SplashResult` for return from resolve_event."""
    if result is None:
        return None
    if not getattr(result, "victims", None) and not getattr(
        result, "absorbed_total", 0
    ):
        return None
    return {
        "strategy": result.strategy,
        "event_name": result.event_name,
        "victims": [{"discord_id": vid, "amount": amt} for vid, amt in result.victims],
        "total_burned": result.total_burned,
        "mode": getattr(result, "mode", "burn"),
        "absorbed_total": getattr(result, "absorbed_total", 0),
        "shielded_count": getattr(result, "shielded_count", 0),
    }


def _approx_duel_win_prob(
    *, player_hp: int, boss_hp: int,
    player_hit: float, player_dmg: int,
    boss_hit: float, boss_dmg: int,
    crit_chance: float = 0.0, crit_bonus: int = 0,
    trials: int = 500,
) -> float:
    """Estimate the probability the player wins a boss HP duel.

    Used by ``scout_boss`` to surface an approximate win% to players
    without resolving an actual fight. Monte Carlo with a local ``Random``
    so the estimate does not consume the global RNG stream (important for
    deterministic dig tests).
    """
    if player_hp <= 0 or boss_hp <= 0:
        return 0.0
    if trials <= 0:
        return 0.0
    rng = random.Random()
    wins = 0
    for _ in range(trials):
        php, bhp = player_hp, boss_hp
        while True:
            if rng.random() < player_hit:
                dmg = player_dmg
                if crit_chance > 0 and rng.random() < crit_chance:
                    dmg += crit_bonus
                bhp -= dmg
            if bhp <= 0:
                wins += 1
                break
            if rng.random() < boss_hit:
                php -= boss_dmg
            if php <= 0:
                break
    raw = wins / trials
    return max(WIN_CHANCE_FLOOR, min(WIN_CHANCE_CAP, raw))


def _luminosity_combat_penalty(luminosity: int) -> tuple[float, int]:
    """Translate current luminosity into (player_hit_offset, boss_dmg_bonus).

    Bright (>=76) → (0, 0)
    Dim (26-75) → (-0.03, 0)
    Dark (1-25) → (-0.08, 0)
    Pitch black (0) → (-0.15, +1)
    """
    if luminosity >= LUMINOSITY_BRIGHT:
        return (0.0, 0)
    if luminosity >= LUMINOSITY_DIM:
        return (-LUMINOSITY_DIM_HIT_PENALTY, 0)
    if luminosity >= LUMINOSITY_DARK:
        return (-LUMINOSITY_DARK_HIT_PENALTY, 0)
    return (-LUMINOSITY_PITCH_HIT_PENALTY, LUMINOSITY_PITCH_BOSS_DMG_BONUS)


def _prestige_cave_in_multiplier(prestige_level: int) -> float:
    """Soft prestige scaling on cave-in chance.

    P0 → 0.90×, P3 → 0.99× (~current), P10 → 1.20×. Stacks multiplicatively
    with relic / lantern / OVERGROWTH halving.
    """
    return 0.9 + max(0, prestige_level) * 0.03
