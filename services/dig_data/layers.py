"""Layer definitions, pacing constants, and layer weather for /dig.

Extracted from the original ``dig_constants`` module; see
``services.dig_constants`` for the public facade.
"""

from __future__ import annotations

from dataclasses import dataclass

# ---------------------------------------------------------------------------
# Layer Definitions
# ---------------------------------------------------------------------------

@dataclass(frozen=True)
class LayerDef:
    """Immutable definition for a tunnel layer."""
    name: str
    depth_min: int
    depth_max: int | None          # None means unbounded (Abyss)
    cave_in_pct: float             # base probability 0-1
    jc_min: int
    jc_max: int
    advance_min: int
    advance_max: int
    emoji: str


_LAYERS_DEF: list[LayerDef] = [
    LayerDef("Dirt",          0,   25,  0.05, 0,  1,  1, 3, "\U0001f7eb"),        # brown square
    LayerDef("Stone",         26,  50,  0.10, 0,  1,  1, 3, "\u2b1c"),            # gray (white square)
    LayerDef("Crystal",       51,  75,  0.18, 0,  2,  1, 2, "\U0001f48e"),        # diamond
    LayerDef("Magma",         76,  100, 0.25, 1,  3,  1, 2, "\U0001f525"),        # fire
    LayerDef("Abyss",         101, 150, 0.35, 1,  4,  1, 2, "\U0001f573\ufe0f"),  # hole
    LayerDef("Fungal Depths", 151, 200, 0.40, 1,  5,  1, 2, "\U0001f344"),        # mushroom
    LayerDef("Frozen Core",   201, 275, 0.45, 2,  4,  1, 2, "\u2744\ufe0f"),      # snowflake
    LayerDef("The Hollow",    276, None, 0.50, 2,  6,  1, 1, "\u26ab"),           # black circle
]

LAYER_BOUNDARIES: list[int] = [25, 50, 75, 100, 150, 200, 275]


def get_layer(depth: int) -> LayerDef:
    """Return the layer definition for a given depth."""
    for layer in reversed(_LAYERS_DEF):
        if depth >= layer.depth_min:
            return layer
    return _LAYERS_DEF[0]


# ---------------------------------------------------------------------------
# Pacing Constants
# ---------------------------------------------------------------------------

FREE_DIG_COOLDOWN_SECONDS: int = 7_200           # 2 hours
CHEER_COOLDOWN_SECONDS: int = 30                 # short anti-spam, independent of dig

PAID_DIG_COSTS_PER_DAY: list[int] = [3, 5, 10, 20, 40]
PAID_DIG_COST_CAP: int = 40

# First dig guarantees
FIRST_DIG_ADVANCE_MIN: int = 3
FIRST_DIG_ADVANCE_MAX: int = 7
FIRST_DIG_JC_MIN: int = 1
FIRST_DIG_JC_MAX: int = 5
BASE_DIG_JC_PAYOUT_CAP: int = 20

# Milestone rewards: depth -> JC bonus.
# Only awarded the first time a tunnel reaches each depth (tracked via
# ``tunnels.max_depth``) so bosses knocking players back and forth do not
# farm the bonuses repeatedly.
MILESTONES: dict[int, int] = {
    25: 3,
    50: 6,
    75: 12,
    100: 20,
    150: 30,
    200: 50,
    275: 80,
    350: 100,
    400: 150,
}

# Streak rewards: consecutive-day count -> JC bonus.
# Shared by both /dig and Dota match payouts so the two systems can't drift.
STREAKS: dict[int, int] = {
    3: 1,
    7: 3,
    14: 6,
    30: 10,
}
DIG_STREAK_JC_PAYOUT_CAP: int = 10


# ---------------------------------------------------------------------------
# Layer Weather (daily modifiers)
# ---------------------------------------------------------------------------
# Each game day, 2 layers get weather. At least 1 targets a populated layer.
# Effects use the same modifier keys as ascension/corruption.

@dataclass(frozen=True)
class LayerWeather:
    """A weather condition that can affect a layer for a day."""
    id: str
    name: str
    layer: str
    description: str              # player-facing flavour
    effects: dict                 # modifier dict consumed by dig flow


LAYER_WEATHER_POOL: dict[str, list[LayerWeather]] = {
    "Dirt": [
        LayerWeather("earthworm_migration", "Earthworm Migration", "Dirt",
                     "Worms churn the soil. Digging is easy, but they ate all the coins.",
                     {"advance_bonus": 1, "jc_bonus": -1}),
        LayerWeather("mudslide_warning", "Mudslide Warning", "Dirt",
                     "The ground is slick. Cave-ins happen more, but the mud cushions the fall.",
                     {"cave_in_bonus": 0.10, "cave_in_loss_cap": 3}),
        LayerWeather("root_overgrowth", "Root Overgrowth", "Dirt",
                     "Ancient roots crack the earth open, revealing buried things.",
                     {"advance_bonus": -1, "artifact_multiplier": 2.0}),
    ],
    "Stone": [
        LayerWeather("fossil_rush", "Fossil Rush", "Stone",
                     "The stone is unusually rich with fossils today.",
                     {"artifact_multiplier": 2.0}),
        LayerWeather("seismic_tremors", "Seismic Tremors", "Stone",
                     "The ground won't stop shaking. Things keep falling out of the walls.",
                     {"cave_in_bonus": 0.08, "event_chance_multiplier": 0.50}),
        LayerWeather("mineral_vein", "Mineral Vein", "Stone",
                     "A rich vein of ore runs through the entire layer.",
                     {"jc_bonus": 2}),
    ],
    "Crystal": [
        LayerWeather("crystal_resonance", "Crystal Resonance", "Crystal",
                     "The crystals hum in harmony. Fortune favors the bold today.",
                     {"risky_success_bonus": 0.15, "jc_multiplier": -0.25}),
        LayerWeather("prismatic_surge", "Prismatic Surge", "Crystal",
                     "Light refracts wildly. Strange things emerge from the rainbows.",
                     {"event_chance_multiplier": 1.0, "event_jc_bonus": 3}),
        LayerWeather("shatter_warning", "Shatter Warning", "Crystal",
                     "The crystals are unstable. Dangerous, but the shards are valuable.",
                     {"cave_in_bonus": 0.12, "jc_bonus": 3}),
    ],
    "Magma": [
        LayerWeather("eruption", "Eruption", "Magma",
                     "The magma is surging. Everything is more dangerous and more rewarding.",
                     {"cave_in_bonus": 0.12, "jc_multiplier": 0.75}),
        LayerWeather("cooling_period", "Cooling Period", "Magma",
                     "The lava recedes. Safe, but the good stuff cooled over.",
                     {"cave_in_bonus": -0.10, "jc_multiplier": -0.25}),
        LayerWeather("lava_bloom", "Lava Bloom", "Magma",
                     "Rare minerals crystallize in the cooling lava pools.",
                     {"artifact_multiplier": 1.5, "luminosity_drain_multiplier": 0.50}),
    ],
    "Abyss": [
        LayerWeather("void_tide", "Void Tide", "Abyss",
                     "The void pushes back harder, but rewards those who push through.",
                     {"risky_success_bonus": 0.15, "cave_in_loss_bonus": 2}),
        LayerWeather("whisper_storm", "Whisper Storm", "Abyss",
                     "The whispers are deafening. Events cascade into each other.",
                     {"event_chance_multiplier": 1.0, "event_chain_bonus": 0.25}),
        LayerWeather("deep_calm", "Deep Calm", "Abyss",
                     "An eerie stillness. Safe, but boring.",
                     {"cave_in_bonus": -0.12, "event_chance_multiplier": -0.50}),
    ],
    "Fungal Depths": [
        LayerWeather("spore_bloom", "Spore Bloom", "Fungal Depths",
                     "Bioluminescent spores flood the tunnels. You dig faster but the light burns out.",
                     {"advance_bonus": 2, "luminosity_drain_multiplier": 1.0}),
        LayerWeather("mycelium_pulse", "Mycelium Pulse", "Fungal Depths",
                     "The fungal network is active. It shares wealth with those who listen.",
                     {"jc_multiplier": 0.50, "event_chance_multiplier": 0.25}),
        LayerWeather("fungal_frenzy", "Fungal Frenzy", "Fungal Depths",
                     "Everything is growing. Including the things that shouldn't be.",
                     {"event_chance_multiplier": 2.0, "cave_in_bonus": 0.08}),
    ],
    "Frozen Core": [
        LayerWeather("time_dilation", "Time Dilation", "Frozen Core",
                     "Time runs thick here today. Every coin is worth double, but digging is slow.",
                     {"jc_multiplier": 1.0, "advance_bonus": -1}),
        LayerWeather("frozen_stillness", "Frozen Stillness", "Frozen Core",
                     "Absolute zero. Nothing collapses. Nothing happens. Nothing at all.",
                     {"cave_in_bonus": -1.0, "event_chance_multiplier": -1.0}),
        LayerWeather("temporal_storm", "Temporal Storm", "Frozen Core",
                     "Time fractures. Chaos, but the fragments are valuable.",
                     {"cave_in_bonus": 0.15, "jc_multiplier": 0.75, "event_chance_multiplier": 0.50}),
    ],
    "The Hollow": [
        LayerWeather("hollow_breathes", "The Hollow Breathes", "The Hollow",
                     "The Hollow inhales. Everything amplifies. Tread carefully.",
                     {"jc_multiplier": 0.50, "cave_in_bonus": 0.10, "event_chance_multiplier": 0.50}),
        LayerWeather("void_harvest", "Void Harvest", "The Hollow",
                     "The void gives up its treasures. It will want them back.",
                     {"artifact_multiplier": 3.0, "cave_in_bonus": 0.15}),
        LayerWeather("deep_silence", "Deep Silence", "The Hollow",
                     "The Hollow holds its breath. Safer, but it takes the colour from everything.",
                     {"cave_in_bonus": -0.15, "jc_multiplier": -0.50}),
    ],
}

WEATHER_BY_ID: dict[str, LayerWeather] = {
    w.id: w
    for weathers in LAYER_WEATHER_POOL.values()
    for w in weathers
}
