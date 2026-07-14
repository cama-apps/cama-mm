"""Sabotage, cave-in, event, and progression balance knobs for /dig.

Extracted from the original ``dig_constants`` module; see
``services.dig_constants`` for the public facade.
"""

from __future__ import annotations

import math
import random

# ---------------------------------------------------------------------------
# Sabotage / Defense Constants
# ---------------------------------------------------------------------------

SABOTAGE_BASE_COST: int = 5
SABOTAGE_COST_DIVISOR: int = 5             # cost = max(SABOTAGE_BASE_COST, target_depth // SABOTAGE_COST_DIVISOR)
SABOTAGE_SUCCESS_CHANCE: float = 0.50
SABOTAGE_DAMAGE_MIN: int = 3
SABOTAGE_DAMAGE_MAX: int = 8
SABOTAGE_COOLDOWN_SECONDS: int = 43_200    # 12 hours

INSURANCE_BASE_COST: int = 5
INSURANCE_COST_DEPTH_DIVISOR: int = 25     # cost = INSURANCE_BASE_COST + depth // INSURANCE_COST_DEPTH_DIVISOR
INSURANCE_DURATION_SECONDS: int = 86_400   # 24 hours


# ---------------------------------------------------------------------------
# Cave-in Constants
# ---------------------------------------------------------------------------

CAVE_IN_BLOCK_LOSS_MIN: int = 6
CAVE_IN_BLOCK_LOSS_MAX: int = 14

# Depth bands. Cave-ins escalate as the tunnel goes deeper.
CAVE_IN_BAND_SHALLOW: str = "shallow"
CAVE_IN_BAND_MID: str = "mid"
CAVE_IN_BAND_DEEP: str = "deep"
CAVE_IN_BAND_ENDGAME: str = "endgame"

CAVE_IN_DEPTH_BAND_MID: int = 50
CAVE_IN_DEPTH_BAND_DEEP: int = 150
CAVE_IN_DEPTH_BAND_ENDGAME: int = 250

# Per-band block loss ranges (shallow matches legacy MIN/MAX).
CAVE_IN_BLOCK_LOSS_RANGES: dict[str, tuple[int, int]] = {
    CAVE_IN_BAND_SHALLOW: (CAVE_IN_BLOCK_LOSS_MIN, CAVE_IN_BLOCK_LOSS_MAX),
    CAVE_IN_BAND_MID: (8, 18),
    CAVE_IN_BAND_DEEP: (12, 25),
    CAVE_IN_BAND_ENDGAME: (16, 32),
}

# Per-band medical-bill ranges.
CAVE_IN_MEDICAL_BILL_RANGES: dict[str, tuple[int, int]] = {
    CAVE_IN_BAND_SHALLOW: (3, 9),
    CAVE_IN_BAND_MID: (6, 15),
    CAVE_IN_BAND_DEEP: (12, 25),
    CAVE_IN_BAND_ENDGAME: (18, 40),
}

# Stun = extra digs of slower cooldown after the cave-in.
CAVE_IN_STUN_DIGS_BY_BAND: dict[str, int] = {
    CAVE_IN_BAND_SHALLOW: 2,
    CAVE_IN_BAND_MID: 3,
    CAVE_IN_BAND_DEEP: 4,
    CAVE_IN_BAND_ENDGAME: 5,
}

# Injury = digs of reduced advance after the cave-in.
CAVE_IN_INJURY_DIGS_BY_BAND: dict[str, int] = {
    CAVE_IN_BAND_SHALLOW: 3,
    CAVE_IN_BAND_MID: 4,
    CAVE_IN_BAND_DEEP: 5,
    CAVE_IN_BAND_ENDGAME: 6,
}

# Consequence weights (percent) per band. Total of each row = 100. New
# consequence types appear at deeper bands. The resolver must reroll if the
# selected consequence isn't applicable in the current state (e.g.
# spilled_satchel with empty inventory).
CAVE_IN_CONSEQUENCE_WEIGHTS: dict[str, list[tuple[str, int]]] = {
    CAVE_IN_BAND_SHALLOW: [
        ("stun", 30), ("injury", 30), ("medical_bill", 40),
    ],
    CAVE_IN_BAND_MID: [
        ("stun", 25), ("injury", 25), ("medical_bill", 30),
        ("gear_nick", 10), ("spilled_satchel", 5),
        ("snuffed_light", 4), ("cracked_hat", 1),
    ],
    CAVE_IN_BAND_DEEP: [
        ("stun", 20), ("injury", 20), ("medical_bill", 25),
        ("gear_nick", 15), ("spilled_satchel", 8),
        ("snuffed_light", 7), ("cracked_hat", 5),
    ],
    CAVE_IN_BAND_ENDGAME: [
        ("stun", 18), ("injury", 18), ("medical_bill", 22),
        ("gear_nick", 18), ("spilled_satchel", 10),
        ("snuffed_light", 9), ("cracked_hat", 5),
    ],
}

# Catastrophic sub-roll: after the cave-in resolves, this fraction become
# catastrophic instead. Catastrophic cave-ins layer on heavy effects:
# multi-dig stun, depth roll-back to nearest milestone, all temp_buffs
# cleared, deep gear hit, and a heavy medical bill.
CAVE_IN_CATASTROPHIC_PCT_BY_BAND: dict[str, float] = {
    CAVE_IN_BAND_SHALLOW: 0.0,
    CAVE_IN_BAND_MID: 0.01,
    CAVE_IN_BAND_DEEP: 0.03,
    CAVE_IN_BAND_ENDGAME: 0.05,
}

CAVE_IN_CATASTROPHIC_MEDICAL_BILL: tuple[int, int] = (50, 200)
CAVE_IN_CATASTROPHIC_STUN_DIGS_RANGE: tuple[int, int] = (3, 5)
CAVE_IN_CATASTROPHIC_GEAR_TICKS: int = 3
CAVE_IN_CATASTROPHIC_MILESTONE_STEP: int = 25  # roll back to floor((depth-1)/step)*step

# Negative-event tuning: central scalars so risky/desperate failures bite a bit
# harder without editing every authored event. Applied at resolution time in
# events_mixin. Only negative JC and negative curse effects are scaled —
# positive payouts and cave-in medical bills are untouched.
NEGATIVE_EVENT_JC_MULTIPLIER: float = 1.3   # scales flat JC *losses* on failures
CURSE_STRENGTH_MULT: float = 1.3            # scales negative curse effect magnitudes
CURSE_DURATION_BONUS_DIGS: int = 1          # +N digs on every applied curse
DIG_EVENT_DEFLATION_MULTIPLIER: float = 1.10


def strengthen_dig_event_penalty(amount: int) -> int:
    """Strengthen an authored event penalty, rounding away from zero."""
    if amount == 0:
        return 0
    magnitude = math.ceil(abs(amount) * DIG_EVENT_DEFLATION_MULTIPLIER)
    return magnitude if amount > 0 else -magnitude

# Helltide bell: marquee guild-wide modifier set by an event. While active,
# every dig in the guild burns this many JC from its yield (pure deflation,
# coins are destroyed not transferred).
HELLTIDE_MODIFIER_ID: str = "helltide_active"
HELLTIDE_TAX_PER_DIG: int = 5


def cave_in_band(depth: int) -> str:
    """Classify a tunnel depth into one of the four cave-in escalation bands."""
    if depth >= CAVE_IN_DEPTH_BAND_ENDGAME:
        return CAVE_IN_BAND_ENDGAME
    if depth >= CAVE_IN_DEPTH_BAND_DEEP:
        return CAVE_IN_BAND_DEEP
    if depth >= CAVE_IN_DEPTH_BAND_MID:
        return CAVE_IN_BAND_MID
    return CAVE_IN_BAND_SHALLOW


def pick_cave_in_consequence(
    band: str,
    *,
    has_consumables: bool,
    has_equipped_gear: bool,
    can_lower_luminosity: bool,
    has_hard_hat_charges: bool,
) -> str:
    """Weighted-pick a cave-in consequence id for the given band.

    Filters out consequences whose state requirement isn't met, then renormalizes
    the remaining weights and rolls. Falls back to ``"medical_bill"`` if no
    consequence is applicable (extremely unlikely, but defensive).
    """
    applicable = {
        "stun": True,
        "injury": True,
        "medical_bill": True,
        "gear_nick": has_equipped_gear,
        "spilled_satchel": has_consumables,
        "snuffed_light": can_lower_luminosity,
        "cracked_hat": has_hard_hat_charges,
    }
    weights = [(cid, w) for cid, w in CAVE_IN_CONSEQUENCE_WEIGHTS[band] if applicable.get(cid, False) and w > 0]
    if not weights:
        return "medical_bill"
    total = sum(w for _, w in weights)
    roll = random.randint(1, total)
    upto = 0
    for cid, w in weights:
        upto += w
        if roll <= upto:
            return cid
    return weights[-1][0]


def roll_catastrophic_cave_in(band: str) -> bool:
    """True if this cave-in escalates to catastrophic for the given band."""
    pct = CAVE_IN_CATASTROPHIC_PCT_BY_BAND.get(band, 0.0)
    if pct <= 0:
        return False
    return random.random() < pct


# ---------------------------------------------------------------------------
# Miscellaneous
# ---------------------------------------------------------------------------

MAX_INVENTORY_SLOTS: int = 8
ABANDON_REFUND_PCT: float = 0.10           # 10% of depth in JC


# ---------------------------------------------------------------------------
# Progressive Tips
# ---------------------------------------------------------------------------

PROGRESSIVE_TIPS: list[tuple[int, int | None, str]] = [
    # (min_depth, max_depth_or_None, tip_text)
    (0,  10,   "Use /dig to advance your tunnel. Your first dig each day is free!"),
    (0,  10,   "Buy items from the shop with /dig shop. Dynamite blasts through rock fast."),
    (0,  10,   "Each layer gets harder but more rewarding. Keep digging!"),
    (10, 25,   "Ask a friend to /dig help you — it slows down decay too."),
    (10, 25,   "Watch out for sabotage! Buy insurance to protect your tunnel."),
    (10, 25,   "Set a trap to punish anyone who tries to sabotage you."),
    (25, 50,   "Bosses guard each layer boundary. Choose your strategy wisely."),
    (25, 50,   "Prestige resets your depth but grants permanent bonuses."),
    (25, 50,   "Upgrade your pickaxe for better digging performance."),
    (50, None, "Relics give permanent bonuses — equip them from your inventory."),
    (50, None, "Deeper layers have rarer artifacts. Keep exploring!"),
    (50, None, "Stack sabotage defenses: insurance + reinforcement + relics."),
]
