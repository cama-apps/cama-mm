"""Luminosity, prestige, ascension, mutation, and corruption data for /dig.

Extracted from the original ``dig_constants`` module; see
``services.dig_constants`` for the public facade.
"""

from __future__ import annotations

from dataclasses import dataclass

# ---------------------------------------------------------------------------
# Luminosity Constants
# ---------------------------------------------------------------------------

LUMINOSITY_MAX: int = 100

# Luminosity drain per dig, by layer name. Layers not listed have 0 drain.
LUMINOSITY_DRAIN_PER_DIG: dict[str, int] = {
    "Crystal": 0,
    "Magma": 3,
    "Abyss": 5,
    "Fungal Depths": 2,
    "Frozen Core": 7,
    "The Hollow": 10,
}

# Hard depth wall: the deep refuses to yield further. Players hitting
# this depth must prestige to continue. Sized so that post-pinnacle
# (depth 350) progress feels like a slow descent under pressure rather
# than an open runway.
PRESTIGE_HARD_CAP: int = 500
# Past this depth the drain rate accelerates linearly — +1 extra drain
# per LUMINOSITY_DEEP_DRAIN_BLOCKS_PER_STEP blocks. At cap (depth 500)
# the bonus is +10, doubling The Hollow's base drain.
LUMINOSITY_DEEP_DRAIN_START_DEPTH: int = 350
LUMINOSITY_DEEP_DRAIN_BLOCKS_PER_STEP: int = 20

# Pinnacle catch-up: if a player tunneled past the pinnacle without
# defeating it (legacy tunnels that pre-date the pinnacle, or skipped
# encounters), the pinnacle re-procs at this depth so prestige isn't
# permanently locked out. Tier bosses must still all be cleared.
PINNACLE_REPROC_DEPTH: int = 450

# Thresholds and their gameplay effects
LUMINOSITY_BRIGHT: int = 76       # 76-100: normal
LUMINOSITY_DIM: int = 26          # 26-75: +5% cave-in, 1.5x event chance
LUMINOSITY_DARK: int = 1          # 1-25: +15% cave-in, forced events, risky -10%, JC +25%
LUMINOSITY_PITCH_BLACK: int = 0   # 0: +25% cave-in, forced risky, JC +50%, darkness events

LUMINOSITY_DIM_CAVE_IN_BONUS: float = 0.05
LUMINOSITY_DIM_EVENT_MULTIPLIER: float = 1.5
LUMINOSITY_DARK_CAVE_IN_BONUS: float = 0.15
LUMINOSITY_DARK_EVENT_MULTIPLIER: float = 2.5
LUMINOSITY_DARK_RISKY_PENALTY: float = 0.10   # subtracted from risky success chance
LUMINOSITY_DARK_JC_MULTIPLIER: float = 1.25
LUMINOSITY_PITCH_CAVE_IN_BONUS: float = 0.25
LUMINOSITY_PITCH_EVENT_MULTIPLIER: float = 3.0
LUMINOSITY_PITCH_FORCE_RISKY: bool = True      # safe option removed at pitch black
LUMINOSITY_PITCH_JC_MULTIPLIER: float = 1.50

# Boss combat penalties from low luminosity (boss revamp)
LUMINOSITY_DIM_HIT_PENALTY: float = 0.03         # -3% player_hit at Dim
LUMINOSITY_DARK_HIT_PENALTY: float = 0.08        # -8% player_hit at Dark
LUMINOSITY_PITCH_HIT_PENALTY: float = 0.15       # -15% player_hit at Pitch Black
LUMINOSITY_PITCH_BOSS_DMG_BONUS: int = 1         # bosses hit harder in pitch black

# Slow on-demand refill — replaces the old daily snap-back to 100.
# Recovery is computed as floor(hours_elapsed * (REFILL_PER_DAY / 24)) on
# every dig and boss encounter, using `last_lum_update_at` on the tunnel.
LUMINOSITY_REFILL_PER_DAY: int = 20


# ---------------------------------------------------------------------------
# Prestige Constants
# ---------------------------------------------------------------------------

MAX_PRESTIGE: int = 10

PRESTIGE_CROWNS: dict[int, str] = {
    0: "",
    1: "\u26cf\ufe0f",      # pick
    2: "\U0001f48e",         # gem
    3: "\U0001f451",         # crown
    4: "\U0001f4a0",         # diamond with dot
    5: "\u2b50",             # star
    6: "\U0001f531",         # trident
    7: "\u267e\ufe0f",       # infinity
    8: "\U0001f525",         # fire
    9: "\U0001f30c",         # milky way
    10: "\U0001f5a4",        # black heart
}

RELIC_SLOTS_BASE: int = 1  # relic_slots = min(prestige_level + RELIC_SLOTS_BASE, RELIC_SLOTS_MAX)
RELIC_SLOTS_MAX: int = 6   # hard ceiling so slots stop scaling with prestige forever

PRESTIGE_PERKS: list[str] = [
    "advance_boost",
    "cave_in_resistance",
    "loot_multiplier",
    "mixed_bonus",
    "deep_sight",
    "veteran_miner",
    "tunnel_mastery",
    "dark_adaptation",
    "the_endless",
    "patient_step",
    "steady_hands",
    "reading_the_stone",
]

# Soft cap on how many times a single perk can be picked across prestiges.
# At-cap perks are simply hidden from the random picker — players see only
# perks they can still pick.
PRESTIGE_PERK_STACK_CAP: int = 5

# User-facing perk display names. Keys here override the default
# title-case-the-id rendering. Internal IDs (PRESTIGE_PERKS keys) stay the
# same — only the label changes — so existing prestige_perks JSON is intact.
PRESTIGE_PERK_DISPLAY_NAMES: dict[str, str] = {
    "loot_multiplier": "Loot Bonus",
}


def perk_display_name(perk_id: str) -> str:
    """Return the user-facing label for a perk id."""
    return PRESTIGE_PERK_DISPLAY_NAMES.get(perk_id) or perk_id.replace("_", " ").title()

# Per-pick mechanical bonuses for each perk. The aggregator sums these
# across all picked perks to produce the player's effective effect dict.
PRESTIGE_PERK_VALUES: dict[str, dict[str, float]] = {
    "advance_boost": {"advance_min_bonus": 1.0},
    "cave_in_resistance": {"cave_in_reduction": 0.05},
    "loot_multiplier": {"jc_bonus": 1.0},
    "mixed_bonus": {"advance_min_bonus": 0.5, "cave_in_reduction": 0.02, "jc_bonus": 0.5},
    "deep_sight": {"luminosity_drain_reduction": 0.25},
    "veteran_miner": {"risky_success_bonus": 0.05},
    "tunnel_mastery": {"expedition_reward_bonus": 0.50},
    "dark_adaptation": {"dim_cave_in_immunity": 1.0},
    "the_endless": {"hollow_advance_bonus": 1.0},  # The Hollow advance becomes 1-2
    "patient_step": {"streak_bonus_multiplier": 0.5},
    "steady_hands": {"cave_in_loss_reduction": 0.25},
    "reading_the_stone": {"event_choice_reveal": 1.0},
}


# ---------------------------------------------------------------------------
# Ascension Modifiers (stacking per prestige level)
# ---------------------------------------------------------------------------

@dataclass(frozen=True)
class AscensionModifier:
    """Difficulty + reward modifier activated at a prestige level."""
    level: int
    name: str
    penalty: str          # player-facing penalty description
    reward: str           # player-facing reward description
    effects: dict         # mechanical effects dict
    gameplay: bool        # True if this introduces new mechanics (not just numbers)


ASCENSION_MODIFIERS: dict[int, AscensionModifier] = {
    1: AscensionModifier(
        level=1, name="Dense Stone",
        penalty="Stone is denser but richer",
        reward="JC loot +18%",
        effects={"jc_multiplier": 0.18},
        gameplay=False,
    ),
    2: AscensionModifier(
        level=2, name="Unstable Ground",
        penalty="Cave-in chance +3%, dig JC -3%",
        reward="Event chance +20%",
        effects={
            "cave_in_bonus": 0.03,
            "event_chance_multiplier": 0.20,
            "jc_layer_penalty": 0.03,
        },
        gameplay=False,
    ),
    3: AscensionModifier(
        level=3, name="Hungry Darkness",
        penalty="Luminosity drains 25% faster, dig JC -2%",
        reward="Rare events 50% more common",
        effects={
            "luminosity_drain_multiplier": 0.25,
            "rare_event_multiplier": 0.50,
            "jc_layer_penalty": 0.02,
        },
        gameplay=False,
    ),
    4: AscensionModifier(
        level=4, name="Boss Rage",
        penalty="Bosses fight harder the longer you delve",
        reward="Boss payouts +50%",
        effects={"boss_payout_multiplier": 0.50, "jc_layer_penalty": 0.05},
        gameplay=False,
    ),
    5: AscensionModifier(
        level=5, name="Erosion",
        penalty="Decay rate +50%",
        reward="Milestone rewards +50%",
        effects={"decay_multiplier": 0.50, "milestone_multiplier": 0.50, "jc_layer_penalty": 0.07},
        gameplay=False,
    ),
    6: AscensionModifier(
        level=6, name="Corruption",
        penalty="Each dig rolls a random micro-modifier",
        reward="Artifact find rate doubled",
        effects={"corruption": True, "artifact_multiplier": 2.0, "jc_layer_penalty": 0.05},
        gameplay=True,
    ),
    7: AscensionModifier(
        level=7, name="Event Chains",
        penalty="Events can chain (same or higher rarity)",
        reward="Chained events give 1.5x JC",
        effects={"event_chain": True, "chain_jc_multiplier": 1.5},
        gameplay=True,
    ),
    8: AscensionModifier(
        level=8, name="Mutations",
        penalty="1 forced random mutation per prestige run",
        reward="Choose 1 mutation from 3 (may be positive)",
        effects={"mutations": True},
        gameplay=True,
    ),
    9: AscensionModifier(
        level=9, name="Cruel Echoes",
        penalty="Safe event options now have 10% failure chance",
        reward="Legendary events 3x more common",
        effects={"cruel_safe_fail": 0.10, "legendary_event_multiplier": 3.0},
        gameplay=True,
    ),
    10: AscensionModifier(
        level=10, name="The Endless",
        penalty="Paid dig costs +50%",
        reward="Score multiplier 2x",
        effects={"paid_dig_cost_multiplier": 0.50, "score_multiplier": 2.0},
        gameplay=False,
    ),
}


# ---------------------------------------------------------------------------
# Mutation Definitions (P8+)
# ---------------------------------------------------------------------------

@dataclass(frozen=True)
class MutationDef:
    """A permanent run quirk assigned at prestige (P8+)."""
    id: str
    name: str
    description: str
    positive: bool
    effects: dict


MUTATIONS_POOL: list[MutationDef] = [
    # Positive
    MutationDef("cave_in_loot", "Lucky Rubble", "Cave-ins have 30% chance to drop 1-3 JC", True,
                {"cave_in_loot_chance": 0.30, "cave_in_loot_min": 1, "cave_in_loot_max": 3}),
    MutationDef("dark_sight", "Dark Sight", "No luminosity penalty to cave-in chance", True,
                {"ignore_luminosity_cave_in": True}),
    MutationDef("thick_skin", "Thick Skin", "First cave-in each day is prevented", True,
                {"daily_cave_in_shield": True}),
    MutationDef("treasure_sense", "Treasure Sense", "+25% artifact find chance", True,
                {"artifact_chance_bonus": 0.25}),
    MutationDef("event_magnet", "Event Magnet", "+30% event encounter rate", True,
                {"event_chance_bonus": 0.30}),
    MutationDef("second_wind", "Second Wind", "After cave-in, next dig gets +3 advance", True,
                {"post_cave_in_advance": 3}),
    # Negative
    MutationDef("brittle_walls", "Brittle Walls", "Cave-in block loss +2", False,
                {"cave_in_loss_bonus": 2}),
    MutationDef("heavy_air", "Heavy Air", "Advance max -1", False,
                {"advance_max_penalty": 1}),
    MutationDef("jinxed", "Jinxed", "5% chance any dig yields 0 JC", False,
                {"zero_jc_chance": 0.05}),
    MutationDef("paranoia", "Paranoia", "Sabotage damage +25%", False,
                {"sabotage_damage_bonus": 0.25}),
    MutationDef("restless", "Restless", "Free dig cooldown +1 hour", False,
                {"cooldown_bonus_seconds": 3600}),
    MutationDef("fragile", "Fragile", "Injuries last 1 extra dig", False,
                {"injury_duration_bonus": 1}),
]

MUTATION_BY_ID: dict[str, MutationDef] = {m.id: m for m in MUTATIONS_POOL}


# ---------------------------------------------------------------------------
# Corruption Effects (P6+)
# ---------------------------------------------------------------------------

@dataclass(frozen=True)
class CorruptionEffect:
    """A one-dig micro-modifier rolled at P6+."""
    id: str
    description: str
    weird: bool           # True = chaotic/humorous, False = straightforward negative
    effects: dict


CORRUPTION_EFFECTS: list[CorruptionEffect] = [
    # Bad (80% weight)
    CorruptionEffect("corrupt_jc", "-1 JC this dig", False, {"jc_penalty": 1}),
    CorruptionEffect("corrupt_cave_in", "+3% cave-in this dig", False, {"cave_in_bonus": 0.03}),
    CorruptionEffect("corrupt_advance", "-1 advance this dig", False, {"advance_penalty": 1}),
    CorruptionEffect("corrupt_luminosity", "-5 extra luminosity drain", False, {"luminosity_drain": 5}),
    CorruptionEffect("corrupt_no_artifact", "No artifact roll this dig", False, {"skip_artifact": True}),
    CorruptionEffect("corrupt_risky", "Risky event success -5%", False, {"risky_penalty": 0.05}),
    # Weird (20% weight)
    CorruptionEffect("corrupt_double_half", "JC doubled then halved (net loss on odd)", True,
                     {"double_half_jc": True}),
    CorruptionEffect("corrupt_ominous_name", "Tunnel name temporarily changes to something ominous", True,
                     {"ominous_name": True}),
    CorruptionEffect("corrupt_fixed_jc", "You find exactly 1 JC. Always 1. No more, no less.", True,
                     {"fixed_jc": 1}),
    CorruptionEffect("corrupt_echo", "Your pickaxe swings echo twice. Advance is rolled twice, take the lower.", True,
                     {"min_advance_roll": True}),
]

CORRUPTION_BAD: list[CorruptionEffect] = [c for c in CORRUPTION_EFFECTS if not c.weird]
CORRUPTION_WEIRD: list[CorruptionEffect] = [c for c in CORRUPTION_EFFECTS if c.weird]

