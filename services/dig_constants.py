"""
Constants for the tunnel digging minigame.

Pure data/constants file with no external dependencies.
Organized into sections: layers, pacing, pickaxes, consumables, bosses,
artifacts, events, achievements, naming, ASCII art, prestige, decay,
sabotage/defense, injuries, and tips.
"""

from __future__ import annotations

import random
from dataclasses import dataclass, field
from typing import Any

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
    LayerDef("Frozen Core",   201, 275, 0.45, 2,  5,  1, 2, "\u2744\ufe0f"),      # snowflake
    LayerDef("The Hollow",    276, None, 0.50, 2,  7,  1, 1, "\u26ab"),           # black circle
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

FREE_DIG_COOLDOWN_SECONDS: int = 10_800          # 3 hours
CHEER_COOLDOWN_SECONDS: int = 30                 # short anti-spam, independent of dig

PAID_DIG_COSTS_PER_DAY: list[int] = [3, 6, 12, 24, 48]
PAID_DIG_COST_CAP: int = 48

# First dig guarantees
FIRST_DIG_ADVANCE_MIN: int = 3
FIRST_DIG_ADVANCE_MAX: int = 7
FIRST_DIG_JC_MIN: int = 1
FIRST_DIG_JC_MAX: int = 5
FIRST_DIG_CAVE_IN: bool = False

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
    300: 100,
    400: 150,
}

# Streak rewards: consecutive-day count -> JC bonus
STREAKS: dict[int, int] = {
    3: 2,
    7: 5,
    14: 10,
    30: 15,
}


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


# ---------------------------------------------------------------------------
# Pickaxe Tiers
# ---------------------------------------------------------------------------

@dataclass(frozen=True)
class PickaxeTier:
    """Immutable definition for a pickaxe upgrade tier."""
    name: str
    advance_bonus: int              # extra blocks per dig
    cave_in_reduction: float        # absolute % reduction (0.05 = 5%)
    loot_bonus: int                 # extra JC per dig
    # Requirements
    depth_required: int
    jc_cost: int
    prestige_required: int          # 0 means no prestige gate


_PICKAXE_TIERS_DEF: list[PickaxeTier] = [
    PickaxeTier("Wooden",       0, 0.0,  0, depth_required=0,   jc_cost=0,    prestige_required=0),
    PickaxeTier("Stone",        1, 0.0,  0, depth_required=25,  jc_cost=15,   prestige_required=0),
    PickaxeTier("Iron",         1, 0.05, 0, depth_required=50,  jc_cost=50,   prestige_required=0),
    PickaxeTier("Diamond",      2, 0.05, 2, depth_required=75,  jc_cost=150,  prestige_required=0),
    PickaxeTier("Obsidian",     3, 0.10, 3, depth_required=100, jc_cost=300,  prestige_required=1),
    PickaxeTier("Frostforged",  3, 0.20, 3, depth_required=200, jc_cost=600,  prestige_required=3),
    PickaxeTier("Void-Touched", 4, 0.20, 5, depth_required=275, jc_cost=1200, prestige_required=5),
]

PICKAXE_TIERS: list[dict] = [
    {
        "name": p.name, "advance_bonus": p.advance_bonus,
        "cave_in_reduction": p.cave_in_reduction, "loot_bonus": p.loot_bonus,
        "depth_required": p.depth_required, "jc_cost": p.jc_cost,
        "prestige_required": p.prestige_required,
    }
    for p in _PICKAXE_TIERS_DEF
]


# ---------------------------------------------------------------------------
# Boss-combat Gear
# ---------------------------------------------------------------------------
# Three persistent slots (Weapon, Armor, Boots) modify boss-fight stats.
# Each slot has 7 tiers reusing the existing pickaxe names (Wooden →
# Void-Touched). Shop sells Wooden–Diamond; Obsidian–Void-Touched are
# boss-drop-only. Durability ticks once per boss fight; at zero the
# piece auto-unequips and must be repaired before re-equipping.

from domain.models.dig_gear import GearSlot, GearTierDef  # noqa: E402

GEAR_MAX_DURABILITY: int = 20
GEAR_REPAIR_COST_PCT: float = 0.33
GEAR_BOSS_DROP_RATE: float = 0.07
# Maps boss-boundary depth → tier index of the dropped piece. Boundaries
# missing from this map (25/50/75) drop nothing; players buy low-tier
# shop gear there instead.
GEAR_DROP_DEPTH_TIER_MAP: dict[int, int] = {100: 4, 150: 4, 200: 5, 275: 6}
# One-shot grants the first time a player crosses these depths (slot,
# tier). Implementation reads this lazily in dig advance flow; the user
# only ever sees one of each.
GEAR_MILESTONE_GRANTS: dict[int, tuple[str, int]] = {
    50: ("armor", 1),    # Stone Plate at depth 50
    100: ("boots", 2),   # Iron Boots at depth 100
    200: ("armor", 3),   # Diamond Plate at depth 200
}

# Weapon = pickaxe. Tier-by-tier the dig stats here mirror PICKAXE_TIERS
# above so weapon and pickaxe collapse to the same item; the new boss
# stat columns are layered on top.
WEAPON_TIERS: list[GearTierDef] = [
    GearTierDef("Wooden Pickaxe",       tier=0, slot=GearSlot.WEAPON,
                player_dmg=0, player_hit=0.00,
                advance_bonus=0, cave_in_reduction=0.00, loot_bonus=0,
                shop_price=0,    depth_required=0,   prestige_required=0),
    GearTierDef("Stone Pickaxe",        tier=1, slot=GearSlot.WEAPON,
                player_dmg=0, player_hit=0.01,
                advance_bonus=1, cave_in_reduction=0.00, loot_bonus=0,
                shop_price=15,   depth_required=25,  prestige_required=0),
    GearTierDef("Iron Pickaxe",         tier=2, slot=GearSlot.WEAPON,
                player_dmg=0, player_hit=0.02,
                advance_bonus=1, cave_in_reduction=0.05, loot_bonus=0,
                shop_price=50,   depth_required=50,  prestige_required=0),
    GearTierDef("Diamond Pickaxe",      tier=3, slot=GearSlot.WEAPON,
                player_dmg=1, player_hit=0.03,
                advance_bonus=2, cave_in_reduction=0.05, loot_bonus=2,
                shop_price=150,  depth_required=75,  prestige_required=0),
    GearTierDef("Obsidian Pickaxe",     tier=4, slot=GearSlot.WEAPON,
                player_dmg=1, player_hit=0.04,
                advance_bonus=3, cave_in_reduction=0.10, loot_bonus=3,
                shop_price=300,  depth_required=100, prestige_required=1),
    GearTierDef("Frostforged Pickaxe",  tier=5, slot=GearSlot.WEAPON,
                player_dmg=2, player_hit=0.05,
                advance_bonus=3, cave_in_reduction=0.20, loot_bonus=3,
                shop_price=600,  depth_required=200, prestige_required=3),
    GearTierDef("Void-Touched Pickaxe", tier=6, slot=GearSlot.WEAPON,
                player_dmg=2, player_hit=0.07,
                advance_bonus=4, cave_in_reduction=0.20, loot_bonus=5,
                shop_price=1200, depth_required=275, prestige_required=5),
]

# Armor adds player_hp (so the piece "soaks" boss hits). Base player_hp
# is 2–5 depending on risk_tier, so a full Void Plate roughly doubles
# survivability — the dominant survivability lever.
ARMOR_TIERS: list[GearTierDef] = [
    GearTierDef("Wooden Plate",       tier=0, slot=GearSlot.ARMOR,
                player_hp_bonus=0, shop_price=0),
    GearTierDef("Stone Plate",        tier=1, slot=GearSlot.ARMOR,
                player_hp_bonus=0, shop_price=20,   depth_required=25),
    GearTierDef("Iron Plate",         tier=2, slot=GearSlot.ARMOR,
                player_hp_bonus=1, shop_price=60,   depth_required=50),
    GearTierDef("Diamond Plate",      tier=3, slot=GearSlot.ARMOR,
                player_hp_bonus=2, shop_price=180,  depth_required=75),
    GearTierDef("Obsidian Plate",     tier=4, slot=GearSlot.ARMOR,
                player_hp_bonus=3, shop_price=350,  depth_required=100, prestige_required=1),
    GearTierDef("Frostforged Plate",  tier=5, slot=GearSlot.ARMOR,
                player_hp_bonus=3, shop_price=700,  depth_required=200, prestige_required=3),
    GearTierDef("Void-Touched Plate", tier=6, slot=GearSlot.ARMOR,
                player_hp_bonus=4, shop_price=1400, depth_required=275, prestige_required=5),
]

# Boots reduce boss_hit (the chance an incoming attack lands). Stays
# bounded to a sane range so even Void boots don't make the player
# untouchable on their own.
BOOTS_TIERS: list[GearTierDef] = [
    GearTierDef("Wooden Boots",       tier=0, slot=GearSlot.BOOTS,
                boss_hit_reduction=0.00, shop_price=0),
    GearTierDef("Stone Boots",        tier=1, slot=GearSlot.BOOTS,
                boss_hit_reduction=0.02, shop_price=25,   depth_required=25),
    GearTierDef("Iron Boots",         tier=2, slot=GearSlot.BOOTS,
                boss_hit_reduction=0.04, shop_price=70,   depth_required=50),
    GearTierDef("Diamond Boots",      tier=3, slot=GearSlot.BOOTS,
                boss_hit_reduction=0.06, shop_price=200,  depth_required=75),
    GearTierDef("Obsidian Boots",     tier=4, slot=GearSlot.BOOTS,
                boss_hit_reduction=0.08, shop_price=400,  depth_required=100, prestige_required=1),
    GearTierDef("Frostforged Boots",  tier=5, slot=GearSlot.BOOTS,
                boss_hit_reduction=0.10, shop_price=800,  depth_required=200, prestige_required=3),
    GearTierDef("Void-Touched Boots", tier=6, slot=GearSlot.BOOTS,
                boss_hit_reduction=0.13, shop_price=1500, depth_required=275, prestige_required=5),
]

GEAR_TIER_TABLES: dict[GearSlot, list[GearTierDef]] = {
    GearSlot.WEAPON: WEAPON_TIERS,
    GearSlot.ARMOR:  ARMOR_TIERS,
    GearSlot.BOOTS:  BOOTS_TIERS,
}


# ---------------------------------------------------------------------------
# Consumable Items
# ---------------------------------------------------------------------------

@dataclass(frozen=True)
class Consumable:
    """Immutable definition for a purchasable consumable item."""
    id: str
    name: str
    cost: int
    description: str
    # Mechanical parameters stored as a dict for flexible use
    params: dict[str, int | float]


CONSUMABLES: dict[str, Consumable] = {
    "dynamite": Consumable(
        id="dynamite",
        name="Dynamite",
        cost=5,
        description="+5 bonus blocks, 10% mini cave-in (-2 blocks)",
        params={"bonus_blocks": 5, "mini_cave_in_pct": 0.10, "mini_cave_in_loss": 2},
    ),
    "hard_hat": Consumable(
        id="hard_hat",
        name="Hard Hat",
        cost=8,
        description="Prevent cave-in for next 3 digs",
        params={"uses": 3},
    ),
    "lantern": Consumable(
        id="lantern",
        name="Lantern",
        cost=4,
        description="Full scan + -50% cave-in next dig",
        params={"cave_in_reduction": 0.50, "scan": 1},
    ),
    "reinforcement": Consumable(
        id="reinforcement",
        name="Reinforcement",
        cost=6,
        description="Prevent decay 48h + 25% sabotage reduction",
        params={"decay_prevent_hours": 48, "sabotage_reduction": 0.25},
    ),
    "torch": Consumable(
        id="torch",
        name="Torch",
        cost=6,
        description="+50 luminosity. Light the way.",
        params={"luminosity_restore": 50},
    ),
    "grappling_hook": Consumable(
        id="grappling_hook",
        name="Grappling Hook",
        cost=10,
        description="Prevents block loss on next cave-in",
        params={"prevent_cave_in_loss": 1},
    ),
    "sonar_pulse": Consumable(
        id="sonar_pulse",
        name="Sonar Pulse",
        cost=8,
        description="Reveals next event before it triggers",
        params={"event_preview": 1},
    ),
    "depth_charge": Consumable(
        id="depth_charge",
        name="Depth Charge",
        cost=15,
        description="+8 advance but triggers mini cave-in (-3). Net +5.",
        params={"bonus_blocks": 8, "mini_cave_in_loss": 3},
    ),
    "void_bait": Consumable(
        id="void_bait",
        name="Void Bait",
        cost=20,
        description="Doubles event chance for 3 digs",
        params={"event_multiplier": 2.0, "duration_digs": 3},
    ),
}

HARD_HAT_USES: int = 3


# ---------------------------------------------------------------------------
# Boss Definitions
# ---------------------------------------------------------------------------

@dataclass(frozen=True)
class BossDef:
    """Immutable definition for a layer boss."""
    depth: int
    name: str
    title: str
    ascii_art: str
    dialogue: list[str]             # 5 stages: threatening -> absurd
    boss_id: str = ""               # stable unique identifier (e.g. "grothak", "pudge")
    mechanic_pool: tuple[str, ...] = ()  # keys into MECHANIC_REGISTRY; one rolled per fight
    stinger_id: str = ""            # key into STINGER_REGISTRY; fires on player loss


BOSSES: dict[int, BossDef] = {
    25: BossDef(
        depth=25,
        name="Grothak the Unbreakable",
        title="Guardian of the Shallows",
        ascii_art=(
            "########.....########\n"
            "#.......|   |.......#\n"
            "#.......,-^-,.......#\n"
            "#....../ o o \\.......#\n"
            "#.....| (___) |.....#\n"
            "#......\\ === /.......#\n"
            "#.......'---'.......#\n"
            "#.........@.........#\n"
            "#####################"
        ),
        dialogue=[
            "You dare enter MY dirt?! I'll crush you like the worm you are!",
            "Again?! My back already hurts from the last fight... ugh.",
            "Look, can we reschedule? My chiropractor says I need rest.",
            "YOU AGAIN?! I literally just sat down!",
            "Fine. Hit me. I can't feel anything below the waist anyway.",
        ],
        boss_id="grothak",
        mechanic_pool=("grothak_earthquake", "grothak_crumble_wall"),
        stinger_id="grothak_crumble",
    ),
    50: BossDef(
        depth=50,
        name="Crystalia the Refracted",
        title="Mistress of Perfect Angles",
        ascii_art=(
            "   /\\_/\\\n"
            "  ( o.o )\n"
            " />diamonds<\\\n"
            " \\_______/\n"
            "   |||||"
        ),
        dialogue=[
            "Your asymmetrical face offends me. Prepare to be geometrically corrected!",
            "You're back and you STILL haven't fixed that crooked nose?!",
            "Please just stand three degrees to the left... no, MY left. UGH.",
            "YOU AGAIN?! Do you know how long it took to re-align these crystals?!",
            "I give up. Nothing is symmetrical anymore. Not even my will to fight.",
        ],
        boss_id="crystalia",
        mechanic_pool=("crystalia_prism", "crystalia_shatter"),
        stinger_id="crystalia_shard",
    ),
    75: BossDef(
        depth=75,
        name="Magmus Rex",
        title="Sovereign of the Molten Depths",
        ascii_art=(
            "  ~*~*~\n"
            " {(O  O)}\n"
            " {  <>  }\n"
            " {\\_^^_/}\n"
            "  ~~~~~"
        ),
        dialogue=[
            "BURN, MORTAL! I am the flame that— actually, can we do this later?",
            "Ugh, not you again. Do you know how hard it is to get PTO down here?",
            "I put in for vacation THREE CENTURIES AGO. HR hasn't responded.",
            "YOU AGAIN?! I was literally packing my bags for Bali!",
            "I'm just gonna lie here. Lava is basically a hot tub, right? ...right?",
        ],
        boss_id="magmus_rex",
        mechanic_pool=("magmus_eruption", "magmus_meteor"),
        stinger_id="magmus_burn",
    ),
    100: BossDef(
        depth=100,
        name="The Void Warden",
        title="Keeper of the Final Dark",
        ascii_art=(
            "  .o0O0o.\n"
            " (  ???  )\n"
            " |  _V_  |\n"
            " ( '---' )\n"
            "  `o0O0o'"
        ),
        dialogue=[
            "You gaze into the abyss, and the abyss... wonders why it bothers.",
            "Oh. You again. Do I even exist if no one digs here?",
            "I've been guarding nothing for eons. What's the point, really?",
            "YOU AGAIN?! Is this all there is? Darkness and... diggers?",
            "You know what? Take the void. I'm going to go find myself.",
        ],
        boss_id="void_warden",
        mechanic_pool=("voidwarden_collapse", "voidwarden_silence"),
        stinger_id="void_collapse",
    ),
    150: BossDef(
        depth=150,
        name="Sporeling Sovereign",
        title="The One Who Grows",
        ascii_art=(
            "  .oO@Oo.\n"
            " /  we   \\\n"
            "( are one )\n"
            " \\  all  /\n"
            "  'oO@Oo'"
        ),
        dialogue=[
            "We are the soil and the soil is us. You trespass on ourselves.",
            "You return. We have grown since last you came. We remember your footsteps.",
            "We considered offering you tea. Then we remembered we are mushrooms.",
            "YOU AGAIN. We were in the middle of photosynthesis. ...Wait. We don't do that.",
            "Fine. We yield. Would you like a mushroom recipe? We have thousands.",
        ],
        boss_id="sporeling_sovereign",
        mechanic_pool=("sporeling_cloud", "sporeling_roots"),
        stinger_id="sporeling_rot",
    ),
    200: BossDef(
        depth=200,
        name="Chronofrost",
        title="The Still Moment",
        ascii_art=(
            "  *  . *  .\n"
            " / frozen  \\\n"
            "| t i m e  |\n"
            " \\ stands /\n"
            "  *  . *  ."
        ),
        dialogue=[
            "You arrive exactly when I expected. I've been waiting since before you were born.",
            "We've done this before. You just don't remember yet. I envy that.",
            "I could tell you how this ends but you wouldn't believe me. I barely do.",
            "YOU AGAIN. Or is it still? Time is a suggestion down here.",
            "Go. I've seen every possible outcome and in most of them you win anyway.",
        ],
        boss_id="chronofrost",
        mechanic_pool=("chronofrost_still", "chronofrost_rewind"),
        stinger_id="chronofrost_stillness",
    ),
    275: BossDef(
        depth=275,
        name="The Nameless Depth",
        title="[REDACTED]",
        ascii_art=(
            "  . . . . .\n"
            " .         .\n"
            " .  ?   ?  .\n"
            " .    _    .\n"
            "  . . . . ."
        ),
        dialogue=[
            "I was you, once. Before the digging consumed me.",
            "Your tunnel. I know its name. I know all the names.",
            "You dig to find something. I dug to forget something. We are the same.",
            "YOU AGAIN. Or am I you again? The distinction stopped mattering at depth 250.",
            "Take the hollow. It was always yours. I was just keeping it warm.",
        ],
        boss_id="nameless_depth",
        mechanic_pool=("nameless_whisper", "nameless_silence"),
        stinger_id="nameless_erase",
    ),
}


# ---------------------------------------------------------------------------
# New Dota-themed bosses (2 per tier, sharing each tier with 1 grandfathered
# boss). A tunnel rolls one boss per tier when it first crosses the milestone
# and locks that pick for the run (see boss_progress JSON shape on the
# tunnels table).
# ---------------------------------------------------------------------------

_DOTA_BOSSES: dict[str, BossDef] = {
    "pudge": BossDef(
        depth=25,
        boss_id="pudge",
        name="The Butcher",
        title="Stitched-Together Hooker",
        ascii_art=(
            "   _____\n"
            "  /     \\\n"
            " | X o X |~~===>\n"
            "  \\_____/   (hook)\n"
            "   /|||\\\n"
        ),
        dialogue=[
            "FRESH MEAT!",
            "Oh, you came back. The last one tasted like regret.",
            "I skipped lunch for this. You'd better be worth it.",
            "YOU AGAIN?! My hook is blunt from you alone.",
            "Fine. Walk past. I'm too tired to even taunt.",
        ],
        mechanic_pool=("pudge_hook", "pudge_rot"),
        stinger_id="pudge_drag",
    ),
    "ogre_magi": BossDef(
        depth=25,
        boss_id="ogre_magi",
        name="The Twin-Skulled",
        title="Two Heads, Zero Plans",
        ascii_art=(
            "   (o)(o)\n"
            "  /      \\\n"
            " |  urrrk |\n"
            "  \\      /\n"
            "   \\____/\n"
            "    ||||\n"
        ),
        dialogue=[
            "One of us casts! The other forgets!",
            "We saw you yesterday. We forgot you today. Hi again!",
            "Left head wants to fight. Right head wants nachos.",
            "YOU AGAIN! ...who? Oh right. YOU.",
            "Both heads tired. Both heads say: yield.",
        ],
        mechanic_pool=("ogre_multicast", "ogre_fireblast"),
        stinger_id="ogre_blast",
    ),
    "crystal_maiden": BossDef(
        depth=50,
        boss_id="crystal_maiden",
        name="The Frostbinder",
        title="Cold in the Best Way",
        ascii_art=(
            "    ,-'-.\n"
            "   ( *.* )\n"
            "  /~|\"|~\\\n"
            " / brrrr \\\n"
            "   | | |\n"
        ),
        dialogue=[
            "Stay a while. You'll be cold forever.",
            "You came back? My mana hasn't even regenerated.",
            "Okay, listen. I just did my hair. Please die quickly.",
            "YOU AGAIN?! I was literally mid-ult.",
            "Okay fine, I'll come quietly. But stop ganking me.",
        ],
        mechanic_pool=("cm_frostbite", "cm_freezing_field"),
        stinger_id="cm_freeze",
    ),
    "tusk": BossDef(
        depth=50,
        boss_id="tusk",
        name="The Ice Warlord",
        title="The Walrus With A Plan",
        ascii_art=(
            "   .---.\n"
            "  ( o o )\n"
            " / |===| \\\n"
            " \\_______/\n"
            "  ~~~snow~~~\n"
        ),
        dialogue=[
            "You ever been yeeted by a walrus? You're about to.",
            "Snowball's out. Good luck.",
            "I will kick you so hard you forget your own depth.",
            "YOU AGAIN?! I'm out of snow. Give me a minute.",
            "Fine. Go. Tell your friends a walrus sent you.",
        ],
        mechanic_pool=("tusk_snowball", "tusk_walrus_punch"),
        stinger_id="tusk_kick",
    ),
    "lina": BossDef(
        depth=75,
        boss_id="lina",
        name="The Scorchwitch",
        title="She Who Brings the Heat",
        ascii_art=(
            "   ~*~*~\n"
            "   (` )\n"
            "   /\\_/\\\n"
            "  ( >_< )\n"
            "   /   \\\n"
        ),
        dialogue=[
            "My fingers are warming up. Say goodbye.",
            "Oh look, you. Again. I'll try to kill you differently this time.",
            "I'm low-key tired. Let's just one-shot this.",
            "YOU AGAIN?! My mana bar has trust issues.",
            "Ugh. Fine. Take the depth. My hair's frizzed anyway.",
        ],
        mechanic_pool=("lina_laguna", "lina_dragon_slave"),
        stinger_id="lina_scorch",
    ),
    "doom": BossDef(
        depth=75,
        boss_id="doom",
        name="The Deathbringer",
        title="Lord of the Avernus",
        ascii_art=(
            "   .---.\n"
            "  /X X X\\\n"
            " |  ___  |\n"
            "  \\_____/\n"
            "   ||||| \n"
        ),
        dialogue=[
            "Silence. The end approaches.",
            "You return. I am unimpressed.",
            "Every one of your digs extends my work week.",
            "YOU AGAIN. Even the damned get tired.",
            "Go. I need a holiday from you.",
        ],
        mechanic_pool=("doom_mark", "doom_scorched_earth"),
        stinger_id="doom_brand",
    ),
    "spectre": BossDef(
        depth=100,
        boss_id="spectre",
        name="The Dread Shade",
        title="The Dagger in the Dark",
        ascii_art=(
            "   _____\n"
            "  /     \\\n"
            " |  o o  |\n"
            "  \\  V  /\n"
            "   \\___/\n"
            "    ~||~\n"
        ),
        dialogue=[
            "I have already struck you. You just haven't noticed.",
            "You keep returning. I never actually leave.",
            "We are the same wound.",
            "YOU AGAIN. I am, as ever.",
            "Go. My work is never done anyway.",
        ],
        mechanic_pool=("spectre_haunt", "spectre_dagger"),
        stinger_id="spectre_haunting",
    ),
    "void_spirit": BossDef(
        depth=100,
        boss_id="void_spirit",
        name="The Astral Echo",
        title="Dimensional Tourist",
        ascii_art=(
            "    .\" \".\n"
            "   ( *.* )\n"
            "    \\=|=/\n"
            "  ~~/   \\~~\n"
            "    /   \\\n"
        ),
        dialogue=[
            "I stepped sideways through space to kill you. Worth it.",
            "Back from a different dimension. You still here?",
            "I know seven of your tunnels. Yours is the worst one.",
            "YOU AGAIN?! I am literally everywhere else.",
            "Fine. I'll take this dimension off.",
        ],
        mechanic_pool=("void_spirit_step", "void_spirit_aether"),
        stinger_id="void_spirit_exile",
    ),
    "treant_protector": BossDef(
        depth=150,
        boss_id="treant_protector",
        name="The Elder Grove",
        title="Old Growth, Old Grudges",
        ascii_art=(
            "      /\\\n"
            "     /  \\\n"
            "    /\\/\\ \\\n"
            "    /   \\ \\\n"
            "   / (0) \\\n"
            "    | | |\n"
        ),
        dialogue=[
            "You dig. I grow. One of us is patient.",
            "Again. Trees have long memories.",
            "Every time you return I have more rings.",
            "YOU AGAIN. I am older than your tunnel.",
            "Go. Even trees can grow tired.",
        ],
        mechanic_pool=("treant_overgrowth", "treant_leech_seed"),
        stinger_id="treant_entangle",
    ),
    "broodmother": BossDef(
        depth=150,
        boss_id="broodmother",
        name="The Nestmother",
        title="Nine Hundred Hungry Children",
        ascii_art=(
            "    /\\ /\\\n"
            "   (oOOo)\n"
            "  / '--' \\\n"
            " ~~~webs~~~\n"
            "    \\\\||//\n"
        ),
        dialogue=[
            "My children are hungry. Please don't run.",
            "You keep bringing yourself back. Thoughtful of you.",
            "I still haven't named half of them. Want to help?",
            "YOU AGAIN?! I was in the middle of spinning.",
            "Fine. Go. Leave us to our weaving.",
        ],
        mechanic_pool=("broodmother_spawn", "broodmother_web"),
        stinger_id="broodmother_webbing",
    ),
    "faceless_void": BossDef(
        depth=200,
        boss_id="faceless_void",
        name="The Timeless One",
        title="There Is No Timing Like His Timing",
        ascii_art=(
            "   _______\n"
            "  /       \\\n"
            " |   _ _   |\n"
            "  \\  /-\\  /\n"
            "   \\_____/\n"
            "    time\n"
        ),
        dialogue=[
            "I saw this coming. Literally.",
            "You again. I was expecting you five seconds ago.",
            "I'll freeze the moment and walk away. Have fun.",
            "YOU AGAIN. My cooldown is up, regrettably.",
            "Go. You were going to win this one anyway.",
        ],
        mechanic_pool=("faceless_void_chrono", "faceless_void_backtrack"),
        stinger_id="void_chrono",
    ),
    "weaver": BossDef(
        depth=200,
        boss_id="weaver",
        name="The Skitterwing",
        title="The One Who Unpicks",
        ascii_art=(
            "   .-.\n"
            "  ( ^ )\n"
            "  /|X|\\\n"
            " / | | \\\n"
            "  ~===~\n"
        ),
        dialogue=[
            "I will pull one thread. You will unravel.",
            "Oh. You. Again. I hadn't even finished stitching.",
            "Time-lapse away now and we both save energy.",
            "YOU AGAIN?! I reset my own timeline to rest.",
            "Take the depth. I'll weave it back later.",
        ],
        mechanic_pool=("weaver_timelapse", "weaver_shukuchi"),
        stinger_id="weaver_unmake",
    ),
    "oracle": BossDef(
        depth=275,
        boss_id="oracle",
        name="The Blindfolded Seer",
        title="Seer of Bad Bets",
        ascii_art=(
            "   .-\"\"\"-.\n"
            "  / ? ? ? \\\n"
            " | o . o |\n"
            "  \\_=_=_/\n"
            "    |||\n"
        ),
        dialogue=[
            "I have already decided which one of us wins.",
            "You? Again? I foresaw it. And still find it tedious.",
            "Let's flip for it. Pick a side. Both sides lose.",
            "YOU AGAIN. I predicted this too.",
            "Go. The coin is tired.",
        ],
        mechanic_pool=("oracle_fortune", "oracle_false_promise"),
        stinger_id="oracle_fate",
    ),
    "terrorblade": BossDef(
        depth=275,
        boss_id="terrorblade",
        name="The Sundered Prince",
        title="Betrayer and Sunderer",
        ascii_art=(
            "     /\\_/\\\n"
            "    ( >_< )\n"
            "   _/|'-'|\\_\n"
            "  |__|---|__|\n"
            "     /|v|\\\n"
        ),
        dialogue=[
            "I will trade lives with you. You will not like yours.",
            "You returned. Willingly. I admire the theater.",
            "One more trade. Then we talk severance.",
            "YOU AGAIN?! My mirror image is tired.",
            "Take the hollow. Cleave yourself out of it.",
        ],
        mechanic_pool=("terrorblade_sunder", "terrorblade_metamorphosis"),
        stinger_id="terrorblade_sundering",
    ),
}


# BOSSES_BY_TIER: new canonical per-tier grouping. The first entry per tier
# is the grandfathered fantasy boss (preserved from ``BOSSES``); the remaining
# entries are the Dota-themed additions. All gameplay code that selects
# which boss a tunnel faces should go through this table via
# ``get_boss_pool_for_tier`` or ``get_boss_by_id``.
BOSSES_BY_TIER: dict[int, list[BossDef]] = {
    25:  [BOSSES[25],  _DOTA_BOSSES["pudge"],            _DOTA_BOSSES["ogre_magi"]],
    50:  [BOSSES[50],  _DOTA_BOSSES["crystal_maiden"],   _DOTA_BOSSES["tusk"]],
    75:  [BOSSES[75],  _DOTA_BOSSES["lina"],             _DOTA_BOSSES["doom"]],
    100: [BOSSES[100], _DOTA_BOSSES["spectre"],          _DOTA_BOSSES["void_spirit"]],
    150: [BOSSES[150], _DOTA_BOSSES["treant_protector"], _DOTA_BOSSES["broodmother"]],
    200: [BOSSES[200], _DOTA_BOSSES["faceless_void"],    _DOTA_BOSSES["weaver"]],
    275: [BOSSES[275], _DOTA_BOSSES["oracle"],           _DOTA_BOSSES["terrorblade"]],
}


# BOSSES_BY_ID: flat lookup, boss_id -> BossDef.
BOSSES_BY_ID: dict[str, BossDef] = {
    boss.boss_id: boss
    for tier_list in BOSSES_BY_TIER.values()
    for boss in tier_list
}


def get_boss_pool_for_tier(tier: int) -> list[BossDef]:
    """Return the list of candidate BossDefs for the given tier depth."""
    return BOSSES_BY_TIER.get(tier, [])


def get_boss_by_id(boss_id: str) -> BossDef | None:
    """Return the BossDef with the given boss_id, or None."""
    return BOSSES_BY_ID.get(boss_id)


# Boss fight mechanics ────────────────────────────────────────────
# Bosses are resolved as a multi-round HP duel: player and boss alternate
# turns (player first), each rolling their tier's hit chance and dealing
# damage on a hit. Whoever reaches 0 HP first loses. Player loss = forfeit
# wager + cave-in to the previous milestone.
#
# Per-tier stats (player_hp, boss_hp, player_hit, player_dmg, boss_hit, boss_dmg).
# Reckless is tuned for WAGERED play. Free-fight reckless clamps to the
# PLAYER_HIT_FLOOR (0.10 * BOSS_FREE_FIGHT_ACCURACY_MOD = 0.06, floored to
# 0.05) which is intentionally near-impossible: high-stakes wager only.
BOSS_DUEL_STATS: dict[str, dict[str, float]] = {
    "cautious": {"player_hp": 5, "boss_hp": 4, "player_hit": 0.60, "player_dmg": 1, "boss_hit": 0.30, "boss_dmg": 1},
    "bold":     {"player_hp": 3, "boss_hp": 5, "player_hit": 0.35, "player_dmg": 2, "boss_hit": 0.45, "boss_dmg": 1},
    "reckless": {"player_hp": 2, "boss_hp": 6, "player_hit": 0.10, "player_dmg": 3, "boss_hit": 0.60, "boss_dmg": 1},
}

# Boss difficulty curve — hand-tuned lookup tables. Replaces the prior
# linear-formula scaling. The tables are the single source of truth: each
# cell is added to the boss base+archetype stats, and a Monte-Carlo
# simulation confirmed the resulting curve. Tune by editing entries.
BOSS_TIER_BONUS: dict[int, dict[str, float]] = {
    # boundary depth: {boss_hp_add, boss_hit_add, boss_dmg_add, player_hit_pen}
    25:  {"hp": 0,  "hit": 0.00, "dmg": 0, "pen": 0.00},
    50:  {"hp": 1,  "hit": 0.00, "dmg": 0, "pen": 0.01},
    75:  {"hp": 2,  "hit": 0.01, "dmg": 0, "pen": 0.02},
    100: {"hp": 3,  "hit": 0.03, "dmg": 0, "pen": 0.04},
    150: {"hp": 5,  "hit": 0.04, "dmg": 0, "pen": 0.05},
    200: {"hp": 5,  "hit": 0.05, "dmg": 0, "pen": 0.06},
    275: {"hp": 6,  "hit": 0.06, "dmg": 0, "pen": 0.06},
    300: {"hp": 9,  "hit": 0.10, "dmg": 0, "pen": 0.06},   # pinnacle: HP grind, no dmg cliff
}
BOSS_PRESTIGE_BONUS: dict[int, dict[str, float]] = {
    # prestige: {boss_hp_add, boss_hit_add, boss_dmg_add, player_hit_pen}
    # P1/P3/P5 carry extra cushion to offset the gear-unlock power spike.
    0: {"hp": 0,  "hit": 0.00, "dmg": 0, "pen": 0.000},
    1: {"hp": 9,  "hit": 0.08, "dmg": 0, "pen": 0.030},   # Obsidian unlock cushion
    2: {"hp": 9,  "hit": 0.09, "dmg": 0, "pen": 0.050},
    3: {"hp": 12, "hit": 0.12, "dmg": 0, "pen": 0.080},   # Frost unlock cushion
    4: {"hp": 14, "hit": 0.14, "dmg": 0, "pen": 0.110},
    5: {"hp": 24, "hit": 0.20, "dmg": 0, "pen": 0.140},   # Void unlock cushion (big)
    6: {"hp": 26, "hit": 0.23, "dmg": 0, "pen": 0.165},
    7: {"hp": 27, "hit": 0.25, "dmg": 1, "pen": 0.190},   # purgatory: only +dmg row
}
PLAYER_HIT_FLOOR: float = 0.05                     # hard floor so Reckless remains playable
PLAYER_HIT_CEILING: float = 0.90                   # hard ceiling — luminosity already eats hit chance, so leave a wider cap
BOSS_FREE_FIGHT_ACCURACY_MOD: float = 0.6          # multiplied into player_hit when wager == 0
BOSS_ROUND_CAP: int = 20                           # safety valve against infinite loops
WIN_CHANCE_CAP: float = 0.95                       # ceiling on displayed/computed win probability
WIN_CHANCE_FLOOR: float = 0.05                     # floor on displayed/computed win probability ("miracle" chance)

# Boss archetypes — applied on top of risk-tier base stats so each boss
# in a tier feels distinct (e.g. Pudge tanks, Lina glass-cannons).
# hp_mult applies to base boss_hp; hit/dmg are additive offsets.
BOSS_ARCHETYPES: dict[str, dict[str, float]] = {
    "tank":         {"hp_mult": 1.5, "hit_offset": -0.03, "dmg_offset": 0},
    "bruiser":      {"hp_mult": 1.0, "hit_offset": 0.00,  "dmg_offset": 0},
    "glass_cannon": {"hp_mult": 0.7, "hit_offset": 0.05,  "dmg_offset": 1},
    "slippery":     {"hp_mult": 0.8, "hit_offset": 0.10,  "dmg_offset": 0},
}

# Per-boss archetype assignment (heuristic by Dota persona).
BOSS_ARCHETYPE_BY_ID: dict[str, str] = {
    # Tier 25
    "grothak":             "bruiser",
    "pudge":               "tank",
    "ogre_magi":           "glass_cannon",
    # Tier 50
    "crystalia":           "bruiser",
    "crystal_maiden":      "glass_cannon",
    "tusk":                "tank",
    # Tier 75
    "magmus_rex":          "tank",
    "lina":                "glass_cannon",
    "doom":                "bruiser",
    # Tier 100
    "void_warden":         "slippery",
    "spectre":             "slippery",
    "void_spirit":         "slippery",
    # Tier 150
    "sporeling_sovereign": "tank",
    "treant_protector":    "tank",
    "broodmother":         "glass_cannon",
    # Tier 200
    "chronofrost":         "slippery",
    "faceless_void":       "slippery",
    "weaver":              "slippery",
    # Tier 275
    "nameless_depth":      "tank",
    "oracle":              "glass_cannon",
    "terrorblade":         "glass_cannon",
}

# Payouts: depth -> (cautious_multiplier, bold_multiplier, reckless_multiplier).
# Flatter and harder than the pre-nerf table; the old exponential growth at
# top-end depths was the main jopacoin inflation source.
BOSS_PAYOUTS: dict[int, tuple[float, float, float]] = {
    25:  (1.2, 1.9, 3.4),
    50:  (1.4, 2.6, 4.6),
    75:  (1.7, 3.4, 5.8),
    100: (1.9, 3.8, 6.7),
    150: (2.3, 4.6, 7.8),
    200: (2.6, 5.3, 9.0),
    275: (2.9, 6.2, 9.6),
}


# ---------------------------------------------------------------------------
# Artifacts
# ---------------------------------------------------------------------------

@dataclass(frozen=True)
class ArtifactDef:
    """Immutable definition for a discoverable artifact."""
    id: str
    name: str
    layer: str                      # layer name (e.g. "Dirt", "Stone")
    rarity: str                     # Common | Uncommon | Rare | Legendary
    lore_text: str
    is_relic: bool
    effect: str | None              # description of mechanical effect, or None


RARITY_DROP_RATES: dict[str, float] = {
    "Common": 0.05,
    "Uncommon": 0.02,
    "Rare": 0.005,
    "Legendary": 0.001,
}

# Functional Relics (6) ─────────────────────────────────────────
RELICS: list[ArtifactDef] = [
    ArtifactDef(
        id="mole_claws",
        name="Mole Claws",
        layer="Dirt",
        rarity="Rare",
        lore_text="Fashioned from the claws of the Great Undermole, these gloves let you tear through earth like butter.",
        is_relic=True,
        effect="+1 advance permanently",
    ),
    ArtifactDef(
        id="crystal_compass",
        name="Crystal Compass",
        layer="Crystal",
        rarity="Rare",
        lore_text="A shard of living crystal that hums near danger. It always points away from collapse.",
        is_relic=True,
        effect="-3% cave-in permanently",
    ),
    ArtifactDef(
        id="magma_heart",
        name="Magma Heart",
        layer="Magma",
        rarity="Rare",
        lore_text="Still beating after a thousand years in the lava, this heart radiates warmth and fortune.",
        is_relic=True,
        effect="+1 JC loot permanently",
    ),
    ArtifactDef(
        id="obsidian_shield",
        name="Obsidian Shield",
        layer="Magma",
        rarity="Rare",
        lore_text="Forged in volcanic fury, this shield absorbs ill intent from rival diggers.",
        is_relic=True,
        effect="-15% sabotage damage permanently",
    ),
    ArtifactDef(
        id="root_network",
        name="Root Network",
        layer="Stone",
        rarity="Rare",
        lore_text="Ancient roots lace through the stone, binding your tunnel walls against the passage of time.",
        is_relic=True,
        effect="-25% decay rate permanently",
    ),
    ArtifactDef(
        id="echo_stone",
        name="Echo Stone",
        layer="Crystal",
        rarity="Legendary",
        lore_text="This stone whispers the locations of hidden things. Collectors would kill for it—some have.",
        is_relic=True,
        effect="+10% artifact find chance permanently",
    ),
    ArtifactDef(
        id="spore_cloak",
        name="Spore Cloak",
        layer="Fungal Depths",
        rarity="Rare",
        lore_text="Woven from living mycelium, this cloak feeds on darkness and gives back light.",
        is_relic=True,
        effect="-50% luminosity drain permanently",
    ),
    ArtifactDef(
        id="frozen_clock",
        name="Frozen Clock",
        layer="Frozen Core",
        rarity="Rare",
        lore_text="The hands haven't moved in millennia. Time itself seems embarrassed by this.",
        is_relic=True,
        effect="Decay halved permanently",
    ),
    ArtifactDef(
        id="hollow_eye",
        name="Hollow Eye",
        layer="The Hollow",
        rarity="Legendary",
        lore_text="It sees everything. Every path, every choice, every consequence. It blinks when you're not looking.",
        is_relic=True,
        effect="Complex events reveal all paths",
    ),
    ArtifactDef(
        id="mycelium_link",
        name="Mycelium Link",
        layer="Fungal Depths",
        rarity="Rare",
        lore_text="A living thread connecting you to the fungal network. When you help someone, the network amplifies it.",
        is_relic=True,
        effect="+5% help bonus when helping others",
    ),
]

# Collectible (non-relic) Artifacts (14) ────────────────────────
COLLECTIBLE_ARTIFACTS: list[ArtifactDef] = [
    ArtifactDef(
        id="ancient_shovel",
        name="Ancient Shovel",
        layer="Dirt",
        rarity="Common",
        lore_text="A wooden shovel from the First Diggers. The handle is worn smooth by countless hands.",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="petrified_worm",
        name="Petrified Worm",
        layer="Dirt",
        rarity="Common",
        lore_text="A worm the size of your forearm, frozen in stone mid-wriggle. Unsettling.",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="rusty_coin",
        name="Rusty Coin",
        layer="Dirt",
        rarity="Common",
        lore_text="An old Jopacoin, so corroded you can barely make out the grinning face on it.",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="fossil_imprint",
        name="Fossil Imprint",
        layer="Stone",
        rarity="Common",
        lore_text="The impression of a creature that hasn't existed for millennia. It looks... angry.",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="stone_tablet",
        name="Stone Tablet",
        layer="Stone",
        rarity="Uncommon",
        lore_text="Covered in runes that roughly translate to: 'Kilroy was here.'",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="geode_heart",
        name="Geode Heart",
        layer="Stone",
        rarity="Uncommon",
        lore_text="Crack it open and amethyst crystals sparkle inside. Too pretty to sell.",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="singing_shard",
        name="Singing Shard",
        layer="Crystal",
        rarity="Common",
        lore_text="This crystal fragment emits a faint melody when held. The tune is oddly catchy.",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="prismatic_lens",
        name="Prismatic Lens",
        layer="Crystal",
        rarity="Uncommon",
        lore_text="Light bends impossibly through this lens, revealing colors that shouldn't exist.",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="frozen_flame",
        name="Frozen Flame",
        layer="Crystal",
        rarity="Rare",
        lore_text="A flame trapped in crystal, still flickering after centuries. It's warm to the touch.",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="lava_pearl",
        name="Lava Pearl",
        layer="Magma",
        rarity="Uncommon",
        lore_text="Formed over millennia in a magma pocket. It glows with inner heat and smugness.",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="charred_diary",
        name="Charred Diary",
        layer="Magma",
        rarity="Common",
        lore_text="Most pages are ash, but one reads: 'Day 412. Still hot. Still digging. Send help.'",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="void_fragment",
        name="Void Fragment",
        layer="Abyss",
        rarity="Uncommon",
        lore_text="A shard of absolute nothing. Looking at it too long makes you question your life choices.",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="abyssal_eye",
        name="Abyssal Eye",
        layer="Abyss",
        rarity="Rare",
        lore_text="It blinks. You're sure it blinks. The Void Warden says it's 'decorative.'",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="entropy_marble",
        name="Entropy Marble",
        layer="Abyss",
        rarity="Legendary",
        lore_text="Contains a miniature universe in its final moments. Beautiful and deeply unsettling.",
        is_relic=False, effect=None,
    ),
    # Fungal Depths collectibles
    ArtifactDef(
        id="glowing_spore",
        name="Glowing Spore",
        layer="Fungal Depths",
        rarity="Common",
        lore_text="It pulses like a heartbeat. Don't name it. You'll get attached.",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="fungal_scripture",
        name="Fungal Scripture",
        layer="Fungal Depths",
        rarity="Uncommon",
        lore_text="Written in spore patterns. Roughly translates to: 'We were here before the stone.'",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="sovereign_cap",
        name="Sovereign's Cap",
        layer="Fungal Depths",
        rarity="Rare",
        lore_text="A mushroom cap the size of a dinner plate, still warm from the Sporeling Sovereign's head.",
        is_relic=False, effect=None,
    ),
    # Frozen Core collectibles
    ArtifactDef(
        id="ice_memory",
        name="Ice Memory",
        layer="Frozen Core",
        rarity="Common",
        lore_text="A crystal of frozen time. Inside, a snowflake falls forever.",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="paradox_coin",
        name="Paradox Coin",
        layer="Frozen Core",
        rarity="Uncommon",
        lore_text="Both heads and tails simultaneously. Useless for coin flips. Priceless for philosophers.",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="chrono_shard",
        name="Chrono Shard",
        layer="Frozen Core",
        rarity="Rare",
        lore_text="It shows you what this cave looked like a million years ago: exactly the same.",
        is_relic=False, effect=None,
    ),
    # The Hollow collectibles
    ArtifactDef(
        id="hollow_whisper",
        name="Hollow Whisper",
        layer="The Hollow",
        rarity="Uncommon",
        lore_text="A captured whisper from The Hollow. It says your name sometimes.",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="depth_record",
        name="Depth Record",
        layer="The Hollow",
        rarity="Legendary",
        lore_text="A stone tablet recording the deepest dig ever attempted. The last entry is dated tomorrow.",
        is_relic=False, effect=None,
    ),
    # Special — Roshan drop
    ArtifactDef(
        id="aegis_fragment",
        name="Aegis Fragment",
        layer="The Hollow",
        rarity="Legendary",
        lore_text="A shard of immortality, cracked but not broken. It pulses with defiant energy.",
        is_relic=True,
        effect="Revives from next cave-in (consumed on use)",
    ),
    ArtifactDef(
        id="cheese",
        name="Cheese",
        layer="The Hollow",
        rarity="Legendary",
        lore_text="Aged in the deepest pit of the world. The smell alone could wake an ancient.",
        is_relic=False, effect=None,
    ),
    # PoE nod
    ArtifactDef(
        id="frozen_azurite",
        name="Frozen Azurite",
        layer="Frozen Core",
        rarity="Uncommon",
        lore_text="A deep blue crystal that hums with stored energy. Cartographers prize these above gold.",
        is_relic=False, effect=None,
    ),
]

ALL_ARTIFACTS: list[ArtifactDef] = RELICS + COLLECTIBLE_ARTIFACTS

ARTIFACT_BY_ID: dict[str, ArtifactDef] = {a.id: a for a in ALL_ARTIFACTS}


# ---------------------------------------------------------------------------
# Random Events
# ---------------------------------------------------------------------------

@dataclass(frozen=True)
class EventOutcome:
    """Possible outcome of a choice in a random event."""
    description: str
    advance: int                    # blocks gained (+) or lost (-)
    jc: int                         # JC gained (+) or lost (-)
    cave_in: bool                   # does this trigger a cave-in?


@dataclass(frozen=True)
class SplashConfig:
    """Splash effect that reaches other players when a digger's event resolves.

    ``strategy`` selects the victim pool:
        * ``"random_active"``  - recently-active players in the guild
        * ``"richest_n"``      - top-N positive-balance players
        * ``"active_diggers"`` - players who have dug in the last 7 days

    ``trigger`` picks when the splash fires on the event outcome:
    ``"success"``, ``"failure"``, or ``"always"``.

    ``mode`` controls direction:
        * ``"burn"``  - victims' JC is debited (coins destroyed, deflation lever)
        * ``"grant"`` - targets are credited JC (cooperative splash, e.g.
                        Io tether pact sharing spoils with a partner)
        * ``"steal"`` - victims' JC is transferred to the digger via
                        ``steal_atomic`` (no fee, can push victim below 0
                        down to MAX_DEBT — matches Red/Blue Shell semantics)

    For ``"burn"`` debits are clamped so a non-negative player is not pushed
    below 0. ``"steal"`` is unclamped on the victim side (intentional).
    """

    strategy: str
    victim_count: int
    penalty_jc: int
    trigger: str = "failure"
    mode: str = "burn"


@dataclass(frozen=True)
class EventChoice:
    """A choice the player can make during an event."""
    label: str
    success: EventOutcome
    failure: EventOutcome | None    # None if the choice always succeeds
    success_chance: float           # 0-1, 1.0 = guaranteed


@dataclass(frozen=True)
class TempBuff:
    """Temporary modifier applied by an event outcome."""
    id: str
    name: str
    duration_digs: int
    effect: dict = field(default_factory=dict)  # {"cave_in_reduction": 0.10} or {"advance_bonus": 2}


@dataclass(frozen=True)
class EventStep:
    """One step in a multi-step complex encounter."""
    description: tuple[str, ...]
    choices: list[EventChoice] = field(default_factory=list)


@dataclass(frozen=True)
class RandomEvent:
    """Immutable definition for a random tunnel event."""
    id: str
    name: str                       # internal label (logs, admin, debug) — not shown to players
    description: tuple[str, ...]    # 1+ flavor variants; one is picked at display time
    min_depth: int | None           # None = any depth
    max_depth: int | None           # None = any depth
    safe_option: EventChoice
    risky_option: EventChoice
    # Expansion fields (defaults for backward compatibility with existing events)
    complexity: str = "choice"      # "simple" | "choice" | "complex"
    layer: str | None = None        # restrict to specific layer name, None = any
    rarity: str = "common"          # "common" | "uncommon" | "rare" | "legendary"
    steps: tuple[EventStep, ...] | None = None  # for complex multi-step events
    buff_on_success: TempBuff | None = None      # temp buff granted on risky success
    requires_dark: bool = False     # only triggers at Pitch Black luminosity
    social: bool = False            # references other players
    ascii_art: str | None = None    # roguelike-style ASCII scene (5-7 lines)
    # Prestige expansion fields
    desperate_option: EventChoice | None = None   # third choice: very low odds, massive reward/fail
    boon_options: tuple[TempBuff, ...] | None = None  # for complexity="boon" events
    min_prestige: int = 0           # minimum prestige level required
    # Splash: optional penalty applied to other players in the guild when
    # this event resolves (see SplashConfig.trigger for which outcome fires it).
    splash: SplashConfig | None = None


def pick_description(event: Any) -> str:
    """Pick a random flavor-text variant from an event.

    Accepts RandomEvent, EventStep, wrapper with ``_d`` dict, or plain dict.
    If the description is a tuple/list, one entry is chosen at random.
    If it's a bare string (legacy/dynamic payloads), it's returned as-is.
    """
    desc: Any
    if isinstance(event, dict):
        desc = event.get("description", "")
    elif hasattr(event, "_d") and isinstance(event._d, dict):
        desc = event._d.get("description", "")
    else:
        desc = getattr(event, "description", "")
    if isinstance(desc, (tuple, list)):
        if not desc:
            return ""
        return random.choice(desc)
    return desc or ""


RANDOM_EVENTS: list[RandomEvent] = [
    RandomEvent(
        id="underground_stream",
        name="Underground Stream",
        description=(
            "You break through into a cavern with a rushing underground river.",
            "The wall gives way and cold spray hits your face — an underground river roars past in the dark.",
            "A river you did not know existed carves through the rock ahead of you, fast and unlit.",
        ),
        min_depth=None, max_depth=None,
        safe_option=EventChoice(
            "Wade through carefully",
            success=EventOutcome("You cross safely and find coins on the far bank.", 0, 4, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Dive in and search the riverbed",
            success=EventOutcome("You surface clutching a fistful of treasure!", 0, 10, False),
            failure=EventOutcome("The current drags you back. You lose ground.", -4, 0, False),
            success_chance=0.50,
        ),
    ),
    RandomEvent(
        id="gas_pocket",
        name="Gas Pocket",
        description=(
            "A foul-smelling green gas seeps from a crack in the wall.",
            "Your lantern flame turns sickly green. Something is leaking through the rock.",
            "A hiss. A smell. A color gas has no business being. You hold your breath on instinct.",
        ),
        min_depth=10, max_depth=None,
        safe_option=EventChoice(
            "Retreat and ventilate",
            success=EventOutcome("You wait it out. Fresh air returns.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Dig through the gas",
            success=EventOutcome("The gas was harmless! And behind it—gems!", 2, 6, False),
            failure=EventOutcome("The gas ignites! BOOM!", -8, -12, True),
            success_chance=0.35,
        ),
    ),
    RandomEvent(
        id="techies_cache",
        name="Techies' Hidden Cache",
        description=(
            "A suspicious pile of barrels marked with a smiley face. Squee, Spleen, and Spoon were here.",
            "Barrels. Stacked in a pyramid with a crayon smiley taped to the top one. You do not hear giggling. Probably.",
            "Someone left a tidy pile of explosives in the tunnel. The 'Do Not Dismount' sticker is a nice touch.",
        ),
        min_depth=15, max_depth=75,
        safe_option=EventChoice(
            "Leave the explosives alone",
            success=EventOutcome("Smart choice. You hear a muffled 'aww' from behind the wall.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Repurpose the explosives",
            success=EventOutcome("KABOOM! A new passage opens up—and some coins fly out!", 5, 8, False),
            failure=EventOutcome("KABOOM! ...in the wrong direction. Techies sends his regards.", -9, -15, True),
            success_chance=0.30,
        ),
    ),
    RandomEvent(
        id="meepo_clones",
        name="Meepo's Lost Clones",
        description=(
            "Three identical Meepos are arguing about which way is up.",
            "A Meepo is loudly disagreeing with himself. Four times, in perfect unison, except when he isn't.",
            "Several Meepos are pointing in different directions and all shouting 'this way!' with complete confidence.",
        ),
        min_depth=None, max_depth=50,
        safe_option=EventChoice(
            "Give them directions",
            success=EventOutcome("They thank you and leave a tip. 'We were never here.'", 0, 6, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Convince them to dig for you",
            success=EventOutcome("Four Meepos carve through rock at terrifying speed!", 4, 4, False),
            failure=EventOutcome("They all net you. It takes an hour to untangle.", -3, 0, False),
            success_chance=0.55,
        ),
    ),
    RandomEvent(
        id="cursed_chest",
        name="Cursed Chest",
        description=(
            "An ornate chest sits in the middle of the tunnel, glowing faintly purple.",
            "A chest has been placed dead center in the passage. It hums. The floor around it is suspiciously clean.",
            "A gilded box sits in the tunnel, radiating a faint purple glow. Its lock has no keyhole.",
        ),
        min_depth=25, max_depth=None,
        safe_option=EventChoice(
            "Walk past it",
            success=EventOutcome("Wisdom is its own reward. (You find a coin on the ground.)", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Open the chest",
            success=EventOutcome("Jackpot! The 'curse' was just mood lighting!", 0, 16, False),
            failure=EventOutcome("The chest bites you. Yes, bites.", -6, -15, False),
            success_chance=0.35,
        ),
    ),
    RandomEvent(
        id="lost_miner",
        name="Lost Miner",
        description=(
            "A bewildered miner is wandering in circles, muttering about 'the surface.'",
            "Another miner stumbles past, eyes hollow. 'Which way's up?' they ask. They don't wait for an answer.",
            "A figure in mining gear walks by in the wrong direction, mumbling to a pickaxe they no longer carry.",
        ),
        min_depth=None, max_depth=None,
        safe_option=EventChoice(
            "Point them toward the exit",
            success=EventOutcome("They give you a grateful nod and a couple coins.", 0, 4, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Ask them to help you dig deeper",
            success=EventOutcome("Together you make great progress!", 3, 2, False),
            failure=EventOutcome("They panic and cause a minor collapse.", -3, 0, True),
            success_chance=0.60,
        ),
    ),
    RandomEvent(
        id="crystal_golem",
        name="Crystal Golem",
        description=(
            "A golem made of shimmering crystals blocks the path. It seems to be... sleeping?",
            "Something the size of an ox, built entirely of crystal shards, snores softly against the tunnel wall.",
            "A crystalline giant is crumpled in a heap like it laid down for a nap and forgot to get up.",
        ),
        min_depth=40, max_depth=90,
        safe_option=EventChoice(
            "Tiptoe around it",
            success=EventOutcome("You sneak past. A crystal chip falls—finders keepers.", 0, 4, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Mine the golem for crystals",
            success=EventOutcome("You harvest rare crystals before it wakes!", 0, 12, False),
            failure=EventOutcome("It wakes up. It's not happy.", -8, -12, True),
            success_chance=0.25,
        ),
    ),
    RandomEvent(
        id="mushroom_grove",
        name="Mushroom Grove",
        description=(
            "A bioluminescent mushroom grove fills a side chamber with soft blue light.",
            "Glowing mushrooms the size of your head carpet a small chamber. The light is almost the color of the sky.",
            "You step into a grotto lit only by living blue fungus. It's beautiful and slightly wrong.",
        ),
        min_depth=5, max_depth=60,
        safe_option=EventChoice(
            "Admire and move on",
            success=EventOutcome("The beauty inspires you. Somehow you dig better.", 1, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Eat the glowing mushroom",
            success=EventOutcome("Incredible! You can see through walls! ...temporarily.", 3, 4, False),
            failure=EventOutcome("Your stomach disagrees violently. Time lost.", -2, -3, False),
            success_chance=0.50,
        ),
    ),
    RandomEvent(
        id="magma_geyser",
        name="Magma Geyser",
        description=(
            "The ground rumbles. A vent of superheated steam shoots up ahead.",
            "A low hiss becomes a shriek. Ahead, scalding steam jets from a crack on a timer you don't know.",
            "You feel the heat before you see it — a magma vent blasting straight up through the tunnel.",
        ),
        min_depth=60, max_depth=None,
        safe_option=EventChoice(
            "Wait for it to subside",
            success=EventOutcome("Patience pays off. You continue safely.", 1, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Sprint through between eruptions",
            success=EventOutcome("You dash through and find a treasure cache behind!", 3, 10, False),
            failure=EventOutcome("Terrible timing. You get steamed like a dumpling.", -6, -15, True),
            success_chance=0.30,
        ),
    ),
    RandomEvent(
        id="ancient_elevator",
        name="Ancient Elevator",
        description=(
            "A rickety mine elevator with frayed ropes. A sign reads: 'Use at own risk.'",
            "A wooden lift dangles from rope that is, charitably, half of what it should be. The platform creaks in a breeze that isn't there.",
            "An old mine elevator sits in its shaft. Ropes frayed, sign illegible, and technically still operational.",
        ),
        min_depth=20, max_depth=None,
        safe_option=EventChoice(
            "Take the stairs",
            success=EventOutcome("Slow and steady. You find a coin on a step.", 1, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Ride the elevator",
            success=EventOutcome("WHOOSH! Express to the depths!", 6, 0, False),
            failure=EventOutcome("The rope snaps. You tumble.", -4, -9, True),
            success_chance=0.40,
        ),
    ),
    RandomEvent(
        id="void_whispers",
        name="Void Whispers",
        description=(
            "Unintelligible whispers echo from a crack in the abyss wall. They're... offering a deal?",
            "A thin crack in the rock exhales syllables. You don't understand them but you know what they want.",
            "Voices drift from a fissure, polite and patient. They have been waiting for someone to listen.",
        ),
        min_depth=80, max_depth=None,
        safe_option=EventChoice(
            "Ignore the voices",
            success=EventOutcome("The whispers fade. Probably for the best.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Accept the void's bargain",
            success=EventOutcome("The void is generous today. Dark riches are yours!", 2, 16, False),
            failure=EventOutcome("The void takes more than it gives.", -9, -21, True),
            success_chance=0.25,
        ),
    ),
    RandomEvent(
        id="friendly_mole",
        name="Friendly Mole",
        description=(
            "A large mole pokes its nose out of the wall and squeaks at you encouragingly.",
            "Something soft and velvet-furred emerges from the dirt in front of you, chirps once, and waits.",
            "A mole the size of a small dog has made a hole beside yours. It seems very pleased to have company.",
        ),
        min_depth=None, max_depth=40,
        safe_option=EventChoice(
            "Pet the mole",
            success=EventOutcome("The mole is delighted! It shows you a shortcut.", 2, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Follow the mole into its burrow",
            success=EventOutcome("The mole leads you to its treasure stash!", 3, 8, False),
            failure=EventOutcome("You get stuck in the burrow. Embarrassing.", -3, 0, False),
            success_chance=0.55,
        ),
    ),

    # ===================================================================
    # EXPANSION EVENTS — Dirt Layer (0-25)
    # ===================================================================

    RandomEvent(
        id="worm_council",
        name="Worm Council",
        description=(
            "A circle of earthworms convenes before you. They appear to be voting on something important.",
            "Worms in a perfect ring. One of them is clearly the chair. They pause their deliberation to look at you.",
            "Something that looks like a parliament of worms is in session. You have walked in during an important motion.",
        ),
        min_depth=None, max_depth=25,
        safe_option=EventChoice(
            "Observe respectfully",
            success=EventOutcome("The motion passes. You are declared an honorary annelid.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Cast a vote",
            success=EventOutcome("Your vote tips the balance. The worms are grateful.", 1, 4, False),
            failure=EventOutcome("You voted wrong. The worms are displeased.", -2, 0, False),
            success_chance=0.65,
        ),
        complexity="simple", layer="Dirt",
    ),
    RandomEvent(
        id="buried_lunch_box",
        name="Buried Lunch Box",
        description=(
            "A perfectly preserved lunch box from the surface. The sandwich inside is... questionable.",
            "A tin lunch pail, rust-free despite the years, with a sandwich inside that smells neither fresh nor dead.",
            "You unearth someone's lost midday meal. The bread is intact. That is the most alarming part.",
        ),
        min_depth=None, max_depth=25,
        safe_option=EventChoice(
            "Sell the vintage box",
            success=EventOutcome("A collector would pay good money for this.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Eat the ancient sandwich",
            success=EventOutcome("Aged like fine wine. You feel invigorated.", 1, 6, False),
            failure=EventOutcome("Food poisoning. Your ancestors are disappointed.", 0, -3, False),
            success_chance=0.60,
        ),
        layer="Dirt",
    ),
    RandomEvent(
        id="dig_dog",
        name="The Dig Dog",
        description=(
            "A dog is digging enthusiastically nearby. It is objectively better at this than you.",
            "A scruffy terrier has a tunnel going in parallel to yours. It is already two blocks ahead.",
            "A dog. Underground. Digging with the technique of a professional. It hasn't noticed you yet.",
        ),
        min_depth=None, max_depth=25,
        safe_option=EventChoice(
            "Watch and learn",
            success=EventOutcome("The dog teaches you a new technique. Good boy.", 2, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Race the dog",
            success=EventOutcome("You win! The dog doesn't care. It's just happy to dig.", 3, 2, False),
            failure=EventOutcome("The dog laps you twice. Humbling.", 0, 0, False),
            success_chance=0.40,
        ),
        complexity="simple", layer="Dirt",
    ),
    RandomEvent(
        id="root_maze",
        name="Root Maze",
        description=(
            "A tangle of ancient roots blocks the path. Something metallic glints deep inside.",
            "Old roots have woven themselves into a wall. Deep in the mesh, the reflection of something small and shiny.",
            "The passage ends in a wall of gnarled roots. Through a gap, you catch a metallic wink.",
        ),
        min_depth=5, max_depth=25,
        safe_option=EventChoice(
            "Hack a narrow path",
            success=EventOutcome("Slow but steady. You squeeze through.", 1, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Reach for the glint",
            success=EventOutcome("A handful of buried coins! Worth the scratches.", 3, 4, False),
            failure=EventOutcome("Tangled. It takes an hour to free yourself.", -3, 0, False),
            success_chance=0.50,
        ),
        layer="Dirt",
    ),
    RandomEvent(
        id="pickaxe_head_flies_off",
        name="Pickaxe Head Flies Off",
        description=(
            "Your pickaxe head detaches mid-swing and sails into the darkness. You hear it land... somewhere.",
            "CLANG. Your pickaxe is suddenly half a pickaxe. The heavy end is out there, past the lantern light.",
            "The head of your tool flies off mid-swing. It bounces once, twice, and is gone.",
        ),
        min_depth=None, max_depth=30,
        safe_option=EventChoice(
            "Go find it",
            success=EventOutcome("Found it! And some coins along the way.", -2, 4, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Dig with your hands",
            success=EventOutcome("Surprisingly effective. You find the head AND a geode.", 1, 6, False),
            failure=EventOutcome("Your hands disagree with this approach.", -3, 0, False),
            success_chance=0.45,
        ),
        complexity="simple", layer="Dirt",
    ),

    # ===================================================================
    # EXPANSION EVENTS — Stone Layer (26-50)
    # ===================================================================

    RandomEvent(
        id="toll_keeper",
        name="The Toll Keeper",
        description=(
            "A spectral figure blocks the path. It holds out one translucent hand. 'Toll or riddle. Your choice.'",
            "A ghost in threadbare robes stands in the middle of the tunnel. 'The usual,' it sighs. 'Coin or clever.'",
            "Cold air thickens into a translucent shape. It has been guarding this passage for a very long time and is clearly bored.",
        ),
        min_depth=26, max_depth=55,
        safe_option=EventChoice(
            "Pay the toll (3 JC)",
            success=EventOutcome("The ghost pockets the coins somehow and steps aside.", 2, -9, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Answer the riddle",
            success=EventOutcome("'Correct,' it says, sounding disappointed. It drops its own toll.", 3, 10, False),
            failure=EventOutcome("Wrong. 'Everyone says fire,' it sighs. It takes your coins anyway.", 0, -6, False),
            success_chance=0.50,
        ),
        complexity="complex", layer="Stone",
    ),
    RandomEvent(
        id="gravity_pocket",
        name="Gravity Pocket",
        description=(
            "An area where gravity is... optional. Your tools float. You float. Everything floats down here.",
            "You take one step and your feet don't come back down. Your pickaxe hovers at eye level, patient.",
            "Something has switched off the down. You drift sideways, mildly confused, alongside a cloud of rock dust.",
        ),
        min_depth=26, max_depth=55,
        safe_option=EventChoice(
            "Wait for gravity to return",
            success=EventOutcome("It does. Eventually. You feel vaguely insulted.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Swim downward through the void",
            success=EventOutcome("You swim through solid rock. Best not to think about it.", 4, 0, False),
            failure=EventOutcome("You float up instead. Ceiling is farther than you thought.", -4, 0, False),
            success_chance=0.55,
        ),
        layer="Stone",
    ),
    RandomEvent(
        id="fossil_argument",
        name="Fossil Argument",
        description=(
            "Two fossils embedded in the wall face each other. They look like they were fighting when they died.",
            "Locked in the stone, two creatures frozen mid-grapple. Nobody won. Nobody moved.",
            "Two skeletons fused into the wall, still arguing after several million years.",
        ),
        min_depth=26, max_depth=55,
        safe_option=EventChoice(
            "Leave them to their eternal dispute",
            success=EventOutcome("Some arguments aren't worth getting between.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Extract both fossils",
            success=EventOutcome("Museum quality! Separate display cases, obviously.", 0, 8, False),
            failure=EventOutcome("They crumble to dust the moment you touch them. Of course.", -4, 0, False),
            success_chance=0.45,
        ),
        layer="Stone",
    ),
    RandomEvent(
        id="sandwich_lady",
        name="The Sandwich Lady",
        description=(
            "A woman appears from behind a stalagmite carrying a tray of baguettes. 'I didn't ask for this,' you think.",
            "A woman rounds the corner with a tray of baguettes, as if the tunnel were a hotel corridor and you had called room service.",
            "Someone is selling sandwiches down here. Underground. A hundred feet deep. She seems completely unbothered.",
        ),
        min_depth=26, max_depth=55,
        safe_option=EventChoice(
            "Accept the baguette",
            success=EventOutcome("The baguette is surprisingly good. She vanishes.", 0, 4, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Decline politely",
            success=EventOutcome("She shrugs and leaves a tip jar. It has coins in it.", 0, 6, False),
            failure=EventOutcome("She doesn't take rejection well. Baguette to the face.", -2, 0, False),
            success_chance=0.70,
        ),
        complexity="simple", layer="Stone",
    ),
    RandomEvent(
        id="echo_chamber",
        name="Echo Chamber",
        description=(
            "Your footsteps echo in impossibly complex patterns. For a moment, you hear the steps of every miner who has ever been here.",
            "The chamber plays your footsteps back to you, layered with the footsteps of everyone who ever stood where you stand.",
            "You take a single step. The echo comes back as a chorus. You are not the first.",
        ),
        min_depth=26, max_depth=55,
        safe_option=EventChoice(
            "Listen",
            success=EventOutcome("The echoes fade. You feel less alone. Somehow that's worse.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Shout into the chamber",
            success=EventOutcome("Your echo comes back with directions. And coins.", 2, 4, False),
            failure=EventOutcome("Your echo comes back louder. Much louder. Cave-in.", -4, 0, True),
            success_chance=0.55,
        ),
        complexity="simple", layer="Stone", social=True,
    ),

    # ===================================================================
    # EXPANSION EVENTS — Crystal Layer (51-75)
    # ===================================================================

    RandomEvent(
        id="mirror_tunnel",
        name="Mirror Tunnel",
        description=(
            "Crystal walls reflect infinite copies of you. One of the reflections waves. You didn't wave.",
            "The corridor is mirrored on every surface. You are reflected a thousand times. One of you is smiling.",
            "Polished crystal on every wall. You see yourself everywhere. Then one of the yous blinks out of sync.",
        ),
        min_depth=51, max_depth=80,
        safe_option=EventChoice(
            "Ignore it and move on",
            success=EventOutcome("Smart. The reflection looks disappointed.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Wave back",
            success=EventOutcome("The reflection smiles and points to a hidden cache.", 2, 8, False),
            failure=EventOutcome("It grabs your hand and tries to pull you through. You wrench free.", -4, -3, False),
            success_chance=0.50,
        ),
        complexity="complex", layer="Crystal",
    ),
    RandomEvent(
        id="resonance_cascade",
        name="Resonance Cascade",
        description=(
            "Crystals vibrate at increasing frequency. They're about to shatter. The air tastes like ozone.",
            "The whole chamber is humming. The pitch climbs past what your teeth can stand. Something is about to give.",
            "Every crystal in the wall is ringing like a tuning fork. You have maybe thirty seconds before they all let go.",
        ),
        min_depth=51, max_depth=80,
        safe_option=EventChoice(
            "Run",
            success=EventOutcome("You make it out just as the cascade peaks. Glass everywhere.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Stand your ground",
            success=EventOutcome("The shatter opens a massive cavern. You're the first one here.", 5, 8, False),
            failure=EventOutcome("The shatter opens nothing. Except your skin. Medical bill incoming.", -8, -6, True),
            success_chance=0.35,
        ),
        layer="Crystal",
    ),
    RandomEvent(
        id="crystal_garden",
        name="Crystal Garden",
        description=(
            "Someone has been cultivating crystals here. Tiny signs read 'do not touch' and 'water weekly.'",
            "A little crystal garden sits in a sconce, neatly arranged. Hand-lettered placards label each variety in a language you don't know.",
            "Someone has been tending crystals like houseplants. There is a watering can. Nobody has been here in a while.",
        ),
        min_depth=51, max_depth=80,
        safe_option=EventChoice(
            "Admire the garden",
            success=EventOutcome("You find a donation jar. It's been a while since anyone else visited.", 0, 4, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Harvest a crystal",
            success=EventOutcome("The gardener isn't coming back. These are yours now.", 0, 10, False),
            failure=EventOutcome("The crystals fight back. They're pricklier than they look.", -3, -3, False),
            success_chance=0.45,
        ),
        complexity="simple", layer="Crystal",
    ),
    RandomEvent(
        id="gem_rock",
        name="Gem Rock",
        description=(
            "A rock glitters with embedded gems. Mining it is risky — these rocks are known to fight back.",
            "A boulder studded with raw gems sits in the middle of your path. It is slightly warm to the touch.",
            "You find a lump of ore flecked with color. Everyone you know has warned you about lumps of ore flecked with color.",
        ),
        min_depth=51, max_depth=80,
        safe_option=EventChoice(
            "Chip carefully at the edges",
            success=EventOutcome("A modest haul. Patience rewarded.", 0, 4, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Swing hard",
            success=EventOutcome("Uncut diamond! ...wait, wrong game. Still valuable though.", 0, 12, False),
            failure=EventOutcome("The rock was a mimic. Why is everything down here a mimic.", -3, -6, False),
            success_chance=0.40,
        ),
        layer="Crystal",
    ),
    RandomEvent(
        id="prism_trap",
        name="Prism Trap",
        description=(
            "Light bends through crystal prisms creating a dazzling but disorienting maze of rainbows.",
            "The tunnel becomes a refraction puzzle. Rainbows crisscross the floor. You can't tell where solid rock ends and colored air begins.",
            "Prisms scatter your lantern light into a dozen false paths. Only one of them is real.",
        ),
        min_depth=51, max_depth=80,
        safe_option=EventChoice(
            "Close your eyes and feel the walls",
            success=EventOutcome("Slow but effective. You emerge on the other side.", 1, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Follow the brightest beam",
            success=EventOutcome("The light leads you true. A shortcut reveals itself.", 3, 4, False),
            failure=EventOutcome("You walk in circles for an hour. The prisms mock you.", -4, 0, False),
            success_chance=0.50,
        ),
        layer="Crystal",
    ),

    # ===================================================================
    # EXPANSION EVENTS — Magma Layer (76-100)
    # ===================================================================

    RandomEvent(
        id="lava_surfer",
        name="Lava Surfer",
        description=(
            "A chunk of obsidian floats on a lava river. It's just barely big enough to stand on.",
            "A slab of cooled volcanic glass rides the lava flow like a raft. The current is faster than it looks.",
            "A river of molten rock. A single dark slab rides the surface. Someone has carved a foothold into it.",
        ),
        min_depth=76, max_depth=105,
        safe_option=EventChoice(
            "Go around",
            success=EventOutcome("The long way. Safe, boring, and slightly damp.", 1, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Surf the lava",
            success=EventOutcome("You ride the rapids like a champion. The lava respects you.", 6, 4, False),
            failure=EventOutcome("You fall in. It's exactly as hot as you'd expect.", -6, -9, True),
            success_chance=0.40,
        ),
        layer="Magma",
    ),
    RandomEvent(
        id="forge_spirit",
        name="Forge Spirit",
        description=(
            "A fire elemental hammers at an anvil. It notices you. 'Trade or fight. I don't do small talk.'",
            "Something shaped like a man, made entirely of fire, is working an anvil. It pauses, hammer raised, and looks at you.",
            "An elemental smith turns its burning face toward you. The anvil smokes. 'Trade. Or don't.'",
        ),
        min_depth=76, max_depth=105,
        safe_option=EventChoice(
            "Trade (5 JC)",
            success=EventOutcome("It hands you something warm. Your next digs feel... enhanced.", 0, -15, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Fight it",
            success=EventOutcome("You shatter the elemental. Its core is worth a fortune.", 0, 16, False),
            failure=EventOutcome("Fire hurts. Who knew.", -6, -6, True),
            success_chance=0.40,
        ),
        complexity="complex", layer="Magma",
        buff_on_success=TempBuff("forged", "Forge-Tempered", 3, {"advance_bonus": 2}),
    ),
    RandomEvent(
        id="volcanic_vent_gambit",
        name="Volcanic Vent Gambit",
        description=(
            "A volcanic vent cycles between eruptions. The gap between bursts is exactly three heartbeats.",
            "A geyser of fire punches the ceiling on a rhythm. One. Two. Three. One. Two. Three.",
            "The vent ahead fires in perfect intervals. You can cross between bursts if you are brave and fast.",
        ),
        min_depth=76, max_depth=None,
        safe_option=EventChoice(
            "Wait for it to subside",
            success=EventOutcome("Patience. The vent cools after an hour.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Sprint through between eruptions",
            success=EventOutcome("Three heartbeats. You made it in two.", 8, 6, False),
            failure=EventOutcome("Terrible timing. Cooked.", -9, -9, True),
            success_chance=0.30,
        ),
        layer="Magma",
    ),
    RandomEvent(
        id="heat_mirage",
        name="Heat Mirage",
        description=(
            "You see an oasis ahead. With a swimming pool. And a bartender. It's 100% not real.",
            "The heat shimmers, and through the shimmer you see palm trees, a pool, and a man polishing glasses. It cannot possibly be there.",
            "A resort. In a magma tunnel. The bartender even waves at you. You are not fooled. You are tempted.",
        ),
        min_depth=76, max_depth=105,
        safe_option=EventChoice(
            "Keep walking",
            success=EventOutcome("You knew it wasn't real but you're still disappointed.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Walk toward it anyway",
            success=EventOutcome("Wait. The bartender WAS real? Free drinks.", 0, 10, False),
            failure=EventOutcome("It was never real. You wasted an hour.", -3, 0, False),
            success_chance=0.25,
        ),
        complexity="simple", layer="Magma",
    ),
    RandomEvent(
        id="shooting_star",
        name="Shooting Star",
        description=(
            "A blazing rock tears through the cavern wall and embeds itself in the floor. It radiates warmth and value.",
            "A chunk of sky has punched through the ceiling. The rock at the bottom of the crater is still glowing.",
            "You watch a meteor carve a line through the cavern and thud into the ground. It hisses softly. It is yours for the taking.",
        ),
        min_depth=76, max_depth=None,
        safe_option=EventChoice(
            "Mine the star",
            success=EventOutcome("Star fragments. Warm to the touch and surprisingly dense.", 0, 8, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Crack it open",
            success=EventOutcome("The core is pure stardust. Priceless.", 0, 16, False),
            failure=EventOutcome("It explodes. Stars do that sometimes.", -4, -3, True),
            success_chance=0.35,
        ),
        complexity="simple", layer="Magma", rarity="uncommon",
    ),

    # ===================================================================
    # EXPANSION EVENTS — Dota Hero Encounters
    # ===================================================================

    RandomEvent(
        id="pudge_fishing",
        name="The Chasm Butcher",
        description=(
            "A butcher on the far side of a chasm is fishing with a meat hook. 'Fresh meat delivery service!' he calls. 'First ride's free.'",
            "A massive butcher stands on the far rim of a chasm, swinging a meat hook over his head. 'FRESH MEAT!' he calls cheerfully. He points at you.",
            "Across the gap, an enormous figure grins through rotten teeth. He holds a meat hook the size of your arm. 'First ride's free.'",
        ),
        min_depth=26, max_depth=None,
        safe_option=EventChoice(
            "Climb around the chasm",
            success=EventOutcome("The long way. You find coins in the crevices.", 0, 4, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Grab the hook",
            success=EventOutcome("WHEEE! You arrive covered in something but 6 blocks deeper.", 6, 0, False),
            failure=EventOutcome("The hook misses. You fall. The butcher waves sadly from above.", -6, -6, True),
            success_chance=0.50,
        ),
        rarity="uncommon",
    ),
    RandomEvent(
        id="tinker_workshop",
        name="The Abandoned Workshop",
        description=(
            "A cluttered workshop full of half-finished gadgets. A sign reads: 'Gone to rearm. Back in 5 minutes.' The sign is 400 years old.",
            "A workbench covered in half-built gadgets, gears, and miniature rockets. A note pinned to the wall: 'be right back'. The ink is very, very faded.",
            "Someone abandoned this tinkerer's workshop mid-project. Half-finished inventions still hum faintly on the bench, waiting for a return.",
        ),
        min_depth=51, max_depth=None,
        safe_option=EventChoice(
            "Scavenge for parts",
            success=EventOutcome("Spare parts. Not glamorous, but sellable.", 0, 6, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Repair a gadget",
            success=EventOutcome("It works! A drill that practically digs itself.", 0, 0, False),
            failure=EventOutcome("It doesn't work. It also sparks. And now your eyebrows are gone.", -3, -3, False),
            success_chance=0.45,
        ),
        complexity="complex", layer="Crystal", rarity="uncommon",
        buff_on_success=TempBuff("tinker_drill", "Rearm Protocol", 3, {"advance_bonus": 2}),
    ),
    RandomEvent(
        id="the_burrow",
        name="The Burrow",
        description=(
            "The ground cracks beneath you. Something chitinous scuttles below. It's been waiting.",
            "A hairline fissure spreads under your boots, and beneath it, a dozen faceted eyes open at once.",
            "The floor is thinner than it should be. Underneath, something carapaced is moving, slow and deliberate.",
        ),
        min_depth=101, max_depth=None,
        safe_option=EventChoice(
            "Retreat slowly",
            success=EventOutcome("It lets you go. This time.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Investigate the cracks",
            success=EventOutcome("An ancient intelligence offers knowledge. Your depth for its wisdom.", 0, 16, False),
            failure=EventOutcome("It didn't want to talk. It wanted lunch.", -8, -9, True),
            success_chance=0.35,
        ),
        complexity="complex", rarity="uncommon",
        buff_on_success=TempBuff("nyx_insight", "Vendetta's Whisper", 2, {"cave_in_reduction": 0.10}),
    ),
    RandomEvent(
        id="arcanist_library",
        name="The Arcanist's Library",
        description=(
            "A chamber lined with tomes. An impossibly old man surrounded by floating orbs doesn't look up. 'I know why you're here,' he says. He sounds bored.",
            "Bookshelves carved into the ice rise past your lantern's reach. A hooded figure turns a page without looking at you. 'Ask your question.'",
            "A library that should not exist. At its center, an old man in grey robes surrounded by slow-orbiting orbs. 'Yes, yes. Come in.'",
        ),
        min_depth=201, max_depth=None,
        safe_option=EventChoice(
            "Browse the shelves",
            success=EventOutcome("You find a primer on geological theory. +30 luminosity restored.", 0, 4, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Ask about the orbs",
            success=EventOutcome("He almost smiles. 'Choose: cold, energy, or fire.' Cold restores your light. The others... well.", 3, 16, False),
            failure=EventOutcome("He waves his hand. You're outside. You don't remember leaving.", -6, 0, False),
            success_chance=0.40,
        ),
        complexity="complex", layer="Frozen Core", rarity="rare",
    ),
    RandomEvent(
        id="the_dark_rift",
        name="The Dark Rift",
        description=(
            "The tunnel opens into a vast underground kingdom. A horned figure on a throne of basalt offers passage. 'Everything has a price,' it says. 'Even the ground you stand on.'",
            "The passage empties into a throne room the size of a city. Something horned watches you from a seat carved from a single block of obsidian.",
            "You step into an underground kingdom. A figure with antlers of black stone waits on a throne. It has been expecting you for some time.",
        ),
        min_depth=101, max_depth=155,
        safe_option=EventChoice(
            "Bow and withdraw",
            success=EventOutcome("It nods. Respect costs nothing. You find coins by the exit.", 0, 6, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Challenge for passage",
            success=EventOutcome("You win the challenge. The throne room collapses into a shortcut.", 8, 10, False),
            failure=EventOutcome("It wasn't really a challenge. More of a demonstration.", -9, -12, True),
            success_chance=0.35,
        ),
        complexity="complex", layer="Abyss", rarity="uncommon",
    ),

    # ===================================================================
    # EXPANSION EVENTS — Abyss Layer (101-150)
    # ===================================================================

    RandomEvent(
        id="void_market",
        name="Void Market",
        description=(
            "Shadowy merchants materialize from nothing. They trade in concepts. 'Depth for wealth? Wealth for depth? Information for blocks?'",
            "Three silhouettes fade into existence around you, draped in hoods that are more void than cloth. 'We barter,' one says.",
            "The dark folds itself into the shape of merchants. They have no faces but they have terms, and the terms are fair.",
        ),
        min_depth=101, max_depth=155,
        safe_option=EventChoice(
            "Trade depth for wealth (3 blocks for 6 JC)",
            success=EventOutcome("Fair trade. Your tunnel shortens. Your wallet fattens.", -4, 12, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Trade wealth for depth (6 JC for 5 blocks)",
            success=EventOutcome("The shadows accept. You feel your tunnel extend.", 5, -18, False),
            failure=EventOutcome("The shadows take your coins and vanish. No refunds in the void.", 0, -18, False),
            success_chance=0.75,
        ),
        complexity="complex", layer="Abyss", rarity="uncommon",
    ),
    RandomEvent(
        id="abyssal_fishing",
        name="Abyssal Fishing",
        description=(
            "A luminous pool of liquid void. Something moves beneath the surface. It might be valuable. It might be alive.",
            "A pool so black it seems to drink the light. Beneath its surface, something large turns over slowly.",
            "The water in this pool is not water. It ripples without wind, and something pale rises just close enough to see before sinking again.",
        ),
        min_depth=101, max_depth=155,
        safe_option=EventChoice(
            "Peer into the depths",
            success=EventOutcome("You see your reflection. It winks. You earn 2 JC for your trouble.", 0, 4, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Reach in",
            success=EventOutcome("Your hand closes around something cold and heavy. Treasure.", 0, 16, False),
            failure=EventOutcome("Whatever it was, it pulled your coin purse in. And your glove.", 0, -12, False),
            success_chance=0.50,
        ),
        layer="Abyss", rarity="uncommon",
    ),
    RandomEvent(
        id="gravity_inversion",
        name="Gravity Inversion",
        description=(
            "You are suddenly falling upward. Then sideways. Then in a direction that doesn't have a name.",
            "Down stops working. You fall, but the fall has no direction you can put a word to.",
            "Gravity turns inside out. Your pickaxe floats to the ceiling. You follow it, then keep going.",
        ),
        min_depth=101, max_depth=None,
        safe_option=EventChoice(
            "Grab hold of something",
            success=EventOutcome("You cling to a stalactite until reality sorts itself out.", 1, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Fall sideways on purpose",
            success=EventOutcome("You fall through solid rock. The physics down here are generous.", 5, 4, False),
            failure=EventOutcome("You fall up, then down, then up again. Nauseating.", -6, -3, False),
            success_chance=0.45,
        ),
        layer="Abyss",
    ),
    RandomEvent(
        id="whispering_walls_extended",
        name="Whispering Walls",
        description=(
            "The void's whispers form coherent sentences. 'We remember when this stone was sky.'",
            "The walls are speaking. Not metaphorically. They have things to say about the age before there was rock.",
            "For the first time down here, the whispers use words. 'We remember.' It repeats, patient, for your benefit.",
        ),
        min_depth=101, max_depth=155,
        safe_option=EventChoice(
            "Cover your ears",
            success=EventOutcome("The whispers fade. Some things are better left unheard.", 0, 4, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Listen carefully",
            success=EventOutcome("'...and the hero bought a clarity and walked into the trees.' Wait, what? Still, the void pays well for listeners.", 2, 20, False),
            failure=EventOutcome("The whispers get inside your head. They rearrange things.", -9, -12, True),
            success_chance=0.30,
        ),
        complexity="complex", layer="Abyss", rarity="rare",
    ),

    # ===================================================================
    # EXPANSION EVENTS — Fungal Depths (151-200)
    # ===================================================================

    RandomEvent(
        id="spore_storm",
        name="Spore Storm",
        description=(
            "A cloud of bioluminescent spores erupts from the fungal walls. Beautiful. Also, you're inhaling them.",
            "The wall ahead breathes out a sparkling green cloud. It's lovely. It's also already in your lungs.",
            "Spores everywhere — glowing, drifting, slightly sweet. You are suddenly aware of every breath you have ever taken.",
        ),
        min_depth=151, max_depth=205,
        safe_option=EventChoice(
            "Retreat and breathe",
            success=EventOutcome("Fresh air. Relatively speaking. Everything down here is questionable.", 1, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Breathe deep",
            success=EventOutcome("The spores are... enhancing? You can see further and dig faster.", 4, 6, False),
            failure=EventOutcome("Spore cough. Your lungs file a formal complaint.", -4, -3, False),
            success_chance=0.45,
        ),
        layer="Fungal Depths",
        buff_on_success=TempBuff("spore_enhanced", "Spore-Enhanced", 2, {"advance_bonus": 1}),
    ),
    RandomEvent(
        id="mycelium_network",
        name="Mycelium Network",
        description=(
            "The fungal root network pulses with light. Data flows through it — the locations of other tunnels.",
            "A web of mycelium covers the chamber, and every strand pulses in sequence. It is transmitting something.",
            "The fungus here is thinking. Light moves through its filaments the way a signal moves through a wire.",
        ),
        min_depth=151, max_depth=205,
        safe_option=EventChoice(
            "Just watch",
            success=EventOutcome("The network hums. You feel connected to something vast.", 0, 6, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Tap in",
            success=EventOutcome("You see the depths of other miners. Knowledge is power.", 0, 10, False),
            failure=EventOutcome("The network rejects you. A psychic headache.", -3, 0, False),
            success_chance=0.55,
        ),
        complexity="complex", layer="Fungal Depths", rarity="uncommon", social=True,
    ),
    RandomEvent(
        id="sporewalker",
        name="The Sporewalker",
        description=(
            "A humanoid made entirely of mushrooms waves at you. It offers a glowing fungal cap with the enthusiasm of someone who has never been told 'no.'",
            "Something vaguely human-shaped, built entirely of mushroom flesh, steps forward with a glowing cap held out on its palm.",
            "A walking mushroom lights up at the sight of you and presents a cap like a gift. It's very proud of this cap.",
        ),
        min_depth=151, max_depth=205,
        safe_option=EventChoice(
            "Accept the cap",
            success=EventOutcome("It tastes like earth and starlight. Your tunnel walls feel more solid.", 0, 6, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Ask for the premium cap",
            success=EventOutcome("The Sporewalker beams. The premium cap glows brighter.", 2, 10, False),
            failure=EventOutcome("There is no premium cap. The Sporewalker is confused. And hurt.", 0, 2, False),
            success_chance=0.50,
        ),
        complexity="simple", layer="Fungal Depths",
        buff_on_success=TempBuff("spore_shield", "Fungal Fortitude", 3, {"cave_in_reduction": 0.05}),
    ),
    RandomEvent(
        id="bioluminescent_cathedral",
        name="Bioluminescent Cathedral",
        description=(
            "You emerge into a cavern so vast your lantern is unnecessary. The ceiling is a galaxy of living light.",
            "The passage opens into a chamber the size of a cathedral. Its ceiling is alive with bioluminescent stars.",
            "You turn off your lantern. You don't need it. The ceiling is holding its own light, and it is an entire sky.",
        ),
        min_depth=151, max_depth=205,
        safe_option=EventChoice(
            "Sit and watch",
            success=EventOutcome("For a moment, the weight of all that stone above you doesn't matter.", 0, 10, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Climb toward the lights",
            success=EventOutcome("You reach the bioluminescent canopy. Up close, it's even more beautiful.", 3, 16, False),
            failure=EventOutcome("You slip. The fall is long but the mushrooms are soft.", -6, 0, False),
            success_chance=0.40,
        ),
        complexity="simple", layer="Fungal Depths", rarity="rare",
    ),

    # ===================================================================
    # EXPANSION EVENTS — Frozen Core (201-275)
    # ===================================================================

    RandomEvent(
        id="time_eddy",
        name="Time Eddy",
        description=(
            "A pocket where time runs differently. You can see your past digs playing out like ghosts ahead of you.",
            "The air ahead shimmers. In it, you see yourself digging at a thousand different past moments, overlapping.",
            "Time is curdled here. Your own ghost is swinging a pickaxe in the middle of the tunnel, a few minutes behind you. Or ahead.",
        ),
        min_depth=201, max_depth=280,
        safe_option=EventChoice(
            "Observe from outside",
            success=EventOutcome("You learn from watching your past mistakes. The future feels clearer.", 0, 4, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Step inside",
            success=EventOutcome("Time bends around you. For a moment you exist in two places at once. Both of them dig.", 4, 8, False),
            failure=EventOutcome("You step out and three hours have passed. Time is cruel.", -4, -6, False),
            success_chance=0.45,
        ),
        complexity="complex", layer="Frozen Core",
    ),
    RandomEvent(
        id="frozen_ancient",
        name="Frozen Ancient",
        description=(
            "Something enormous is frozen in the ice. It has too many limbs. Most of them are wrong.",
            "A shape the size of a whale is locked in the ice wall. Its anatomy does not match anything in any book.",
            "A massive figure, perfectly preserved, embedded in the frozen rock. You try to count its limbs and lose count.",
        ),
        min_depth=201, max_depth=280,
        safe_option=EventChoice(
            "Photograph it",
            success=EventOutcome("For posterity. And selling to tabloids.", 0, 6, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Chip a piece free",
            success=EventOutcome("It's warm. Organic. Worth a fortune to the right buyer.", 0, 20, False),
            failure=EventOutcome("It moved. IT MOVED.", -9, -9, True),
            success_chance=0.35,
        ),
        layer="Frozen Core", rarity="uncommon",
    ),
    RandomEvent(
        id="the_still_point",
        name="The Still Point",
        description=(
            "Absolute silence. Absolute cold. Absolute peace. For one perfect moment, nothing decays.",
            "The chamber is so still that your heartbeat feels loud. The air is not cold. The air is not anything.",
            "A pocket of impossible stillness. No sound. No temperature. No motion. You feel time stop being a factor.",
        ),
        min_depth=201, max_depth=280,
        safe_option=EventChoice(
            "Be still",
            success=EventOutcome("The moment passes. But something lingers — your tunnel feels more permanent.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Try to hold the moment",
            success=EventOutcome("You can't. But trying earned you something.", 0, 10, False),
            failure=EventOutcome("The moment shatters. Cold rushes in.", -3, 0, False),
            success_chance=0.50,
        ),
        complexity="simple", layer="Frozen Core", rarity="rare",
        buff_on_success=TempBuff("still_point", "Temporal Anchor", 5, {"cave_in_reduction": 0.08}),
    ),
    RandomEvent(
        id="paradox_loop",
        name="Paradox Loop",
        description=(
            "You meet yourself coming the other direction. You both stop. Neither of you seems surprised.",
            "Around the next corner, you are waiting for yourself. The other you is holding a lantern at the same angle.",
            "You see yourself walking toward you. Same pickaxe. Same dust on the boots. Neither of you reacts. You have been expecting this.",
        ),
        min_depth=201, max_depth=None,
        safe_option=EventChoice(
            "Offer yourself a coin",
            success=EventOutcome("Your other self accepts. You feel generous and broke simultaneously.", 3, -3, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Fight yourself",
            success=EventOutcome("You win. Or you lost. The distinction is academic at this depth.", 5, 6, False),
            failure=EventOutcome("You lose to yourself. The humiliation is paradoxically doubled.", -8, -6, False),
            success_chance=0.50,
        ),
        complexity="complex", layer="Frozen Core", rarity="rare",
    ),

    # ===================================================================
    # EXPANSION EVENTS — The Hollow (276+)
    # ===================================================================

    RandomEvent(
        id="the_cartographer",
        name="The Cartographer",
        description=(
            "A figure with no face draws maps on the walls. The maps are of places that shouldn't exist. Some of them are accurate.",
            "A tall figure in grey is sketching on the walls with a piece of chalk. Maps, coastlines, rooms. None of them are places you know. All of them are detailed.",
            "A cartographer without a face hunches over a wall, drawing. The maps are of nowhere you've ever been. At least one of them is the tunnel you came from.",
        ),
        min_depth=276, max_depth=None,
        safe_option=EventChoice(
            "Help draw",
            success=EventOutcome("Your contribution is... noted. The Cartographer pays in concepts that translate to coins.", 2, 10, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Buy a map (10 JC)",
            success=EventOutcome("The map shows paths that shouldn't exist. They do anyway.", 5, -30, False),
            failure=EventOutcome("The map is blank. 'That IS The Hollow,' the Cartographer explains.", 0, -30, False),
            success_chance=0.60,
        ),
        complexity="complex", layer="The Hollow", rarity="rare",
    ),
    RandomEvent(
        id="the_final_merchant",
        name="The Final Merchant",
        description=(
            "A merchant who sells only one thing, and it changes every time you meet them. They seem tired of existing.",
            "A stall in the dark. On it: one item. A single item you have never seen before. Behind it, someone who looks like they have been here a very long time.",
            "A merchant without a stall waits in the center of the chamber. They hold one thing. You cannot quite tell what it is. 'Last one,' they say, without hope.",
        ),
        min_depth=276, max_depth=None,
        safe_option=EventChoice(
            "Browse and leave",
            success=EventOutcome("'Everyone browses. Nobody buys,' the merchant says. 'Story of my life.'", 0, 4, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Haggle",
            success=EventOutcome("'Fine. Half price. Just take it.' The merchant looks relieved.", 3, 16, False),
            failure=EventOutcome("The price doubles. 'That's what you get for haggling with the void.'", 0, -15, False),
            success_chance=0.40,
        ),
        complexity="complex", layer="The Hollow", rarity="legendary",
    ),
    RandomEvent(
        id="memory_of_the_surface",
        name="Memory of the Surface",
        description=(
            "For a moment, you remember what sunlight feels like. The warmth. The color. It hurts more than you expected.",
            "Something in the air brings back the feeling of a summer afternoon. You stop digging. Your chest aches for reasons that have nothing to do with the tunnel.",
            "Sunlight. Grass. The smell of a road in July. You haven't thought about any of it in a long time. The memory is sharp and cruel.",
        ),
        min_depth=276, max_depth=None,
        safe_option=EventChoice(
            "Let it pass",
            success=EventOutcome("You dig with renewed purpose. Or possibly spite. Hard to tell the difference down here.", 0, 10, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Hold onto the memory",
            success=EventOutcome("The light stays a little longer. Your pickaxe feels lighter.", 3, 16, False),
            failure=EventOutcome("The memory dissolves. You feel emptier than before.", -4, 0, False),
            success_chance=0.45,
        ),
        complexity="simple", layer="The Hollow", rarity="uncommon",
    ),

    # ===================================================================
    # EXPANSION EVENTS — Darkness-Only (require Pitch Black luminosity)
    # ===================================================================

    RandomEvent(
        id="things_in_the_dark",
        name="Things in the Dark",
        description=(
            "You can't see what's touching your shoulder. It's warm. It might be friendly. It might not.",
            "Something has its hand on your shoulder. You cannot see it. It is breathing, slowly, by your ear.",
            "A pressure on your back. A warmth. It has been there for a few seconds before you noticed. The dark is too thick for your lantern.",
        ),
        min_depth=76, max_depth=None,
        safe_option=EventChoice(
            "Stand very still",
            success=EventOutcome("It leaves. You'll never know what it was. Maybe that's better.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Reach out",
            success=EventOutcome("It was trying to give you something. In the dark. Normally, somehow.", 0, 20, False),
            failure=EventOutcome("It wasn't friendly.", -8, -9, True),
            success_chance=0.50,
        ),
        requires_dark=True,
    ),
    RandomEvent(
        id="the_lightless_path",
        name="The Lightless Path",
        description=(
            "In total darkness, your other senses sharpen. You can hear a path the light would never reveal.",
            "Without light, your ears start to compensate. You can hear a passage opening somewhere to your left. Air moving. A distant sound of water.",
            "The darkness sharpens your other senses. There is a route here you could never have found with a lantern.",
        ),
        min_depth=76, max_depth=None,
        safe_option=EventChoice(
            "Stay put",
            success=EventOutcome("The sound fades. You wait for the light to return.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Follow the sound",
            success=EventOutcome("The path opens into a chamber. You don't need light to feel the treasure.", 5, 10, False),
            failure=EventOutcome("You followed the wrong sound. Deeper darkness.", -6, -6, False),
            success_chance=0.45,
        ),
        complexity="complex", requires_dark=True,
    ),
    RandomEvent(
        id="phosphor_vein",
        name="Phosphor Vein",
        description=(
            "Your pickaxe strikes something that glows. A vein of phosphorescent mineral splits the darkness.",
            "You swing blind and hit something that lights up. A line of glowing mineral runs through the wall, and the tunnel starts to exist again.",
            "A single strike and the wall begins to glow. Phosphor. A whole vein of it. The dark pulls back one step.",
        ),
        min_depth=76, max_depth=None,
        safe_option=EventChoice(
            "Mine the vein",
            success=EventOutcome("Light floods back. The darkness retreats. Luminosity restored.", 0, 6, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Follow the vein deeper",
            success=EventOutcome("It leads to a mother lode. Light AND wealth.", 3, 12, False),
            failure=EventOutcome("The vein dead-ends. But at least you can see again.", 0, 2, False),
            success_chance=0.55,
        ),
        complexity="simple", requires_dark=True, rarity="uncommon",
    ),

    # ===================================================================
    # EXPANSION EVENTS — Roshan Superboss
    # ===================================================================

    RandomEvent(
        id="roshan_lair",
        name="The Ancient's Lair",
        description=(
            "An ancient pit radiates power that makes your bones vibrate. Something immense stirs in the darkness. It has been here since before the stone was stone.",
            "The tunnel ends at a pit so deep that looking into it makes you nauseous. Something down there is breathing. The walls shake in time with it.",
            "A circular shaft opens in the floor. It does not end. Something enormous coils at the bottom, older than the rock, and it is aware of you.",
        ),
        min_depth=276, max_depth=None,
        safe_option=EventChoice(
            "Flee",
            success=EventOutcome("Discretion is the better part of valor. The pit remembers you left.", 0, 10, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Challenge the ancient",
            success=EventOutcome("Against all odds, the ancient falls. An aegis shard clatters to the ground. The pit is silent.", 10, 50, False),
            failure=EventOutcome("The ancient demonstrates why it has survived since the world was young.", -15, -24, True),
            success_chance=0.20,
        ),
        complexity="complex", layer="The Hollow", rarity="legendary", social=True,
    ),

    # ===================================================================
    # EXPANSION EVENTS — OSRS / General Easter Eggs
    # ===================================================================

    RandomEvent(
        id="buying_gf",
        name="The Persistent Miner",
        description=(
            "A miner is shouting into the void with impressive conviction. 'Buying GF 10k!' The void does not respond.",
            "You hear someone yelling from a side passage. 'BUYING GF 10K!' Over and over. Hope springs eternal.",
            "A lone figure stands in a side chamber yelling into the dark. They have been there a while. They are very committed to the bit.",
        ),
        min_depth=101, max_depth=None,
        safe_option=EventChoice(
            "Walk away slowly",
            success=EventOutcome("The shouting fades behind you. Some battles are unwinnable.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Offer business advice",
            success=EventOutcome("They diversify into gem trading. You get a referral bonus.", 0, 8, False),
            failure=EventOutcome("They counter-offer. You are now somehow the one buying the GF.", 0, -9, False),
            success_chance=0.50,
        ),
        complexity="simple", rarity="uncommon",
    ),
    RandomEvent(
        id="rock_golem_encounter",
        name="Rock Golem",
        description=(
            "A tiny golem made of living rock follows you. It seems to have chosen you specifically. It cannot be discouraged.",
            "A small figure of animate pebbles has imprinted on you. It trots behind you, stone clattering, with absolute devotion.",
            "Something the size of a cat, made of loose rock, has decided you are its favorite person. It will not be talked out of it.",
        ),
        min_depth=50, max_depth=None,
        safe_option=EventChoice(
            "Accept your new companion",
            success=EventOutcome("It settles on your shoulder. It's heavier than it looks.", 0, 4, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Try to feed it",
            success=EventOutcome("It eats a rock and purrs. You didn't know rocks could purr.", 1, 6, False),
            failure=EventOutcome("It bites your hand. Rock teeth. Actual rock teeth.", -2, 0, False),
            success_chance=0.65,
        ),
        complexity="simple", rarity="rare",
    ),

    # ===================================================================
    # PRESTIGE EXPANSION EVENTS
    # ===================================================================

    # --- Minecraft-themed — Dirt/Stone (depth 0-55) ---

    RandomEvent(
        id="creeper_ambush",
        name="Creeper Ambush",
        description=(
            "A familiar hissing sound. The walls flash green. You have seconds.",
            "Ssssssss. A green flash from behind. You know exactly what this is. You have seconds.",
            "Something hisses at you from the dark. The walls pulse green. You recognize the timer.",
        ),
        min_depth=0, max_depth=75,
        safe_option=EventChoice(
            "Sprint back",
            success=EventOutcome("You dive clear as the blast craters the tunnel behind you.", -2, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Shield with pickaxe",
            success=EventOutcome("Your pickaxe absorbs the blast. The debris opens a new passage!", 2, 0, False),
            failure=EventOutcome("The explosion sends you tumbling. Everything goes dark.", -6, 0, True),
            success_chance=0.60,
        ),
        desperate_option=EventChoice(
            "Punch the creeper",
            success=EventOutcome("One punch. The creeper detonates harmlessly. Gunpowder everywhere — and coins!", 6, 8, False),
            failure=EventOutcome("You punch it. It detonates. You are not Steve.", -12, 0, True),
            success_chance=0.15,
        ),
    ),
    RandomEvent(
        id="abandoned_minecart",
        name="Abandoned Minecart",
        description=(
            "A rusted minecart sits on ancient rails. Something rattles inside.",
            "An old wooden minecart rests on tracks that disappear into the dark. Whatever is inside it clinks when the cart shifts.",
            "A minecart abandoned mid-journey. The rails vanish into the tunnel ahead. You can hear something rolling around in the cart.",
        ),
        min_depth=0, max_depth=75,
        safe_option=EventChoice(
            "Search the cart",
            success=EventOutcome("A single coin wedged in the seat. Better than nothing.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Ride the rails",
            success=EventOutcome("WHOOSH! The cart rockets through the darkness and deposits you deeper!", 5, 0, False),
            failure=EventOutcome("The rails end abruptly. You and the cart part ways.", -4, 0, False),
            success_chance=0.50,
        ),
    ),
    RandomEvent(
        id="enchanting_table",
        name="Enchanting Table",
        description=(
            "Purple particles drift from a floating book. Strange symbols orbit the air.",
            "A book hovers at waist height over a stone pedestal. Its pages turn by themselves. Purple motes drift out and fade.",
            "A floating tome, open to a page you can't read. Runes orbit it slowly, trailing violet light.",
        ),
        min_depth=0, max_depth=55,
        safe_option=EventChoice(
            "Take nothing",
            success=EventOutcome("You decline.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Take nothing",
            success=EventOutcome("You decline.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        complexity="boon", rarity="uncommon",
        boon_options=(
            TempBuff("efficiency", "Efficiency", 3, {"advance_bonus": 2}),
            TempBuff("fortune", "Fortune", 3, {"jc_bonus": 3}),
            TempBuff("unbreaking", "Unbreaking", 3, {"cave_in_reduction": 0.15}),
        ),
    ),
    RandomEvent(
        id="villager_trade",
        name="Suspicious Villager",
        description=(
            "Hmm. A villager in a brown robe offers emeralds for... dirt blocks?",
            "A big-nosed fellow in a brown robe holds out a handful of emeralds. He wants some of your dirt. He will not explain.",
            "A villager blocks your path, grinning. 'Hmmm!' He gestures at a small pile of emeralds. The trade terms are written in a dialect only he speaks.",
        ),
        min_depth=0, max_depth=80,
        safe_option=EventChoice(
            "Trade politely",
            success=EventOutcome("A fair deal. The villager nods approvingly. 'Hmm.'", 0, 4, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Haggle aggressively",
            success=EventOutcome("The villager sweats. You drive a hard bargain. Emeralds rain.", 0, 10, False),
            failure=EventOutcome("The villager slams the trading window shut. You lose your deposit.", 0, -6, False),
            success_chance=0.45,
        ),
    ),
    RandomEvent(
        id="enderman_stare",
        name="The Staring Contest",
        description=(
            "A tall dark figure stands motionless. Purple particles shimmer. It's watching.",
            "In the corner of the tunnel, a tall, thin figure stands perfectly still. Purple sparks drift off its shoulders. Its eyes are on you.",
            "Something three meters tall, entirely black, perfectly still. The only motion is the slow drift of violet particles around its head.",
        ),
        min_depth=0, max_depth=80,
        safe_option=EventChoice(
            "Look away slowly",
            success=EventOutcome("It teleports away. You exhale.", 1, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Stare back",
            success=EventOutcome("It flinches first. It drops something valuable before vanishing.", 0, 8, False),
            failure=EventOutcome("It shrieks. Reality bends. You are somewhere else now.", -8, 0, False),
            success_chance=0.40,
        ),
        desperate_option=EventChoice(
            "Charge it",
            success=EventOutcome("You tackle an Enderman. It drops a pearl and its dignity.", 3, 16, False),
            failure=EventOutcome("It teleports behind you. 'Nothing personnel, kid.' Cave-in.", -9, 0, True),
            success_chance=0.12,
        ),
        rarity="uncommon",
    ),
    RandomEvent(
        id="mob_spawner",
        name="Mob Spawner",
        description=(
            "A cage spins in the corner, spawning skeletal shapes. Treasure chests flank it.",
            "A wireframe cube in the middle of the chamber whirls, spitting out small skeletal figures one at a time. On either side, chests.",
            "Bones rise out of a rotating cage. Each time it spins, another skeleton. The two chests at its base look extremely full.",
        ),
        min_depth=0, max_depth=75,
        safe_option=EventChoice(
            "Sneak past",
            success=EventOutcome("You slip through unnoticed. The skeletons argue among themselves.", 1, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Grab a chest and run",
            success=EventOutcome("You snatch a chest. Arrows whiz past your ears. Worth it.", 0, 8, False),
            failure=EventOutcome("An arrow finds your knee. The adventuring is over for today.", -4, 0, False),
            success_chance=0.50,
        ),
        desperate_option=EventChoice(
            "Destroy the spawner",
            success=EventOutcome("The cage shatters! Both chests are yours. The skeletons dissolve.", 2, 12, False),
            failure=EventOutcome("The spawner surges. More skeletons than you can count.", -9, 0, True),
            success_chance=0.20,
        ),
        rarity="uncommon",
        buff_on_success=TempBuff("mob_farm", "Mob Farm", 2, {"jc_bonus": 2}),
    ),
    RandomEvent(
        id="witch_cauldron",
        name="Witch's Cauldron",
        description=(
            "A bubbling cauldron sits unattended. Three potion bottles labeled in illegible handwriting.",
            "A black cauldron gurgles over a fire that was not lit by you. Three bottles rest on the rim. The labels are smeared.",
            "Someone's cauldron. No one's witch. The brew smells like rust and strawberries. Three stoppered potions sit near the rim.",
        ),
        min_depth=0, max_depth=75,
        safe_option=EventChoice(
            "Take nothing",
            success=EventOutcome("You decline.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Take nothing",
            success=EventOutcome("You decline.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        complexity="boon",
        boon_options=(
            TempBuff("red_potion", "Red Potion", 1, {"advance_bonus": 3}),
            TempBuff("blue_potion", "Blue Potion", 2, {"cave_in_reduction": 0.20}),
            TempBuff("green_potion", "Green Potion", 1, {"jc_bonus": 5}),
        ),
    ),

    # --- PoE Delve-themed — Crystal/Abyss (depth 51-150) ---

    RandomEvent(
        id="azurite_deposit",
        name="Azurite Deposit",
        description=(
            "A vein of deep blue crystal hums with stored energy. Cartographers would kill for this.",
            "A seam of blue crystal runs diagonally across the tunnel wall. You can feel the charge in it from a meter away.",
            "Azurite. An entire vein of it, humming. People go to war for less.",
        ),
        min_depth=40, max_depth=120,
        safe_option=EventChoice(
            "Mine carefully",
            success=EventOutcome("You chip out a few clean crystals. Respectable haul.", 0, 4, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Blast it open",
            success=EventOutcome("The vein cracks wide. Azurite shards scatter like blue rain!", 0, 12, False),
            failure=EventOutcome("The blast destabilizes the passage. Crystals shatter worthlessly.", -4, 0, False),
            success_chance=0.45,
        ),
    ),
    RandomEvent(
        id="crawler_breakdown",
        name="Crawler Breakdown",
        description=(
            "Your mining rig shudders and sparks. The darkness closes in around you.",
            "The mining crawler coughs, sparks, and dies. Its lantern rig sputters. The tunnel gets a lot bigger without the light.",
            "Your rig has picked a terrible time to break down. Something in the engine is ticking. Something behind you is not.",
        ),
        min_depth=40, max_depth=120,
        safe_option=EventChoice(
            "Repair with spare parts",
            success=EventOutcome("A patch job. It'll hold. You lost some parts though.", 1, -6, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Hotwire it",
            success=EventOutcome("Sparks fly — then the engine roars. It's running better than before!", 4, 0, False),
            failure=EventOutcome("The rig catches fire. You abandon the burning wreck.", -6, 0, False),
            success_chance=0.50,
        ),
        desperate_option=EventChoice(
            "Abandon rig and sprint",
            success=EventOutcome("You outrun the darkness. Barely. Your legs will remember this.", 7, 0, False),
            failure=EventOutcome("The darkness catches you. Something in it has claws.", -9, 0, True),
            success_chance=0.15,
        ),
        rarity="uncommon",
    ),
    RandomEvent(
        id="fossil_cache",
        name="Fossil Cache",
        description=(
            "Fossilized remains embedded in crystal. They could be valuable... or fragile.",
            "Something small and ancient is suspended in a chunk of clear crystal. The outlines are perfect. Too perfect to be safe.",
            "A cluster of fossils encased in crystal like insects in amber. Intact. Priceless. And very, very old.",
        ),
        min_depth=40, max_depth=120,
        safe_option=EventChoice(
            "Photograph and catalog",
            success=EventOutcome("Careful documentation. A small finder's fee.", 1, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Extract fossils",
            success=EventOutcome("Pristine specimens! Collectors will pay handsomely.", 0, 8, False),
            failure=EventOutcome("They crumble to dust the moment you touch them. Ancient and fragile.", -4, 0, False),
            success_chance=0.55,
        ),
    ),
    RandomEvent(
        id="breach_encounter",
        name="Breach Encounter",
        description=(
            "A hand-shaped tear in reality splits open. Things pour through. Purple. Endless. Hungry.",
            "The air in front of you splits like a seam. A purple hand, too large, emerges. Then another. Then more.",
            "Reality bends, and then breaks. A tear in the shape of a grasping hand opens, and things come through. They keep coming through.",
        ),
        min_depth=40, max_depth=170,
        safe_option=EventChoice(
            "Retreat and seal",
            success=EventOutcome("You back away. The breach closes. Wise.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Hold the line",
            success=EventOutcome("You stand your ground. The breach yields treasure as it closes.", 2, 10, False),
            failure=EventOutcome("The breach consumes ground you'd already claimed.", -8, 0, False),
            success_chance=0.40,
        ),
        desperate_option=EventChoice(
            "Dive into the breach",
            success=EventOutcome("You pass through and emerge deeper. Much deeper. With loot.", 5, 24, False),
            failure=EventOutcome("The breach spits you out. Hard. Very, very hard.", -12, 0, True),
            success_chance=0.10,
        ),
        rarity="rare",
    ),
    RandomEvent(
        id="vaal_side_area",
        name="Vaal Side Area",
        description=(
            "A blood-red door pulses in the crystal. Corrupted inscriptions promise treasure and death.",
            "A door set into the crystal wall throbs red, faintly, in time with your heartbeat. Its inscriptions are in a dead tongue, and they make promises.",
            "The wall hides a door. The door hides an altar. The altar hides something that was once powerful and is now only hungry.",
        ),
        min_depth=40, max_depth=170,
        safe_option=EventChoice(
            "Walk past",
            success=EventOutcome("Discretion. Valor. You keep walking.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Enter cautiously",
            success=EventOutcome("The corruption parts for you. Ancient Vaal gold awaits.", 0, 12, False),
            failure=EventOutcome("The corruption bites. Your coin pouch feels lighter.", 0, -12, False),
            success_chance=0.40,
        ),
        desperate_option=EventChoice(
            "Smash the corruption altar",
            success=EventOutcome("The altar shatters! A flood of uncorrupted treasure!", 4, 20, False),
            failure=EventOutcome("The altar explodes. Corruption everywhere. In you. On you.", -10, 0, True),
            success_chance=0.15,
        ),
        rarity="rare",
    ),
    RandomEvent(
        id="syndicate_ambush",
        name="Syndicate Ambush",
        description=(
            "Cloaked figures drop from the ceiling. 'Your tunnel or your coins.'",
            "Three cloaked figures land silently in front of you. One draws a crooked dagger. 'You know the deal.'",
            "A syndicate crew drops from the ceiling in perfect silence. They do not introduce themselves. They don't need to.",
        ),
        min_depth=40, max_depth=170,
        safe_option=EventChoice(
            "Pay the toll",
            success=EventOutcome("They take your coins and let you pass. Professional thieves.", 2, -9, False),
            failure=EventOutcome("They take your coins AND push you back. Unprofessional thieves.", 0, -15, False),
            success_chance=0.90,
        ),
        risky_option=EventChoice(
            "Fight back",
            success=EventOutcome("You scatter the syndicate. They drop their own loot!", 0, 10, False),
            failure=EventOutcome("They outnumber you. The beating is educational.", -6, 0, False),
            success_chance=0.45,
        ),
        rarity="uncommon",
    ),
    RandomEvent(
        id="delve_smuggler",
        name="Delve Smuggler",
        description=(
            "A figure from a hidden passage. 'Psst. Got the good stuff. Fell off a crawler, innit.'",
            "A hand beckons from a hidden side passage. 'Oi. Over 'ere. Got stuff, don't ask where from.'",
            "A grinning smuggler leans out of a crack in the wall, wares spread on a dirty cloth. 'All genuine. All completely legal. Mostly.'",
        ),
        min_depth=40, max_depth=170,
        safe_option=EventChoice(
            "Take nothing",
            success=EventOutcome("You decline.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Take nothing",
            success=EventOutcome("You decline.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        complexity="boon", rarity="uncommon",
        boon_options=(
            TempBuff("volatile_flare", "Volatile Flare", 1, {"advance_bonus": 4}),
            TempBuff("azurite_tonic", "Azurite Tonic", 1, {"luminosity_restore": 30}),
            TempBuff("smugglers_map", "Smuggler's Map", 2, {"advance_bonus": 2}),
        ),
    ),

    # --- WoW Delves-themed — Fungal/Frozen (depth 151-275+) ---

    RandomEvent(
        id="brann_bronzebeard",
        name="Brann Bronzebeard",
        description=(
            "'Well met, adventurer! Brann Bronzebeard, at yer service. Been mapping these depths for decades.'",
            "A ruddy dwarf looks up from a journal and beams at you. 'Well met! Down to explore? Aye, I could use the company.'",
            "A bearded figure in explorer's gear waves from a small camp. 'Ah, a fellow delver! Share a fire and I'll share a trick or two.'",
        ),
        min_depth=130, max_depth=290,
        safe_option=EventChoice(
            "Take nothing",
            success=EventOutcome("You decline.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Take nothing",
            success=EventOutcome("You decline.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        complexity="boon", rarity="uncommon",
        boon_options=(
            TempBuff("combat_buddy", "Combat Buddy", 3, {"advance_bonus": 2}),
            TempBuff("treasure_finder", "Treasure Finder", 3, {"jc_bonus": 2}),
            TempBuff("veteran_guidance", "Veteran Guidance", 3, {"cave_in_reduction": 0.10}),
        ),
    ),
    RandomEvent(
        id="earthen_cache",
        name="Earthen Cache",
        description=(
            "A stone chest sealed with dwarven runes. The lock is complex but cracked with age.",
            "A heavy stone chest rests in an alcove, bound with dwarven ironwork. The runes on the lid are mostly faded.",
            "A runebound chest the color of mountains. The seals have been broken and rewritten many times. They are weakest now.",
        ),
        min_depth=130, max_depth=290,
        safe_option=EventChoice(
            "Force open carefully",
            success=EventOutcome("The lock gives way. Modest treasure inside.", 0, 4, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Smash it",
            success=EventOutcome("The chest shatters. Dwarven gold scatters everywhere!", 0, 10, False),
            failure=EventOutcome("The runes were a trap. The blast sends you reeling.", -4, 0, False),
            success_chance=0.50,
        ),
    ),
    RandomEvent(
        id="campfire_rest",
        name="Campfire Rest",
        description=(
            "A warm campfire glows in an alcove. A bedroll and whetstone sit nearby.",
            "Someone has left a campfire burning in a sheltered alcove. A neat bedroll. A whetstone on a flat rock. An invitation.",
            "A small fire crackles in a pit, warming a space someone else clearly used as camp. Everything needed for a short rest is laid out.",
        ),
        min_depth=130, max_depth=300,
        safe_option=EventChoice(
            "Rest by the fire",
            success=EventOutcome("You sit. The warmth seeps into your bones. A moment of peace.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Sharpen your pickaxe",
            success=EventOutcome("The whetstone sings. Your pickaxe has never been sharper.", 0, 0, False),
            failure=EventOutcome("You slip. A cut on the hand. It'll heal.", 0, 0, False),
            success_chance=0.65,
        ),
        buff_on_success=TempBuff("honed_edge", "Honed Edge", 2, {"advance_bonus": 2}),
    ),
    RandomEvent(
        id="zekvir_shadow",
        name="Zekvir's Shadow",
        description=(
            "A massive spider silhouette blocks the tunnel. Eight eyes gleam red.",
            "The tunnel ends in legs — eight of them, too long, folded against a body the size of a house. Eight red pinpricks focus on you.",
            "A shape the size of a barn occupies the chamber ahead. Only the eyes move. They move to you.",
        ),
        min_depth=130, max_depth=290,
        safe_option=EventChoice(
            "Find another way",
            success=EventOutcome("A detour. Longer, but you keep your limbs.", -2, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Distract and sneak past",
            success=EventOutcome("You toss a rock. It turns. You sprint. Heart hammering.", 3, 0, False),
            failure=EventOutcome("Eight eyes. All looking at you. It pounces.", -8, 0, False),
            success_chance=0.35,
        ),
        desperate_option=EventChoice(
            "Fight Zekvir",
            success=EventOutcome("Against all odds, you drive it back! Legendary loot drops!", 5, 20, False),
            failure=EventOutcome("Zekvir wraps you in silk. The ceiling collapses.", -12, 0, True),
            success_chance=0.12,
        ),
        rarity="rare",
    ),
    RandomEvent(
        id="dark_rider",
        name="The Dark Rider",
        description=(
            "A cloaked rider on a skeletal mount emerges from fungal mist. It extends a gauntlet.",
            "Out of the spore-mist rides a figure on a horse made of bones. The rider is cloaked. The rider extends a gauntlet.",
            "A horseman parts the fungal fog. Both rider and mount are skeletal. A gauntleted hand rises in a greeting, or a demand.",
        ),
        min_depth=130, max_depth=290,
        safe_option=EventChoice(
            "Bow and let it pass",
            success=EventOutcome("It drops a coin as tribute. Respect, it seems, has value.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Accept the handshake",
            success=EventOutcome("A cold grip. Knowledge flows into you. Secrets of the deep.", 0, 10, False),
            failure=EventOutcome("The grip tightens. It takes something from you.", 0, -9, False),
            success_chance=0.45,
        ),
        desperate_option=EventChoice(
            "Challenge the rider",
            success=EventOutcome("Your pickaxe meets spectral steel. You win. The mount is yours — briefly.", 3, 16, False),
            failure=EventOutcome("The rider's lance finds its mark. You crumple.", -9, 0, False),
            success_chance=0.18,
        ),
        rarity="uncommon",
    ),
    RandomEvent(
        id="titan_relic",
        name="Titan Relic",
        description=(
            "An ancient device of impossible complexity. Three runes glow: red, blue, gold.",
            "A relic built by hands that were not human sits on a plinth. Three runes orbit it slowly. You are invited to touch one.",
            "A device older than the rock around it hums at chest height. It offers three glowing options. You do not know what they mean.",
        ),
        min_depth=130, max_depth=290,
        safe_option=EventChoice(
            "Take nothing",
            success=EventOutcome("You decline.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Take nothing",
            success=EventOutcome("You decline.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        complexity="boon", rarity="rare",
        boon_options=(
            TempBuff("power_rune", "Power", 3, {"advance_bonus": 3}),
            TempBuff("protection_rune", "Protection", 5, {"cave_in_reduction": 0.20}),
            TempBuff("fortune_rune", "Fortune", 3, {"jc_bonus": 3}),
        ),
    ),
    RandomEvent(
        id="candle_glow",
        name="The Candle",
        description=(
            "A single candle burns impossibly in the frozen dark. Its light is warm. It shouldn't be here.",
            "In the middle of the ice, a single candle burns. No drafts. No breath. No reason. Its light reaches further than it should.",
            "A candle sits on a simple iron holder in the frozen dark. Its flame is steady. Its warmth is real. Its presence makes no sense.",
        ),
        min_depth=130, max_depth=290,
        safe_option=EventChoice(
            "Light your torch from it",
            success=EventOutcome("The flame passes to your torch. The candle keeps burning.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Take the candle",
            success=EventOutcome("It comes free. The warmth follows you. Two coins fall from the wax.", 0, 4, False),
            failure=EventOutcome("It won't move. Wax drips on your hand. The flame flickers reproachfully.", 0, 0, False),
            success_chance=0.55,
        ),
    ),

    # --- Hades-themed — Deep (depth 100+) ---

    RandomEvent(
        id="olympian_boon",
        name="Olympian Boon",
        description=(
            "A divine light pierces the darkness. 'Choose wisely, mortal. My gift is not given lightly.'",
            "A column of golden light punches down through the stone ceiling. Inside it, the silhouette of a god. A voice you cannot refuse says: 'Pick one.'",
            "The air fills with the scent of ozone and olive oil. Something vast and divine is paying attention to you specifically. It has a gift.",
        ),
        min_depth=100, max_depth=None,
        safe_option=EventChoice(
            "Take nothing",
            success=EventOutcome("You decline.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Take nothing",
            success=EventOutcome("You decline.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        complexity="boon", rarity="uncommon",
        boon_options=(
            TempBuff("zeus_fury", "Zeus's Fury", 2, {"advance_bonus": 4}),
            TempBuff("athena_shield", "Athena's Shield", 1, {"cave_in_reduction": 1.0}),
            TempBuff("hermes_speed", "Hermes's Speed", 1, {"cooldown_reduction": 0.50}),
        ),
    ),
    RandomEvent(
        id="charon_toll",
        name="Charon's Toll",
        description=(
            "A boat drifts on an underground river. The ferryman extends a bony hand.",
            "A skiff glides silently along a black river. At its prow, a robed figure holds out a skeletal palm, waiting.",
            "The river here is still and the ferryman is silent. The price is the price. It has always been the price.",
        ),
        min_depth=100, max_depth=None,
        safe_option=EventChoice(
            "Pay the toll",
            success=EventOutcome("Coins clink into skeletal fingers. The boat glides forward.", 3, -12, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Negotiate",
            success=EventOutcome("The ferryman tilts his head. 'Amusing.' He lets you ride free.", 3, 0, False),
            failure=EventOutcome("'No.' The boat drifts away. You wasted your time.", 0, -6, False),
            success_chance=0.40,
        ),
        desperate_option=EventChoice(
            "Steal the oar",
            success=EventOutcome("You yank the oar and paddle furiously. Charon watches, impressed.", 5, 4, False),
            failure=EventOutcome("Charon capsizes the boat. You sink. You surface somewhere worse.", -9, 0, True),
            success_chance=0.15,
        ),
        rarity="uncommon",
    ),
    RandomEvent(
        id="sisyphus_boulder",
        name="Sisyphus's Request",
        description=(
            "A man pushes an enormous boulder uphill. 'Help me,' he wheezes. 'Just one more push.'",
            "A lean, exhausted man leans against an enormous round stone, straining to roll it up a slope. He turns his head. 'Help. Please. Just this once.'",
            "A figure pushes a boulder up an incline that is somehow both a corridor and a hill. He has been doing this for a long time. His eyes are very tired.",
        ),
        min_depth=100, max_depth=None,
        safe_option=EventChoice(
            "Help push",
            success=EventOutcome("Together, you move it an inch. He smiles. 'That's enough.' He tips you.", 1, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Loot his camp",
            success=EventOutcome("While he pushes, you find his stash. He won't notice for eternity.", 0, 8, False),
            failure=EventOutcome("The boulder rolls back — onto your foot.", -4, 0, False),
            success_chance=0.55,
        ),
    ),
    RandomEvent(
        id="infernal_gate",
        name="Infernal Gate",
        description=(
            "A gate of black iron sealed with chains. Beyond: combat sounds and clinking gold.",
            "A massive iron gate wrapped in chains. Through the bars: the sound of a battle and the unmistakable clink of spilled coin.",
            "An infernal gate blocks the passage, chained shut from the far side. Beyond it, you can hear fighting, and gold, and something laughing.",
        ),
        min_depth=100, max_depth=None,
        safe_option=EventChoice(
            "Leave it sealed",
            success=EventOutcome("Some doors are best left closed.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Open carefully",
            success=EventOutcome("The chains fall. Gold spills through the gap.", 0, 14, False),
            failure=EventOutcome("Something reaches through the gap. It pulls you in.", -8, 0, True),
            success_chance=0.30,
        ),
        desperate_option=EventChoice(
            "Shatter the chains",
            success=EventOutcome("The gate flies open. An arena of riches, and you've already won.", 4, 24, False),
            failure=EventOutcome("The gate opens both ways. What was inside is now outside. With you.", -12, 0, True),
            success_chance=0.12,
        ),
        rarity="rare",
    ),

    # --- Dota 2-themed — All depths ---

    RandomEvent(
        id="riki_ambush",
        name="Riki's Ambush",
        description=(
            "You feel watched. A shimmer in the air. Suddenly — a blade at your throat. 'Surprise.'",
            "The air behind you displaces. Something cold touches your neck. 'Hello,' says a voice you cannot see.",
            "A rustle. A shadow that isn't yours. Then a blade, polite and patient, resting against your throat. 'Don't turn around.'",
        ),
        min_depth=None, max_depth=None,
        safe_option=EventChoice(
            "Drop coins and run",
            success=EventOutcome("He takes the coins and vanishes. At least you're alive.", 0, -6, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Throw dust to reveal him",
            success=EventOutcome("Dust of Appearance! Riki panics. He drops his stash.", 0, 8, False),
            failure=EventOutcome("He blinks behind you. Classic Riki.", -6, 0, False),
            success_chance=0.50,
        ),
        desperate_option=EventChoice(
            "Grab the blade barehanded",
            success=EventOutcome("You catch the blade and twist. Riki squeaks and flees, leaving everything.", 2, 14, False),
            failure=EventOutcome("Blades are sharp. This is not a revelation. It is a reminder.", -9, 0, False),
            success_chance=0.15,
        ),
        rarity="uncommon",
    ),
    RandomEvent(
        id="bounty_rune",
        name="Bounty Rune",
        description=(
            "A golden rune hovers in a side chamber, spinning gently. It pulses with energy.",
            "A rune spins in a pocket of open air, throwing off flecks of gold. It hums, patient, on the verge of paying out.",
            "In the middle of a small side chamber, a bounty rune rotates slowly, dripping light. It's been there a while. It's ready.",
        ),
        min_depth=None, max_depth=None,
        safe_option=EventChoice(
            "Grab it",
            success=EventOutcome("Gold flows into your pockets. Easy money.", 0, 6, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Wait for it to multiply",
            success=EventOutcome("It splits into three! Triple bounty!", 0, 16, False),
            failure=EventOutcome("It fizzles. You get the scraps.", 0, 2, False),
            success_chance=0.45,
        ),
    ),
    RandomEvent(
        id="aghanim_trial",
        name="The Arcane Trial",
        description=(
            "An old wizard materializes in a flash of arcane energy. 'Another one. Let's see what you're made of.'",
            "A flash of arcane light and a robed figure with a deep hood steps into the tunnel. 'So,' the wizard says, without enthusiasm. 'Another applicant.'",
            "The passage fills with light and the smell of burnt ozone. An old wizard crosses his arms and sighs. 'Let's get this over with.'",
        ),
        min_depth=None, max_depth=None,
        safe_option=EventChoice(
            "Decline the trial",
            success=EventOutcome("The wizard shrugs. 'Your loss.' He vanishes in a puff of condescension.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Accept the trial",
            success=EventOutcome("You survive the gauntlet. He slow-claps. 'Not bad.'", 3, 12, False),
            failure=EventOutcome("The trial overwhelms you. The wizard yawns.", -6, 0, False),
            success_chance=0.35,
        ),
        desperate_option=EventChoice(
            "Demand the scepter",
            success=EventOutcome("'Bold.' He hands it over. Power surges through you.", 5, 20, False),
            failure=EventOutcome("'Greedy.' He snaps his fingers. You are elsewhere.", -12, 0, True),
            success_chance=0.10,
        ),
        rarity="rare",
        buff_on_success=TempBuff("aghanim_blessing", "Arcane Blessing", 2, {"advance_bonus": 3, "jc_bonus": 2}),
    ),
    RandomEvent(
        id="tormentor_encounter",
        name="Tormentor",
        description=(
            "A colossal stone figure blocks the path. Lightning crackles across its surface. It does not move. Yet.",
            "Something taller than the ceiling, shaped like a man and built of basalt, stands motionless in the passage. Arcs of lightning run along its joints.",
            "A Tormentor waits in the middle of the chamber. It has been waiting for an age. The lightning in its seams says it is almost ready to stop waiting.",
        ),
        min_depth=75, max_depth=None,
        safe_option=EventChoice(
            "Take the long way around",
            success=EventOutcome("You give it a wide berth. It watches you leave.", -3, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Attack while dormant",
            success=EventOutcome("Critical hit! The Tormentor crumbles. Its core is priceless.", 3, 16, False),
            failure=EventOutcome("It wakes up. Lightning everywhere. You regret this.", -9, 0, True),
            success_chance=0.30,
        ),
        desperate_option=EventChoice(
            "Channel everything",
            success=EventOutcome("You pour everything into one strike. The Tormentor EXPLODES.", 5, 30, False),
            failure=EventOutcome("It absorbs your energy and uses it against you. Badly.", -15, 0, True),
            success_chance=0.12,
        ),
        rarity="rare",
    ),
    RandomEvent(
        id="neutral_item_drop",
        name="Neutral Item",
        description=(
            "A defeated jungle creep lies in the corner. Something glints in the debris.",
            "A slain creature sprawls against the tunnel wall, clearly not killed by you. Something catches the light in the mess around it.",
            "A dead neutral lies near the wall. Whatever killed it left the body. In the debris, something valuable peeks out.",
        ),
        min_depth=None, max_depth=None,
        safe_option=EventChoice(
            "Take nothing",
            success=EventOutcome("You decline.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Take nothing",
            success=EventOutcome("You decline.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        complexity="boon", rarity="uncommon",
        boon_options=(
            TempBuff("trusty_shovel", "Trusty Shovel", 3, {"advance_bonus": 2}),
            TempBuff("philosophers_stone", "Philosopher's Stone", 3, {"jc_bonus": 2}),
            TempBuff("spark_of_courage", "Spark of Courage", 2, {"cave_in_reduction": 0.10, "advance_bonus": 1}),
        ),
    ),

    # --- BGO Chaos — All depths ---

    RandomEvent(
        id="gambling_den",
        name="The Gambling Den",
        description=(
            "A cave lit by guttering candles. Three goblins run a card game. 'Sit down,' one grins.",
            "In a candlelit alcove, three goblins hunch over a card table. One glances up and kicks out an empty stool with his foot. 'Fresh blood. Sit.'",
            "A gambling den tucked into a side passage. Three green-skinned dealers, a deck of cards, and a single empty seat.",
        ),
        min_depth=None, max_depth=None,
        safe_option=EventChoice(
            "Watch a round",
            success=EventOutcome("You observe. One goblin slips you a coin for the audience.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Play a hand",
            success=EventOutcome("Full house! The goblins pay up, grudgingly.", 0, 10, False),
            failure=EventOutcome("The goblins cheat. Of course they cheat.", 0, -12, False),
            success_chance=0.45,
        ),
        desperate_option=EventChoice(
            "Go all in",
            success=EventOutcome("You stare down three goblins and win. They flip the table.", 0, 30, False),
            failure=EventOutcome("You lose everything. A goblin pats your shoulder. 'Better luck next time.'", 0, -24, False),
            success_chance=0.15,
        ),
        rarity="uncommon",
    ),
    RandomEvent(
        id="item_goblin",
        name="Item Goblin",
        description=(
            "A small green creature sprints past carrying a bulging sack. Gold coins scatter.",
            "Something small and green hurtles past you at knee height, clutching a sack twice its size. Coins trail behind it in an arc.",
            "A goblin pelts down the tunnel on short legs, dragging a bulging loot bag. Gold leaks from a hole in the bottom.",
        ),
        min_depth=None, max_depth=None,
        safe_option=EventChoice(
            "Pick up dropped coins",
            success=EventOutcome("Free money on the ground. No complaints.", 0, 4, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Chase it",
            success=EventOutcome("You corner it! The goblin surrenders the sack.", 0, 10, False),
            failure=EventOutcome("It's faster than it looks. You trip on a stalagmite.", -3, 0, False),
            success_chance=0.50,
        ),
        desperate_option=EventChoice(
            "Flying tackle",
            success=EventOutcome("WHAM! You and the goblin slide across the cave floor. The sack is yours!", 0, 16, False),
            failure=EventOutcome("You miss. You slide into a wall. The goblin laughs.", -4, -6, False),
            success_chance=0.20,
        ),
        rarity="uncommon",
    ),
    RandomEvent(
        id="mystery_lever",
        name="The Mystery Lever",
        description=(
            "A lever protrudes from the wall. It's labeled 'PULL ME' in crayon. This can't end well.",
            "A wooden lever sticks out of the tunnel wall. Above it, in red crayon: 'PULL ME'. Below it, a crude smiley. Everything about this is a warning.",
            "Someone has installed a lever in the middle of nowhere and labeled it, with great enthusiasm, 'PULL ME'. You consider your options.",
        ),
        min_depth=None, max_depth=None,
        safe_option=EventChoice(
            "Pull it cautiously",
            success=EventOutcome("A hidden panel opens. Coins spill out. Crayon was right!", 0, 6, False),
            failure=EventOutcome("A bucket of water falls on your head. A sign drops: 'GOTCHA'. One coin as consolation.", 0, -3, False),
            success_chance=0.80,
        ),
        risky_option=EventChoice(
            "Yank it hard",
            success=EventOutcome("The wall rotates! A secret passage opens, blasting you forward!", 6, 0, False),
            failure=EventOutcome("The lever breaks. The ceiling groans. Run.", -8, 0, False),
            success_chance=0.40,
        ),
    ),
    RandomEvent(
        id="identity_thief",
        name="The Identity Thief",
        description=(
            "A shapeshifter has stolen your face. It's telling other miners YOUR tunnel is abandoned.",
            "You round a corner and see yourself, talking to two other miners, gesturing at your tunnel. Your double is in the middle of saying it is abandoned and free for the taking.",
            "Something has taken your face. It is wearing it down the passage and confidently lying about you.",
        ),
        min_depth=None, max_depth=None,
        safe_option=EventChoice(
            "Report it to authorities",
            success=EventOutcome("Bureaucracy wins. The shapeshifter is fined. You get a small settlement.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Confront the imposter",
            success=EventOutcome("'Which one is the real one?' You prove it. The crowd sides with you.", 0, 8, False),
            failure=EventOutcome("The crowd picks the other one. You are escorted away from your own tunnel.", 0, -9, False),
            success_chance=0.50,
        ),
        rarity="uncommon", social=True,
    ),
    RandomEvent(
        id="neow_blessing",
        name="Ancient Blessing",
        description=(
            "A voice older than stone: 'You have walked this path before. I remember. Choose your gift.'",
            "A presence that is not a person speaks to you from every direction at once. 'Old friend. You always come back. Choose.'",
            "The air hums with attention. A voice, patient and ancient, says: 'I remember you. Take what you need.'",
        ),
        min_depth=None, max_depth=None,
        safe_option=EventChoice(
            "Take nothing",
            success=EventOutcome("You decline.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Take nothing",
            success=EventOutcome("You decline.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        complexity="boon", rarity="legendary",
        boon_options=(
            TempBuff("blessing_depth", "Blessing of Depth", 5, {"advance_bonus": 3}),
            TempBuff("blessing_fortune", "Blessing of Fortune", 5, {"jc_bonus": 5}),
            TempBuff("blessing_iron", "Blessing of Iron", 5, {"cave_in_reduction": 0.25}),
        ),
    ),
    # ---- Negative-EV "trap" events (rare). Safe option is always weakly
    # positive so declining is rational; the risky option averages below zero
    # to punish FOMO plays and drain jopacoin out of circulation. -----------
    RandomEvent(
        id="fools_vein",
        name="Fool's Vein",
        description=(
            "A vein of glittering ore runs along the wall. Half of it looks like pyrite. The other half might be real.",
            "The wall sparkles impossibly. Some of it is gold. Some of it is painted rock. You can't tell which.",
            "An offer so good it has to be a trick — or does it? The ore shines back at you, unreadable.",
        ),
        min_depth=10, max_depth=None,
        safe_option=EventChoice(
            "Pocket a sample and move on",
            success=EventOutcome("A steady hand keeps a steady coin. You take the small find.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Pry the whole vein loose",
            success=EventOutcome("The vein is real! A jackpot rattles into your pack.", 0, 24, False),
            failure=EventOutcome("Pyrite. Worthless. Dust rains down and so do a few of your coins.", 0, -12, False),
            success_chance=0.20,
        ),
        rarity="rare",
    ),
    RandomEvent(
        id="sirens_hollow",
        name="Siren's Hollow",
        description=(
            "A low, sweet hum drifts out of a fissure. Something is singing. The sound promises gold.",
            "The stones vibrate with a melody that feels like it's shaping itself to what you want.",
            "A voice behind the magma whispers a price, then offers to forget the price.",
        ),
        min_depth=76, max_depth=None,
        safe_option=EventChoice(
            "Ignore the song and keep digging",
            success=EventOutcome("The hum fades as you pass. Nothing lost, nothing gained.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Follow the singing",
            success=EventOutcome("A small hoard glimmers in the hollow. You walk out richer.", 0, 8, False),
            failure=EventOutcome("The song ends and so does your luck. Coins slip from your belt.", 0, -18, False),
            success_chance=0.55,
        ),
        rarity="rare",
    ),
    RandomEvent(
        id="hex_cursed_shrine",
        name="Hex-Cursed Shrine",
        description=(
            "An old altar, still warm. The offerings on it are fresh. The curse on it is older.",
            "A shrine to a forgotten dig-god. The coins around it look untouched. That's probably fine.",
            "Carvings around the pedestal spell out a warning. Unfortunately, you cannot read dwarvish.",
        ),
        min_depth=51, max_depth=None,
        safe_option=EventChoice(
            "Bow respectfully and move on",
            success=EventOutcome("A small offering left in thanks. The shrine accepts your respect.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Grab the offerings",
            success=EventOutcome("The shrine lets you keep them. This time.", 0, 20, False),
            failure=EventOutcome("The hex lands. You feel lighter — mostly in the coin pouch.", 0, -24, False),
            success_chance=0.25,
        ),
        rarity="rare",
    ),
    RandomEvent(
        id="tricksters_toll",
        name="Trickster's Toll",
        description=(
            "A tollgate in the middle of nowhere. A skeleton leans against it, grinning. 'Coin, or coin?' it asks.",
            "A polite sign reads 'Pay the toll or play the toll'. Neither option looks appealing.",
            "Someone set up a toll booth inside the mines. The operator refuses to explain how.",
        ),
        min_depth=151, max_depth=None,
        safe_option=EventChoice(
            "Pay the toll quietly",
            success=EventOutcome("A few coins change hands. Nothing more.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Play the toll",
            success=EventOutcome("The skeleton shrugs and waves you through. You keep what you have.", 0, 0, False),
            failure=EventOutcome("Bad game, bad luck. The toll collects itself.", 0, -15, False),
            success_chance=0.50,
        ),
        rarity="rare",
    ),
    RandomEvent(
        id="gamblers_rest",
        name="Gambler's Rest",
        description=(
            "A small tavern tucked into the rock. The sign reads 'Gambler's Rest - No Refunds'.",
            "Dim lanterns, sticky tables, and a dealer who smiles a little too much. Just one hand.",
            "The tavern has no name written, only painted dice. The dealer is already shuffling.",
        ),
        min_depth=101, max_depth=None,
        safe_option=EventChoice(
            "Nurse a drink and leave",
            success=EventOutcome("You drink. You leave. You are none the poorer.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Sit at the table",
            success=EventOutcome("The dice smile on you. A tidy pile slides your way.", 0, 12, False),
            failure=EventOutcome("The house always wins. Even down here.", 0, -15, False),
            success_chance=0.40,
        ),
        rarity="rare",
    ),
    # ---- Splash events (legendary). When the configured trigger outcome
    # fires, a SplashConfig debits a pool of other players in the guild.
    # Coin is burned, not transferred to the digger. ------------------------
    RandomEvent(
        id="lumber_collapse",
        name="Lumber Collapse",
        description=(
            "A support beam groans. Somewhere distant, a tunnel just gave up.",
            "The rock around you shifts. Elsewhere in the network, someone else's ceiling comes down.",
            "A deep shudder passes through the whole mine. You feel people stumble.",
        ),
        min_depth=10, max_depth=None,
        safe_option=EventChoice(
            "Shore up the beam",
            success=EventOutcome("You steady the beam. The mine breathes again.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Gamble that it holds",
            success=EventOutcome("The beam creaks, settles, holds. You pocket a small find.", 0, 6, False),
            failure=EventOutcome("The collapse cascades through the network. Two other diggers' stashes get buried with it.", 0, -9, True),
            success_chance=0.40,
        ),
        rarity="legendary",
        splash=SplashConfig(strategy="random_active", victim_count=2, penalty_jc=5, trigger="failure"),
    ),
    RandomEvent(
        id="wealth_siphon",
        name="Wealth Siphon",
        description=(
            "A crystalline node pulses faintly. It tugs at the wealth in the air itself.",
            "The node hums at the frequency of coin. Somewhere above, three very rich people feel a chill.",
            "The deeper you dig, the louder the siphon sings. It is a very hungry song.",
        ),
        min_depth=51, max_depth=None,
        safe_option=EventChoice(
            "Leave it alone",
            success=EventOutcome("You pass without engaging the siphon.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Activate the siphon",
            success=EventOutcome("The siphon pulls from the richest pockets in the guild and dumps a cut into yours.", 0, 16, False),
            failure=EventOutcome("The siphon sputters and coughs. Nothing happens.", 0, 0, False),
            success_chance=0.50,
        ),
        rarity="legendary",
        splash=SplashConfig(strategy="richest_n", victim_count=3, penalty_jc=10, trigger="success"),
    ),
    RandomEvent(
        id="tunnel_network_breach",
        name="Tunnel Network Breach",
        description=(
            "You break through into another tunnel. The digger on the other side is not pleased.",
            "The wall is thinner here than it should be. You hear muffled cursing on the far side.",
            "A draft of someone else's stale air hits you. Apparently three of you share this rock.",
        ),
        min_depth=101, max_depth=None,
        safe_option=EventChoice(
            "Back off and shore it up",
            success=EventOutcome("You seal the breach. The other diggers never notice.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Punch through",
            success=EventOutcome("You crash in and come out with a pile of whatever was lying around.", 2, 10, False),
            failure=EventOutcome("The collapse spreads down the network. Three other diggers lose coin to the cave-in.", 0, -6, True),
            success_chance=0.35,
        ),
        rarity="legendary",
        splash=SplashConfig(strategy="active_diggers", victim_count=3, penalty_jc=8, trigger="failure"),
    ),

    # ---- Delve-themed roguelike expansion (PoE Delve / WoW Delves / Dota 2).
    # Bigger absolute swings than the baseline pool; risky/desperate options
    # land in the ±8-15 / ±20-40 JC band. Five of these carry SplashConfigs
    # (four burn, one grant).
    # -------------------------------------------------------------------------

    # --- Path of Exile Delve ---

    RandomEvent(
        id="sulphite_drought",
        name="Sulphite Drought",
        description=(
            "Your Voltaxic reserves sputter. Niko's crawler rattles to a halt.",
            "The fuel gauge bottoms out mid-delve. The darkness leans in a little closer.",
            "You squeeze the last drop of sulphite from the canister. It isn't enough.",
        ),
        min_depth=26, max_depth=200,
        safe_option=EventChoice(
            "Turn back to refuel",
            success=EventOutcome("You retreat to the last checkpoint. No coin, but you're alive.", -2, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Scavenge the fractured walls",
            success=EventOutcome("Residual sulphite crystals line the cracks — you bottle what you can.", 0, 24, False),
            failure=EventOutcome("The fracture widens. Rocks and dust; no fuel.", -4, -30, False),
            success_chance=0.50,
        ),
        desperate_option=EventChoice(
            "Bleed azurite into the crawler",
            success=EventOutcome("The crawler roars back to life on azurite fumes. Deeper you go.", 2, 60, False),
            failure=EventOutcome("The crawler chokes on the impurity. You're in the dark, down a lot of coin.", -6, -84, False),
            success_chance=0.25,
        ),
        rarity="rare",
    ),

    RandomEvent(
        id="auls_bullet_hell",
        name="Throne of Aul",
        description=(
            "A vast ice cavern opens. Aul the Crystal King regards you from a frozen throne.",
            "Spears of ice rotate around a seated figure at the cavern's far end. He does not speak.",
            "The walls become mirrors. The Crystal King's spear volleys rotate in three different directions.",
        ),
        min_depth=151, max_depth=275,
        safe_option=EventChoice(
            "Flee the throne room",
            success=EventOutcome("You back out the way you came. The ice follows you a little too far.", -3, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Weave between the spear volleys",
            success=EventOutcome("You thread the needle. A Hollow Fossil lies at the throne's base.", 0, 28, False),
            failure=EventOutcome("A spear catches your shoulder. You leave a trail of coin on the way out.", -9, -45, False),
            success_chance=0.40,
        ),
        desperate_option=EventChoice(
            "Charge the throne",
            success=EventOutcome("Impossible, and yet — you strike a chip from the throne itself. The hoard is yours.", 3, 80, False),
            failure=EventOutcome("The spears rotate through you. The throne is unchanged.", -8, -105, True),
            success_chance=0.22,
        ),
        rarity="legendary",
    ),

    RandomEvent(
        id="kurgal_summons",
        name="Kurgal's Circle",
        description=(
            "A bone-lich floats above a cracked obsidian floor. Abyssal wraiths spool from his fingers.",
            "The Blackblooded raises a summoning hand. The tunnels behind you have gone very quiet.",
            "Kurgal's eyes find you. The wraiths begin their patient spiral outward.",
        ),
        min_depth=101, max_depth=200,
        safe_option=EventChoice(
            "Evade the summoning circle",
            success=EventOutcome("You hug the wall and slip past. Kurgal does not turn.", -2, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Fight the wraith waves",
            success=EventOutcome("You shatter the summoning. Kurgal retreats into the obsidian.", 0, 24, False),
            failure=EventOutcome("A wraith slips past you and out into the network. You hear distant screaming.", -4, -36, False),
            success_chance=0.45,
        ),
        desperate_option=EventChoice(
            "Light the Abyss beacon",
            success=EventOutcome("The beacon binds Kurgal in his own circle. His hoard is yours.", 2, 56, False),
            failure=EventOutcome("The beacon detonates. Kurgal's wraiths scatter across the whole network.", -6, -90, False),
            success_chance=0.22,
        ),
        rarity="legendary",
        splash=SplashConfig(strategy="active_diggers", victim_count=2, penalty_jc=10, trigger="failure"),
    ),

    RandomEvent(
        id="flare_starvation",
        name="Flare Starvation",
        description=(
            "Your last flare gutters. At the edge of the light, six pairs of Stalker eyes.",
            "The darkness takes a slow breath. You have one flare left. The Stalkers are patient.",
            "Something in the tunnel ahead just learned your light radius. You have very little left.",
        ),
        min_depth=None, max_depth=None,
        safe_option=EventChoice(
            "Burn your own spare",
            success=EventOutcome("The flare catches. The Stalkers recede for now.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Push through the dim",
            success=EventOutcome("You make the next checkpoint before the light dies. A small score.", 2, 20, False),
            failure=EventOutcome("A Stalker touches you. Your flare goes out early; your coin with it.", -6, -30, False),
            success_chance=0.55,
        ),
        desperate_option=EventChoice(
            "Steal a nearby digger's flare",
            success=EventOutcome("You cross a tunnel wall and lift a flare from another digger's pack. Their problem now.", 0, 32, False),
            failure=EventOutcome("The flare snaps in your hand. The Stalkers close in.", -4, -45, False),
            success_chance=0.30,
        ),
        requires_dark=True, rarity="legendary",
        splash=SplashConfig(strategy="active_diggers", victim_count=1, penalty_jc=8, trigger="success"),
    ),

    RandomEvent(
        id="resonator_socket",
        name="Resonator Gamble",
        description=(
            "A brass-ringed resonator hums from the cavern wall, four empty fossil sockets waiting.",
            "The resonator's sockets glitter with old crafting potential. The fossils in your pack start rattling.",
            "Niko's laugh echoes somewhere: 'Try it. What's the worst that could happen?'",
        ),
        min_depth=51, max_depth=275,
        safe_option=EventChoice(
            "Take the Prismatic and leave",
            success=EventOutcome("A modest crafting bonus, banked. You move on.", 0, 4, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Socket the Aberrant",
            success=EventOutcome("Chaotic resonance, but the output is real coin.", 0, 26, False),
            failure=EventOutcome("The Aberrant shatters. Your pack is lighter by a fossil.", 0, -30, False),
            success_chance=0.50,
        ),
        desperate_option=EventChoice(
            "Stack Faceted and Hollow together",
            success=EventOutcome("The resonator sings. Gem-tag currency cascades out of the socket.", 2, 70, False),
            failure=EventOutcome("A Hollow Fossil does not forgive bad company. The resonator detonates.", -8, -96, True),
            success_chance=0.22,
        ),
        rarity="rare",
    ),

    # --- WoW Delves ---

    RandomEvent(
        id="branns_potion",
        name="Brann's Gambit",
        description=(
            "Brann Bronzebeard winds up a throw. You don't recognize the potion in his hand.",
            "'Catch!' Brann shouts, a little too cheerfully. The vial is already airborne.",
            "Brann has been rummaging again. He is smiling in a way that does not comfort you.",
        ),
        min_depth=None, max_depth=None,
        safe_option=EventChoice(
            "Dodge the throw",
            success=EventOutcome("The vial shatters behind you. You scrape a coin or two from the residue.", -2, 4, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Catch it",
            success=EventOutcome("Elixir of Undermining. Brann beams like it was the plan all along.", 0, 24, False),
            failure=EventOutcome("You caught the wrong end. Acid gets on the coin pouch.", 0, -30, False),
            success_chance=0.55,
        ),
        desperate_option=EventChoice(
            "Redirect the throw toward the wealthiest digger in the guild",
            success=EventOutcome("Brann approves. Someone wealthy, somewhere, is suddenly less wealthy.", 0, 36, False),
            failure=EventOutcome("The vial boomerangs. The potion is inside you now.", -3, -42, False),
            success_chance=0.30,
        ),
        rarity="legendary", social=True,
        # Splash fires on ANY risky/desperate success, not just the desperate
        # "redirect" path. Flavor-only leak: players picking risky and
        # succeeding also tax the richest 3, even though the narrative
        # attributes the redirect to the desperate choice. Accepted as-is.
        splash=SplashConfig(strategy="richest_n", victim_count=3, penalty_jc=12, trigger="success"),
    ),

    RandomEvent(
        id="zekvir_challenge",
        name="Zekvir's Lair",
        description=(
            "A webbed pit. Eight red eyes and eight legs. Above the pit, a faint '??' flickers.",
            "Zekvir's Angler's Web coils around the chamber. He's been waiting.",
            "The ?? tier door is unlocked. Zekvir's maw is already open.",
        ),
        min_depth=201, max_depth=275,
        safe_option=EventChoice(
            "Skip the arena entirely",
            success=EventOutcome("You give Zekvir a wide berth. He does not pursue.", -3, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Enter at the ? tier",
            success=EventOutcome("You survive the Claw Smashes. The Web Terrors leave a chest behind.", 0, 30, False),
            failure=EventOutcome("Angler's Web pulls you in. You escape with bruises and a shorter coin pouch.", -6, -45, False),
            success_chance=0.40,
        ),
        desperate_option=EventChoice(
            "Take the ?? tier",
            success=EventOutcome("Nobody has killed ?? tier. You just did. The loot is unreal.", 3, 90, False),
            failure=EventOutcome("Zekvir devours you whole. Your pack reappears empty at the tunnel mouth.", -9, -120, True),
            success_chance=0.20,
        ),
        rarity="legendary",
    ),

    RandomEvent(
        id="enchanted_candle",
        name="Kriegval's Candle",
        description=(
            "An Enchanted Candle waits at the room's entrance. Smothering Shadows pool in the corners.",
            "You know how this one works. Light the candle. Keep moving. It burns fast when you run.",
            "Kriegval's Rest never forgave a slow foot. The candle is already half gone.",
        ),
        min_depth=51, max_depth=200,
        safe_option=EventChoice(
            "Walk slowly and shield the candle",
            success=EventOutcome("You conserve the flame. Steady coin at a steady pace.", 1, 4, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Jog the room",
            success=EventOutcome("The candle stutters but holds. You clear the room with a handful of coin.", 2, 24, False),
            failure=EventOutcome("Smothering Shadows catch you mid-stride. The candle dies. You limp out.", -4, -36, False),
            success_chance=0.55,
        ),
        desperate_option=EventChoice(
            "Sprint the entire gauntlet",
            success=EventOutcome("Candle out at the exit — a second too late for the Shadows. Huge haul.", 4, 60, False),
            failure=EventOutcome("Candle out mid-room. The ceiling follows it.", -9, -84, True),
            success_chance=0.25,
        ),
        rarity="rare",
    ),

    RandomEvent(
        id="sporbit_cavern",
        name="Mycomancer Cavern",
        description=(
            "Pink Sporbits drift along patrol routes. Their AoEs flicker in rhythm.",
            "The Mycomancer Cavern glows bioluminescent. You count three Sporbit patrol loops.",
            "Fungal cavern. Bright-pink orbs on circuits. You can see the pattern if you watch.",
        ),
        min_depth=101, max_depth=200,
        safe_option=EventChoice(
            "Wait for the patrol pattern",
            success=EventOutcome("You slip through on the timing gap. No coin, but no Sporbits either.", 1, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Dash between Sporbits",
            success=EventOutcome("Threaded it. A fungal cache waits in the far alcove.", 2, 22, False),
            failure=EventOutcome("A Sporbit's AoE catches you. Spores in the pack, coin on the floor.", -4, -33, False),
            success_chance=0.50,
        ),
        desperate_option=EventChoice(
            "Trigger a chain explosion",
            success=EventOutcome("Every Sporbit in the room pops in sequence. The aftermath is all yours.", 3, 64, False),
            failure=EventOutcome("The chain misfires. You are at the epicentre.", -8, -90, True),
            success_chance=0.22,
        ),
        rarity="rare",
    ),

    RandomEvent(
        id="coffer_key_gamble",
        name="Restored Coffer",
        description=(
            "A Restored Coffer sits on a stone altar. Your Coffer Key fits, barely.",
            "The coffer lid is gilded. The Delver's Journey card in your pack suggests you can try for more.",
            "Bountiful? Gilded? The altar offers both doors.",
        ),
        min_depth=26, max_depth=275,
        safe_option=EventChoice(
            "Bank the key for a deeper run",
            success=EventOutcome("You pocket the key. No coin now, but it's still yours.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Unlock the Bountiful Coffer here",
            success=EventOutcome("A tidy Restored Coffer pull. Cosmetics flagged, coin counted.", 0, 28, False),
            failure=EventOutcome("The coffer was trapped — a bad pull on a bad key.", 0, -24, False),
            success_chance=0.60,
        ),
        desperate_option=EventChoice(
            "Gamble the key at the Gilded Stash",
            success=EventOutcome("Seven Myth Dawncrests spill into your pack. The week is made.", 0, 84, False),
            failure=EventOutcome("The stash was picked clean. Your key is ash.", -3, -105, False),
            success_chance=0.20,
        ),
        rarity="rare",
    ),

    # --- Dota 2 ---

    RandomEvent(
        id="chain_frost_cascade",
        name="Chain Frost",
        description=(
            "Lich's orb of Chain Frost hovers at the cavern mouth. It does not care who's adjacent.",
            "The frost finds three diggers at once. Yours, and two others in the network.",
            "You cast Chain Frost. It never bounces just once.",
        ),
        min_depth=101, max_depth=275,
        safe_option=EventChoice(
            "Shield against the cold",
            success=EventOutcome("You weather the pass. A small expense for a clean tunnel.", 0, -15, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Cast Chain Frost and let it bounce",
            success=EventOutcome("The frost lands ten times and pays in crystallized coin.", 0, 26, False),
            failure=EventOutcome("The frost bounces back. Your pack is iced shut.", -3, -36, False),
            success_chance=0.50,
        ),
        rarity="legendary",
        splash=SplashConfig(strategy="active_diggers", victim_count=2, penalty_jc=8, trigger="always"),
    ),

    RandomEvent(
        id="divine_rapier_drop",
        name="Divine Rapier",
        description=(
            "A Divine Rapier juts from the rubble. Heat wafts off the blade.",
            "The Rapier hasn't been claimed. The previous owner is a scorch mark on the wall.",
            "You know the rules. Pick it up and the next cave-in drops it back on the floor.",
        ),
        min_depth=101, max_depth=275,
        safe_option=EventChoice(
            "Leave the Rapier",
            success=EventOutcome("Scraps of creep carcass; a small coin in the ash.", 0, 6, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Pick it up and sprint to the next checkpoint",
            success=EventOutcome("You clear the tunnel without incident. Rapier sold, pockets heavy.", 2, 70, False),
            failure=EventOutcome("The Rapier drops at the cave-in. You don't.", -9, -75, True),
            success_chance=0.50,
        ),
        desperate_option=EventChoice(
            "Carry it deeper",
            success=EventOutcome("Two checkpoints later the Rapier is still yours. Nobody can believe it.", 4, 110, False),
            failure=EventOutcome("A cave-in. A Rapier drop. A very rich stranger a tunnel over.", -12, -150, True),
            success_chance=0.22,
        ),
        rarity="legendary",
    ),

    RandomEvent(
        id="enigma_blackhole",
        name="Event Horizon",
        description=(
            "Enigma's Black Hole anchors a side passage. It is pulling at everything with mass.",
            "The pull is gentle. The thing at the center is not.",
            "Rocks, ore, and the silhouettes of two distant diggers drift slowly toward the sphere.",
        ),
        min_depth=151, max_depth=275,
        safe_option=EventChoice(
            "Skirt the event horizon",
            success=EventOutcome("You take the long way around. It is much longer than you expected.", -2, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Channel through the edge",
            success=EventOutcome("The edge scours your pack and leaves coin behind as sediment.", 0, 28, False),
            failure=EventOutcome("A partial pull. You leave a lot on the other side.", -6, -42, False),
            success_chance=0.40,
        ),
        desperate_option=EventChoice(
            "Drag two diggers in with you",
            success=EventOutcome("The three of you enter. Only you come back out. Pockets full.", 2, 76, False),
            failure=EventOutcome("The Hole has opinions about this plan.", -9, -108, True),
            success_chance=0.25,
        ),
        rarity="legendary",
        splash=SplashConfig(strategy="active_diggers", victim_count=2, penalty_jc=15, trigger="success"),
    ),

    RandomEvent(
        id="io_tether_pact",
        name="Wisp Tether",
        description=(
            "A gold-and-white orb drifts beside you. A tether line flickers to a distant digger.",
            "Io's tether wants a partner. Somewhere in the network, another digger just felt a tug.",
            "The wisp hums. The tether is soft, then insistent.",
        ),
        min_depth=26, max_depth=200,
        safe_option=EventChoice(
            "Decline the tether",
            success=EventOutcome("The wisp fades. You pay a small tribute for its time.", 0, -6, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Accept a soft tether",
            success=EventOutcome("The tether holds. You move a little faster, pay a little less.", 1, 20, False),
            failure=EventOutcome("The tether snaps. The wisp leaves with a grudge.", 0, -24, False),
            success_chance=0.55,
        ),
        desperate_option=EventChoice(
            "Full tether pact",
            success=EventOutcome("The tether sings. Your partner feels the haul land in their pack too.", 2, 50, False),
            failure=EventOutcome("The tether yanks you both the wrong way. You eat the loss.", -4, -60, False),
            success_chance=0.30,
        ),
        rarity="legendary", social=True,
        splash=SplashConfig(strategy="active_diggers", victim_count=1, penalty_jc=10, trigger="success", mode="grant"),
    ),

    RandomEvent(
        id="refresher_shard",
        name="Refresher Shard",
        description=(
            "A Refresher Shard crystal is wedged into the wall. Purple light doubles your shadow.",
            "The shard hums at the frequency of cooldowns. A second cast is waiting for you.",
            "Rubick is not here, but you feel watched anyway.",
        ),
        min_depth=76, max_depth=275,
        safe_option=EventChoice(
            "Pocket the shard for later",
            success=EventOutcome("You bank the shard. A modest finder's bounty for the find.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Double-cast the next event",
            success=EventOutcome("The next event fires twice. Both land clean.", 0, 36, False),
            failure=EventOutcome("Both casts miss. The shard shivers apart in your hand.", 0, -45, False),
            success_chance=0.50,
        ),
        desperate_option=EventChoice(
            "Activate on this event, right now",
            success=EventOutcome("The Refresher refreshes itself. The cave gives twice over.", 2, 76, False),
            failure=EventOutcome("Two casts, two misfires. The cave answers with a collapse.", -6, -105, True),
            success_chance=0.22,
        ),
        rarity="legendary",
    ),

    # ---- Risky cross-player expansion. Mixed flavors: chaos burn, zero-sum
    # theft (mode="steal"), and digger-pays sacrifice. Spread across all depth
    # bands so cross-player risk is part of the dig identity from the start.
    # -------------------------------------------------------------------------

    RandomEvent(
        id="aegis_whisper",
        name="Aegis Whisper",
        description=(
            "A half-buried shield rim glints in the dirt. The metal hums with an old promise.",
            "Something circular is pressed into the wall — a shield, ancient, still warm.",
            "You hear a faint chant from the rock. The stone wants you to make an offering.",
        ),
        min_depth=None, max_depth=None,
        safe_option=EventChoice(
            "Leave it buried",
            success=EventOutcome("You walk past. The chant fades.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Make an offering",
            success=EventOutcome("The shield drinks your coin and breathes a faint protective glow over you.", 0, -8, False),
            failure=EventOutcome("Your coin sinks into the dirt. Nothing happens.", 0, -8, False),
            success_chance=0.50,
        ),
        buff_on_success=TempBuff("aegis_aura", "Aegis Aura", 3, {"cave_in_reduction": 0.15}),
        rarity="uncommon",
    ),
    RandomEvent(
        id="echoing_mime",
        name="Echoing Mime",
        description=(
            "A pale figure performs a silent show in the gloom. Other diggers always seem to stop and watch.",
            "The mime tilts its head at you. Somewhere in the network, picks go still.",
            "It bows. A polite, silent invitation to be made a fool of.",
        ),
        min_depth=None, max_depth=None,
        safe_option=EventChoice(
            "Avert your eyes",
            success=EventOutcome("You keep digging. The performance ends without applause.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Mock the mime back",
            success=EventOutcome("You out-perform it. The mime tips a coin into your palm.", 0, 6, False),
            failure=EventOutcome("The mime doesn't break character. You feel small.", 0, -3, False),
            success_chance=0.65,
        ),
        rarity="uncommon", social=True,
        splash=SplashConfig(
            strategy="active_diggers", victim_count=2, penalty_jc=4,
            trigger="always", mode="burn",
        ),
    ),
    RandomEvent(
        id="crow_snipe",
        name="Crow Snipe",
        description=(
            "A delivery crow flaps past, a heavy pouch swinging from its talons.",
            "A messenger bird overhead — laden, slow, oblivious.",
            "Something glitters in the bird's grip as it crosses overhead.",
        ),
        min_depth=10, max_depth=None,
        safe_option=EventChoice(
            "Wave the crow off",
            success=EventOutcome("The bird carries on its way.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Snipe it down",
            success=EventOutcome("The pouch hits the dirt at your feet. Someone wealthy is having a worse day than you.", 0, 0, False),
            failure=EventOutcome("You miss. A guard's whistle sounds; you pay a fine to slip away.", 0, -6, False),
            success_chance=0.60,
        ),
        rarity="uncommon", social=True,
        splash=SplashConfig(
            strategy="richest_n", victim_count=1, penalty_jc=10,
            trigger="success", mode="steal",
        ),
    ),
    RandomEvent(
        id="smoke_detour",
        name="Smoke Detour",
        description=(
            "A drifting purple haze hides a shortcut through someone else's tunnel.",
            "The smoke pools in a side passage. You can almost see through to the other tunnel network.",
            "A cloaked path opens up. The air tastes like other people's progress.",
        ),
        min_depth=26, max_depth=None,
        safe_option=EventChoice(
            "Stick to your own tunnel",
            success=EventOutcome("You stay the course. Steady progress.", 1, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Slip into the smoke",
            success=EventOutcome("You shortcut clean through and pop out ahead, pockets heavier.", 1, 15, False),
            failure=EventOutcome("The smoke clears around the wrong people. Two diggers' caches collapse.", 0, -8, False),
            success_chance=0.55,
        ),
        rarity="rare", social=True,
        splash=SplashConfig(
            strategy="active_diggers", victim_count=2, penalty_jc=6,
            trigger="failure", mode="burn",
        ),
    ),
    RandomEvent(
        id="strangers_lamp",
        name="Stranger's Lamp",
        description=(
            "An ornate brass lamp is wedged into the wall. You feel the urge to rub it.",
            "The lamp is still warm. Something inside is waiting to be asked for a favor.",
            "A genie's lamp, half-buried. You half-remember warnings about wishes.",
        ),
        min_depth=26, max_depth=None,
        safe_option=EventChoice(
            "Leave the lamp alone",
            success=EventOutcome("You step around it. Some wishes aren't worth making.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Rub the lamp",
            success=EventOutcome("The genie grants you riches — and tosses a coin to a stranger for good measure.", 0, 15, False),
            failure=EventOutcome("The lamp coughs out a puff of soot. You're 5 coin lighter for the trouble.", 0, -5, False),
            success_chance=0.40,
        ),
        rarity="rare", social=True,
        splash=SplashConfig(
            strategy="random_active", victim_count=1, penalty_jc=5,
            trigger="success", mode="grant",
        ),
    ),
    RandomEvent(
        id="drill_sergeant",
        name="Drill Sergeant",
        description=(
            "A spectral sergeant blocks the passage and starts barking orders.",
            "DOWN! UP! DOWN! UP! The ghost will not be ignored.",
            "A phantom drill sergeant materializes, whistle in hand. You stand at attention by reflex.",
        ),
        min_depth=26, max_depth=None,
        safe_option=EventChoice(
            "Crawl past quietly",
            success=EventOutcome("You sneak through during a count. The ghost never notices.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Snap to attention",
            success=EventOutcome("You complete the drill. The ghost salutes and dissolves; your form has never been better.", 0, -10, False),
            failure=None, success_chance=1.0,
        ),
        buff_on_success=TempBuff("drilled", "Drilled", 4, {"advance_bonus": 2}),
        rarity="rare",
    ),
    RandomEvent(
        id="pit_lords_toll",
        name="Pit Lord's Toll",
        description=(
            "A massive horned figure looms in the dark, rattling a cup of bone dice.",
            "A horned silhouette gestures at a pile of skulls. He wants to play.",
            "The pit lord smiles. The kind of smile that costs other people money.",
        ),
        min_depth=51, max_depth=None,
        safe_option=EventChoice(
            "Refuse politely",
            success=EventOutcome("He shrugs and lets you pass. You lose a step backing away.", -1, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Roll the bone dice",
            success=EventOutcome("The dice land in your favor. The pit lord grins and pays out.", 0, 35, False),
            failure=EventOutcome("The dice land badly. The pit lord cackles, and three diggers feel the floor drop.", 0, -10, False),
            success_chance=0.45,
        ),
        rarity="legendary", social=True,
        splash=SplashConfig(
            strategy="active_diggers", victim_count=3, penalty_jc=8,
            trigger="failure", mode="burn",
        ),
    ),
    RandomEvent(
        id="damned_bottle",
        name="Damned Bottle",
        description=(
            "An ornate bottle whispers two names in the dark — yours, and a stranger's.",
            "The bottle sweats a faint blue light. It is full, and it remembers who filled it.",
            "A sealed vessel hums. Cork it, the whispers stop. Uncork it, things get interesting.",
        ),
        min_depth=76, max_depth=None,
        safe_option=EventChoice(
            "Leave it sealed",
            success=EventOutcome("You walk on. The whispers fade.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Uncork it",
            success=EventOutcome("The bottle unfastens itself from another digger's stash. The contents are now yours.", 0, 0, False),
            failure=EventOutcome("The bottle was a curse. It empties your pockets in answer.", 0, -20, False),
            success_chance=0.50,
        ),
        rarity="legendary", social=True,
        splash=SplashConfig(
            strategy="random_active", victim_count=1, penalty_jc=15,
            trigger="success", mode="steal",
        ),
    ),
    RandomEvent(
        id="roshpit_gambit",
        name="Roshpit Gambit",
        description=(
            "An ancient pit yawns open. Tribute paid by the wealthy lines its rim.",
            "A pit older than the mine. Coins from the richest pockets keep washing up at its edge.",
            "The pit hums at a frequency only large balances can hear.",
        ),
        min_depth=76, max_depth=None,
        safe_option=EventChoice(
            "Skirt the pit",
            success=EventOutcome("You give the pit a wide berth. Boring, safe.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Plunge in",
            success=EventOutcome("You surface clutching a king's ransom. Two of the richest just felt it leave their stash.", 0, 40, False),
            failure=EventOutcome("The pit takes its tribute from you instead.", 0, -25, False),
            success_chance=0.50,
        ),
        rarity="legendary", social=True,
        splash=SplashConfig(
            strategy="richest_n", victim_count=2, penalty_jc=12,
            trigger="success", mode="burn",
        ),
    ),
    RandomEvent(
        id="wilderness_stalker",
        name="Wilderness Stalker",
        description=(
            "A masked figure has been tracking you through the deep tunnels. They are no longer subtle.",
            "Footprints behind yours. Then closer. Then close enough to hear breathing.",
            "A stranger steps from the dark, blade drawn. They've been waiting for you.",
        ),
        min_depth=101, max_depth=None,
        safe_option=EventChoice(
            "Hide and wait them out",
            success=EventOutcome("You go still until they pass. You lose ground but you keep your purse.", -1, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Strike first",
            success=EventOutcome("You drop them in one strike and lift their take. Whoever they were, they were carrying.", 0, 0, False),
            failure=EventOutcome("They were better than you. You drag yourself away wounded and lighter.", -1, -25, False),
            success_chance=0.65,
        ),
        rarity="legendary", social=True,
        splash=SplashConfig(
            strategy="active_diggers", victim_count=1, penalty_jc=20,
            trigger="success", mode="steal",
        ),
    ),
]


# ---------------------------------------------------------------------------
# Achievements
# ---------------------------------------------------------------------------

@dataclass(frozen=True)
class AchievementTier:
    """A single tier of an achievement."""
    tier: str                       # Bronze | Silver | Gold
    threshold: int


@dataclass(frozen=True)
class AchievementDef:
    """Immutable definition for a visible achievement."""
    id: str
    name: str
    description: str
    tiers: list[AchievementTier]


VISIBLE_ACHIEVEMENTS: list[AchievementDef] = [
    AchievementDef("dig_count", "Dedicated Digger", "Total digs performed", [
        AchievementTier("Bronze", 10), AchievementTier("Silver", 50), AchievementTier("Gold", 100),
    ]),
    AchievementDef("depth_reached", "Deep Delver", "Maximum depth reached", [
        AchievementTier("Bronze", 25), AchievementTier("Silver", 50), AchievementTier("Gold", 100),
    ]),
    AchievementDef("bosses_killed", "Boss Slayer", "Bosses defeated", [
        AchievementTier("Bronze", 1), AchievementTier("Silver", 2), AchievementTier("Gold", 4),
    ]),
    AchievementDef("artifacts_found", "Artifact Hunter", "Artifacts discovered", [
        AchievementTier("Bronze", 3), AchievementTier("Silver", 8), AchievementTier("Gold", 15),
    ]),
    AchievementDef("times_helped", "Good Samaritan", "Times helped other diggers", [
        AchievementTier("Bronze", 5), AchievementTier("Silver", 15), AchievementTier("Gold", 30),
    ]),
    AchievementDef("cave_ins_survived", "Hard Headed", "Cave-ins survived", [
        AchievementTier("Bronze", 5), AchievementTier("Silver", 20), AchievementTier("Gold", 50),
    ]),
    AchievementDef("jc_earned", "Treasure Hoarder", "Total JC earned from digging", [
        AchievementTier("Bronze", 50), AchievementTier("Silver", 200), AchievementTier("Gold", 500),
    ]),
    AchievementDef("sabotages_done", "Tunnel Rat", "Sabotages performed", [
        AchievementTier("Bronze", 3), AchievementTier("Silver", 10), AchievementTier("Gold", 25),
    ]),
    AchievementDef("streaks", "Consistent Miner", "Longest dig streak (days)", [
        AchievementTier("Bronze", 7), AchievementTier("Silver", 14), AchievementTier("Gold", 30),
    ]),
    AchievementDef("prestige_level", "Prestigious", "Prestige level reached", [
        AchievementTier("Bronze", 1), AchievementTier("Silver", 3), AchievementTier("Gold", 5),
    ]),
    # Expansion achievements
    AchievementDef("depth_deep", "Depth Explorer", "Reach extreme depths", [
        AchievementTier("Bronze", 150), AchievementTier("Silver", 200), AchievementTier("Gold", 300),
    ]),
    AchievementDef("events_survived", "Event Survivor", "Survive risky event outcomes", [
        AchievementTier("Bronze", 5), AchievementTier("Silver", 20), AchievementTier("Gold", 50),
    ]),
    AchievementDef("darkness_walker", "Shadow Walker", "Digs completed in Dark/Pitch Black luminosity", [
        AchievementTier("Bronze", 10), AchievementTier("Silver", 50), AchievementTier("Gold", 100),
    ]),
    AchievementDef("dota_scholar", "Dota Scholar", "Encounter Dota hero events", [
        AchievementTier("Bronze", 3), AchievementTier("Silver", 5), AchievementTier("Gold", 8),
    ]),
    AchievementDef("items_used", "Tool Master", "Unique items used", [
        AchievementTier("Bronze", 5), AchievementTier("Silver", 15), AchievementTier("Gold", 30),
    ]),
    AchievementDef("guild_worthy", "Guild-Worthy", "Reach depth 200", [
        AchievementTier("Bronze", 200), AchievementTier("Silver", 275), AchievementTier("Gold", 400),
    ]),
]


@dataclass(frozen=True)
class HiddenAchievementDef:
    """Immutable definition for a hidden achievement."""
    id: str
    name: str
    description: str
    hint: str


HIDDEN_ACHIEVEMENTS: list[HiddenAchievementDef] = [
    HiddenAchievementDef(
        "midnight_miner", "Midnight Miner",
        "Dig between midnight and 1 AM",
        "The earth is quietest in the witching hour...",
    ),
    HiddenAchievementDef(
        "own_trap", "Rock Bottom",
        "Dig at depth 0 after a cave-in",
        "Sometimes you have to start from the very bottom.",
    ),
    HiddenAchievementDef(
        "broke_prestige", "Humble Beginnings",
        "Prestige with 0 JC balance",
        "True wealth isn't measured in coins.",
    ),
    HiddenAchievementDef(
        "layer_collector", "Completionist",
        "Find all artifacts in a single layer",
        "Some layers hold more secrets than others.",
    ),
    HiddenAchievementDef(
        "first_try_boss", "Flawless Victory",
        "Defeat a boss on your first attempt",
        "Fortune favors the bold... sometimes.",
    ),
    HiddenAchievementDef(
        "deep_explorer", "Into the Unknown",
        "Reach depth 200",
        "How deep does it go? Only one way to find out.",
    ),
    HiddenAchievementDef(
        "survivor", "Unkillable",
        "Survive 50 cave-ins",
        "What doesn't kill you makes you... dustier.",
    ),
    HiddenAchievementDef(
        "century_digger", "Century Club",
        "Dig 100 times",
        "Dedication has its own reward.",
    ),
    HiddenAchievementDef(
        "helpful_friend", "Tunnel BFF",
        "Help someone 50 times",
        "A friend in the deep is a friend indeed.",
    ),
    HiddenAchievementDef(
        "insurance_fraud", "Suspicious Timing",
        "Get sabotaged within 1 hour of insurance expiring",
        "The universe has a cruel sense of humor.",
    ),
    # Expansion hidden achievements
    HiddenAchievementDef(
        "identity_crisis", "Identity Crisis",
        "Encounter The Nameless Depth when your tunnel is named 'The Nameless Depth'",
        "What's in a name? Everything, apparently.",
    ),
    HiddenAchievementDef(
        "paradox_resolved", "Paradox Resolved",
        "Win the Paradox Loop fight against yourself",
        "Can you beat someone who knows all your moves?",
    ),
    HiddenAchievementDef(
        "blind_faith", "Blind Faith",
        "Defeat a boss at Pitch Black luminosity",
        "Who needs eyes in the dark?",
    ),
    HiddenAchievementDef(
        "aegis_claimed", "Aegis Claimed",
        "Defeat Roshan's Lair encounter",
        "The ancient falls. The aegis endures.",
    ),
    HiddenAchievementDef(
        "full_museum", "Curator",
        "Be the first to discover 10+ artifacts in the guild museum",
        "A life's work, preserved underground.",
    ),
    HiddenAchievementDef(
        "art_of_the_deal", "The Art of the Deal",
        "Fail to haggle with The Final Merchant 3 times",
        "Negotiation is not your strong suit.",
    ),
]


# ---------------------------------------------------------------------------
# Tunnel Name Word Pools
# ---------------------------------------------------------------------------

TUNNEL_NAME_ADJECTIVES: list[str] = [
    "Whispering", "Echoing", "Forgotten", "Shimmering", "Crooked",
    "Haunted", "Dusty", "Verdant", "Frostbitten", "Soggy",
    "Screaming", "Gilded", "Moldy", "Thundering", "Slippery",
]

TUNNEL_NAME_NOUNS: list[str] = [
    "Descent", "Passage", "Burrow", "Excavation", "Shaft",
    "Tunnel", "Grotto", "Hollow", "Delve", "Pit",
    "Crevice", "Gallery", "Abyss", "Warren", "Mine",
]

TUNNEL_NAME_TITLE_X: list[str] = [
    "Shaft", "Tunnel", "Mine", "Passage", "Depths",
    "Burrow", "Pit", "Grotto", "Hollow", "Excavation",
]

TUNNEL_NAME_TITLE_Y: list[str] = [
    "Sorrows", "Echoes", "Fortune", "Doom", "Whispers",
    "Secrets", "Madness", "Riches", "Despair", "Wonder",
    "Cheese", "Bones", "Regret",
]

TUNNEL_NAME_SILLY: list[str] = [
    "Tunnel McTunnelface",
    "The Big Hole",
    "Definitely Not a Grave",
    "Hole-y Moley",
    "Rock Bottom",
    "Dig Dug's Revenge",
    "The Mole Hole",
    "Shovel Knight's Disgrace",
    "Spelunky Rejects",
    "The Underground Railroad to Nowhere",
]


# ---------------------------------------------------------------------------
# ASCII Art Templates (one per layer, compact for Discord embeds)
# ---------------------------------------------------------------------------

ASCII_ART: dict[str, str] = {
    "Dirt": (
        "  ~~~~~ SURFACE ~~~~~\n"
        "  ||||||||||||||||||||\n"
        "  ====================\n"
        "  .:. dirt .:. dirt .:.\n"
        "  . . . . . . . . . .\n"
        "  .:. . .:. . .:. . .\n"
        "  . . .worms. . . . .\n"
        "  .:. . .:. . .:. . .\n"
        "      ⛏️ YOU ARE HERE\n"
        "  ===================="
    ),
    "Stone": (
        "  ---- dirt above ----\n"
        "  ####################\n"
        "  #  STONE  LAYER   #\n"
        "  # ite ite ite ite #\n"
        "  #  []  []  []  [] #\n"
        "  # gran gran gran  #\n"
        "  #  []  []  []  [] #\n"
        "  #  fossils here   #\n"
        "      ⛏️ YOU ARE HERE\n"
        "  ####################"
    ),
    "Crystal": (
        "  ---- stone above ---\n"
        "  ********************\n"
        "  * CRYSTAL  CAVERNS *\n"
        "  *  /\\  <>  /\\  <>  *\n"
        "  * <>  /\\  <>  /\\   *\n"
        "  *  *sparkle*  <>   *\n"
        "  * /\\  <>  /\\  <>   *\n"
        "  *  <>  /\\  <>  /\\  *\n"
        "      ⛏️ YOU ARE HERE\n"
        "  ********************"
    ),
    "Magma": (
        "  -- crystals above --\n"
        "  ~~~~~~~~~~~~~~~~~~~~\n"
        "  ~  MAGMA  DEPTHS  ~\n"
        "  ~ =/\\/\\= =/\\/\\=  ~\n"
        "  ~  lava   lava    ~\n"
        "  ~ =/\\/\\= =/\\/\\=  ~\n"
        "  ~  *hiss* *glow*  ~\n"
        "  ~ =/\\/\\= =/\\/\\=  ~\n"
        "      ⛏️ YOU ARE HERE\n"
        "  ~~~~~~~~~~~~~~~~~~~~"
    ),
    "Abyss": (
        "  --- magma above ----\n"
        "  .                  .\n"
        "  .   T H E          .\n"
        "  .     A B Y S S    .\n"
        "  .                  .\n"
        "  .   . . . . . .   .\n"
        "  .  nothing here   .\n"
        "  .    ...or is it? .\n"
        "      ⛏️ YOU ARE HERE\n"
        "  .                  ."
    ),
    "Fungal Depths": (
        "  --- abyss above ----\n"
        "  ~~~~~~~~~~~~~~~~~~~~\n"
        "  ~ FUNGAL  DEPTHS  ~\n"
        "  ~ 🍄 .  🍄 .  🍄 ~\n"
        "  ~  . glow .  glow ~\n"
        "  ~ 🍄 .  🍄 .  🍄 ~\n"
        "  ~  spores  drift  ~\n"
        "  ~ 🍄 .  🍄 .  🍄 ~\n"
        "      ⛏️ YOU ARE HERE\n"
        "  ~~~~~~~~~~~~~~~~~~~~"
    ),
    "Frozen Core": (
        "  --- fungal above ---\n"
        "  ********************\n"
        "  *  FROZEN   CORE  *\n"
        "  * ❄️  .  ❄️  .  ❄️ *\n"
        "  *  time  slows    *\n"
        "  * ❄️  .  ❄️  .  ❄️ *\n"
        "  *  frost  creeps  *\n"
        "  * ❄️  .  ❄️  .  ❄️ *\n"
        "      ⛏️ YOU ARE HERE\n"
        "  ********************"
    ),
    "The Hollow": (
        "  --- frozen above ---\n"
        "                      \n"
        "                      \n"
        "     T H E            \n"
        "       H O L L O W    \n"
        "                      \n"
        "    the mine           \n"
        "      remembers you   \n"
        "      ⛏️ YOU ARE HERE\n"
        "                      "
    ),
}


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
# (depth 300) progress feels like a slow descent under pressure rather
# than an open runway.
PRESTIGE_HARD_CAP: int = 500
# Past this depth the drain rate accelerates linearly — +1 extra drain
# per LUMINOSITY_DEEP_DRAIN_BLOCKS_PER_STEP blocks. At cap (depth 500)
# the bonus is +10, doubling The Hollow's base drain.
LUMINOSITY_DEEP_DRAIN_START_DEPTH: int = 300
LUMINOSITY_DEEP_DRAIN_BLOCKS_PER_STEP: int = 20

# Pinnacle catch-up: if a player tunneled past the pinnacle without
# defeating it (legacy tunnels that pre-date the pinnacle, or skipped
# encounters), the pinnacle re-procs at this depth so prestige isn't
# permanently locked out. Tier bosses must still all be cleared.
PINNACLE_REPROC_DEPTH: int = 400

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

RELIC_SLOTS_BASE: int = 1  # relic_slots = prestige_level + RELIC_SLOTS_BASE

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
]

# Per-prestige-level bonuses for each perk
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
        reward="JC loot +25%",
        effects={"jc_multiplier": 0.25},
        gameplay=False,
    ),
    2: AscensionModifier(
        level=2, name="Unstable Ground",
        penalty="Cave-in chance +3%",
        reward="Event chance +20%",
        effects={"cave_in_bonus": 0.03, "event_chance_multiplier": 0.20},
        gameplay=False,
    ),
    3: AscensionModifier(
        level=3, name="Hungry Darkness",
        penalty="Luminosity drains 25% faster",
        reward="Rare events 50% more common",
        effects={"luminosity_drain_multiplier": 0.25, "rare_event_multiplier": 0.50},
        gameplay=False,
    ),
    4: AscensionModifier(
        level=4, name="Boss Rage",
        penalty="Bosses gain a secret second phase",
        reward="Boss payouts +50%",
        effects={"boss_phase2": True, "boss_payout_multiplier": 0.50},
        gameplay=True,
    ),
    5: AscensionModifier(
        level=5, name="Erosion",
        penalty="Decay rate +50%",
        reward="Milestone rewards +50%",
        effects={"decay_multiplier": 0.50, "milestone_multiplier": 0.50},
        gameplay=False,
    ),
    6: AscensionModifier(
        level=6, name="Corruption",
        penalty="Each dig rolls a random micro-modifier",
        reward="Artifact find rate doubled",
        effects={"corruption": True, "artifact_multiplier": 2.0},
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

EVENT_CHAIN_CHANCE: float = 0.15
EVENT_CHAIN_JC_MULTIPLIER: float = 1.2


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

OMINOUS_TUNNEL_NAMES: list[str] = [
    "The Descent That Never Ends",
    "Tomb of the Last Digger",
    "WHERE ARE YOU GOING",
    "The Walls Are Watching",
    "it knows your name",
    "Tunnel of Regret",
    "The Hungry Dark",
    "Something Lives Here",
]


# ---------------------------------------------------------------------------
# Boss Phase 2 Definitions (P4+, Sekiro / Mythic Lura style)
# ---------------------------------------------------------------------------

@dataclass(frozen=True)
class BossPhase2Def:
    """Secret second phase for bosses at prestige 4+."""
    depth: int
    name: str
    title: str
    dialogue: list[str]
    win_odds_penalty: float   # additional penalty to win odds (-0.10 = -10%)


BOSS_PHASE2: dict[int, BossPhase2Def] = {
    25: BossPhase2Def(
        depth=25,
        name="Grothak the Undying",
        title="Skeletal Wrath",
        dialogue=[
            "You... thought that would STOP me?! I shed my flesh like a coat!",
            "Back again, and so am I. Bones don't tire, worm.",
            "At this point my chiropractor is just a necromancer.",
        ],
        win_odds_penalty=-0.10,
    ),
    50: BossPhase2Def(
        depth=50,
        name="Crystalia Shattered",
        title="The Thousand Reflections",
        dialogue=[
            "You broke me! But every shard is a NEW me! Geometry is ETERNAL!",
            "Which one is real? Trick question. They ALL are.",
            "I have become a fractal. Please send help.",
        ],
        win_odds_penalty=-0.10,
    ),
    75: BossPhase2Def(
        depth=75,
        name="Magmus Unbound",
        title="The Living Eruption",
        dialogue=[
            "My SHELL was holding me BACK! I AM THE VOLCANO NOW!",
            "Cancel my PTO. This is PERSONAL.",
            "I'm literally just lava in a vaguely angry shape at this point.",
        ],
        win_odds_penalty=-0.10,
    ),
    100: BossPhase2Def(
        depth=100,
        name="The Void Unraveled",
        title="What Lies Beyond Nothing",
        dialogue=[
            "You defeated nothing. I AM nothing. How do you kill nothing?",
            "I un-existed. Now I un-un-exist. The math checks out.",
            "I'm a philosophical problem now. Good luck.",
        ],
        win_odds_penalty=-0.12,
    ),
    150: BossPhase2Def(
        depth=150,
        name="The Sporeling Collective",
        title="We Are Legion",
        dialogue=[
            "You killed one. We are MILLIONS. The mycelium REMEMBERS.",
            "We grew back. We always grow back. That's kind of our thing.",
            "Would you like to become one of us? The benefits are excellent.",
        ],
        win_odds_penalty=-0.12,
    ),
    200: BossPhase2Def(
        depth=200,
        name="Chronofrost Paradox",
        title="The Time That Bites Back",
        dialogue=[
            "You defeated me five minutes ago. I came back to before you did.",
            "This is the 47th time we've done this. You just don't remember.",
            "I've already won. I just haven't told you yet.",
        ],
        win_odds_penalty=-0.15,
    ),
    275: BossPhase2Def(
        depth=275,
        name="The Name Reclaimed",
        title="[DATA EXPUNGED]",
        dialogue=[
            "I remember my name now. It's yours.",
            "We are the same person. I'm just the part you buried.",
            "Take my hand. Let's dig together. Forever.",
        ],
        win_odds_penalty=-0.15,
    ),
}


# ---------------------------------------------------------------------------
# Boss Phase Gates & Phase 3 Definitions (boss revamp)
# ---------------------------------------------------------------------------
# Phase gates control when multi-phase boss fights unlock.
# Phase 2: P2+ on any tier (was P4). Phase 3: P5+ AND tier >= 100. Pinnacle
# is always 3-phase regardless of prestige.
BOSS_PHASES: dict[str, int | bool] = {
    "phase_2_min_prestige": 2,
    "phase_3_min_prestige": 5,
    "phase_3_min_tier": 100,
}


@dataclass(frozen=True)
class BossPhase3Def:
    """Endgame third phase for tier 100+ bosses at prestige 5+."""
    depth: int
    name: str
    title: str
    dialogue: list[str]
    win_odds_penalty: float


BOSS_PHASE3: dict[int, BossPhase3Def] = {
    100: BossPhase3Def(
        depth=100,
        name="The Void Itself",
        title="There Was Never Anything Here",
        dialogue=[
            "You unraveled me. The unraveling unravels in turn. Endless.",
            "There is no third phase. There is no second. There was no first.",
            "You are arguing with the carpet now. Good luck.",
        ],
        win_odds_penalty=-0.15,
    ),
    150: BossPhase3Def(
        depth=150,
        name="The Hivemind Awoken",
        title="Every Spore Speaks At Once",
        dialogue=[
            "We are a chorus. The chorus is a single voice. The voice is many.",
            "You are a single thread in our weave. We have eaten threads before.",
            "Every breath you take is a vote in our favor.",
        ],
        win_odds_penalty=-0.18,
    ),
    200: BossPhase3Def(
        depth=200,
        name="Chronofrost Rewound",
        title="The Loop That Forgot Itself",
        dialogue=[
            "I have already won. I have always already won. The verb is set.",
            "We have done this 1,032 times. You only remember one.",
            "Round three. The clock has unwound to zero.",
        ],
        win_odds_penalty=-0.20,
    ),
    275: BossPhase3Def(
        depth=275,
        name="The Final Erasure",
        title="[REDACTED] [REDACTED] [REDACTED]",
        dialogue=[
            "I take your name. I take your shape. I take the gap where you were.",
            "The depth gets the last word. The depth has always had the last word.",
            "You become a story told by the rocks. Be a good story.",
        ],
        win_odds_penalty=-0.20,
    ),
}


# ---------------------------------------------------------------------------
# Phase Transition Events (boss revamp)
# ---------------------------------------------------------------------------
# Drawn at random when a boss enters Phase 2 or Phase 3. Effects are
# applied to the in-progress encounter; flavor goes into the embed.

@dataclass(frozen=True)
class PhaseTransitionEvent:
    id: str
    flavor: str
    description: str
    # Effects applied to the duel mid-fight. Any unset key has no effect.
    player_hp_delta: int = 0
    boss_hp_delta: int = 0
    player_hit_offset: float = 0.0  # additive to player_hit for rest of fight
    boss_hit_offset: float = 0.0
    player_dmg_delta: int = 0
    boss_dmg_delta: int = 0
    luminosity_delta: int = 0       # one-shot tunnel luminosity adjustment


PHASE_TRANSITION_EVENTS: list[PhaseTransitionEvent] = [
    PhaseTransitionEvent(
        id="cave_in",
        flavor="Stalactites fracture overhead.",
        description="Both you and the boss take 1 HP damage.",
        player_hp_delta=-1, boss_hp_delta=-1,
    ),
    PhaseTransitionEvent(
        id="fissure",
        flavor="A magma fissure opens between you.",
        description="-5% player_hit for the remainder of the fight; -5 luminosity.",
        player_hit_offset=-0.05, luminosity_delta=-5,
    ),
    PhaseTransitionEvent(
        id="glowburst",
        flavor="A vein of phosphorus ignites.",
        description="+20 luminosity but the boss sees you better (+5% boss_hit).",
        boss_hit_offset=0.05, luminosity_delta=20,
    ),
    PhaseTransitionEvent(
        id="cold_snap",
        flavor="Frost sweeps through the chamber.",
        description="Both attacks weaken: -1 player_dmg, -1 boss_dmg.",
        player_dmg_delta=-1, boss_dmg_delta=-1,
    ),
    PhaseTransitionEvent(
        id="spore_cloud",
        flavor="Spores fill the air.",
        description="Sluggishness: -3% player_hit and -3% boss_hit.",
        player_hit_offset=-0.03, boss_hit_offset=-0.03,
    ),
    PhaseTransitionEvent(
        id="void_pull",
        flavor="Reality folds inward.",
        description="Both lose 2 HP.",
        player_hp_delta=-2, boss_hp_delta=-2,
    ),
]


# ---------------------------------------------------------------------------
# Pinnacle Boss (boss revamp)
# ---------------------------------------------------------------------------
# A new 8th boss boundary at depth 300 that gates prestige. One of three
# pinnacle candidates is rolled and locked per prestige cycle.
# Always 3 phases. Drops a relic with 2 random rolls on victory.

PINNACLE_DEPTH: int = 300
PINNACLE_RETREAT_FORESHADOW_DEPTH: int = 285  # /dig info hints from this depth
PINNACLE_FORESHADOW_DEPTH: int = 276          # subtle hint after T275 cleared


@dataclass(frozen=True)
class PinnaclePhaseDef:
    """One phase of a pinnacle boss."""
    archetype: str
    title: str
    transition_dialogue: list[str]
    mechanic_pool: tuple[str, ...] = ()


@dataclass(frozen=True)
class PinnacleBossDef:
    """A pinnacle boss candidate. One is rolled and locked per prestige cycle."""
    boss_id: str
    name: str
    persona: str
    ascii_art: str
    phases: tuple[PinnaclePhaseDef, PinnaclePhaseDef, PinnaclePhaseDef]


PINNACLE_BOSSES: dict[str, PinnacleBossDef] = {
    "forgotten_king": PinnacleBossDef(
        boss_id="forgotten_king",
        name="The Forgotten King",
        persona="ancient, dignified, hollowed by time",
        ascii_art=(
            "    .--^^--.\n"
            "   /  ::::  \\\n"
            "  | (o)  (o) |\n"
            "  |    /\\    |\n"
            "  |   '--'   |\n"
            "   \\  '__'  /\n"
            "    '------'\n"
            "      ||||\n"
            "    ##====##"
        ),
        phases=(
            PinnaclePhaseDef(
                archetype="tank",
                title="The Forgotten King",
                transition_dialogue=[],
                mechanic_pool=("king_decree",),
            ),
            PinnaclePhaseDef(
                archetype="glass_cannon",
                title="The Crowned Hunger",
                transition_dialogue=[
                    "The crown burns. I am hungry now. Forgive me.",
                    "Decorum slips. Hunger speaks.",
                ],
                mechanic_pool=("king_feast",),
            ),
            PinnaclePhaseDef(
                archetype="slippery",
                title="The Last Breath of Kings",
                transition_dialogue=[
                    "Last breath. Last lesson. Pay attention.",
                    "I die slowly. You will witness.",
                ],
                mechanic_pool=(
                    "king_deathbed",
                    "pinnacle_arithmetic_challenge",
                    "pinnacle_riddle_challenge",
                ),
            ),
        ),
    ),
    "hollowforged": PinnacleBossDef(
        boss_id="hollowforged",
        name="Hollowforged",
        persona="the depth made flesh, plural, mineral",
        ascii_art=(
            "  /\\/\\/\\/\\/\\/\\\n"
            " /            \\\n"
            "|  __    __    |\n"
            "| (oo)  (oo)   |\n"
            "|              |\n"
            " \\  ========  /\n"
            "  \\__________/\n"
            "    ||    ||\n"
            "   ###    ###"
        ),
        phases=(
            PinnaclePhaseDef(
                archetype="bruiser",
                title="Hollowforged",
                transition_dialogue=[],
                mechanic_pool=("hollow_walls_close",),
            ),
            PinnaclePhaseDef(
                archetype="tank",
                title="Hollowforged Reformed",
                transition_dialogue=[
                    "Reform. The mine has new walls now.",
                    "The walls speak in a different dialect.",
                ],
                mechanic_pool=("hollow_shape_shift",),
            ),
            PinnaclePhaseDef(
                archetype="slippery",
                title="Hollowforged Pluralized",
                transition_dialogue=[
                    "Plural. The depth is many things at once.",
                    "We are the chamber and the wall and the air.",
                ],
                mechanic_pool=(
                    "hollow_many_voices",
                    "pinnacle_arithmetic_challenge",
                    "pinnacle_riddle_challenge",
                ),
            ),
        ),
    ),
    "first_digger": PinnacleBossDef(
        boss_id="first_digger",
        name="The First Digger",
        persona="gaunt, manic, the one who never came back up",
        ascii_art=(
            "       /\\\n"
            "      /  \\\n"
            "     /    \\\n"
            "    | O  O |\n"
            "    |  /\\  |\n"
            "     \\ -- /\n"
            "      \\__/\n"
            "       ||\n"
            "    ___||___\n"
            "   |________|"
        ),
        phases=(
            PinnaclePhaseDef(
                archetype="glass_cannon",
                title="The First Digger",
                transition_dialogue=[],
                mechanic_pool=("digger_pickaxe_duel",),
            ),
            PinnaclePhaseDef(
                archetype="slippery",
                title="The Digger Unbound",
                transition_dialogue=[
                    "Unbound. The pickaxe is no longer needed.",
                    "I dig with my hands now. Cleaner.",
                ],
                mechanic_pool=("digger_phasing",),
            ),
            PinnaclePhaseDef(
                archetype="glass_cannon",
                title="The Digger Eternal",
                transition_dialogue=[
                    "Eternal. The tunnel is me. I am the tunnel.",
                    "Last shift. Last dig. Last.",
                ],
                mechanic_pool=(
                    "digger_tunnel_collapse",
                    "pinnacle_arithmetic_challenge",
                    "pinnacle_riddle_challenge",
                ),
            ),
        ),
    ),
}

PINNACLE_POOL_IDS: tuple[str, ...] = ("forgotten_king", "hollowforged", "first_digger")


# Pinnacle relic — random 2 stats from this pool, name = base + suffix.
@dataclass(frozen=True)
class RelicStatRoll:
    """Possible stat roll for a pinnacle relic. effects keys feed into combat helpers."""
    id: str
    label: str
    effects: dict   # e.g. {"player_hp_bonus": 1} or {"jc_multiplier": 0.05}


PINNACLE_RELIC_STAT_POOL: tuple[RelicStatRoll, ...] = (
    # Combat
    RelicStatRoll("hp_plus_1",        "+1 player HP",
                  {"player_hp_bonus": 1}),
    RelicStatRoll("hit_plus_002",     "+0.02 player_hit",
                  {"player_hit_bonus": 0.02}),
    RelicStatRoll("dmg_plus_per_100", "+1 player_dmg per 100 depth",
                  {"player_dmg_per_100_depth": 1}),
    RelicStatRoll("boss_hit_minus",   "-0.02 boss_hit",
                  {"boss_hit_offset": -0.02}),
    RelicStatRoll("boss_hp_minus_10", "-10% boss starting HP",
                  {"boss_hp_multiplier": -0.10}),
    RelicStatRoll("boss_payout_5",    "+5% boss payout",
                  {"boss_payout_bonus": 0.05}),
    # Dig
    RelicStatRoll("jc_plus_5",        "+5% JC on dig",
                  {"jc_multiplier": 0.05}),
    RelicStatRoll("cave_in_minus_5",  "-5% cave-in chance",
                  {"cave_in_reduction": 0.05}),
    RelicStatRoll("lum_refill_2",     "+2 luminosity refill per day",
                  {"lum_refill_bonus": 2}),
    RelicStatRoll("durability_minus", "-10% gear durability tick",
                  {"durability_reduction": 0.10}),
    RelicStatRoll("inventory_plus_1", "+1 inventory slot",
                  {"inventory_bonus": 1}),
    # Utility
    RelicStatRoll("streak_immunity",  "Once per delve, streak does not break on missed day",
                  {"streak_immunity": True}),
    RelicStatRoll("extra_relic_slot", "+1 equipped-relic slot",
                  {"relic_slot_bonus": 1}),
    RelicStatRoll("scout_free",       "Boss scout costs 0 cooldown",
                  {"scout_free": True}),
    RelicStatRoll("cheer_buff",       "Cheers from others give +6% (was +5%)",
                  {"cheer_bonus": 0.01}),
)

PINNACLE_RELIC_SUFFIX_POOL: tuple[str, ...] = (
    "Echoes", "Hunger", "Patience", "Ruin", "Bloom",
    "Silence", "Endings", "First Light", "Last Breath", "Hollow",
    "Persistence", "Forgotten Things",
)

PINNACLE_RELIC_BASE_NAME: dict[str, str] = {
    "forgotten_king": "Crown",
    "hollowforged":   "Heart",
    "first_digger":   "Pickaxe",
}

# Flat JC reward layered on top of the relic drop.
PINNACLE_BASE_JC_REWARD: int = 500
PINNACLE_JC_PER_PRESTIGE: int = 100


# ---------------------------------------------------------------------------
# Retreat Cost (boss revamp)
# ---------------------------------------------------------------------------
# Retreat now costs depth blocks + a cooldown so it can't be spammed for
# free intel after we add persisted boss HP.
RETREAT_BLOCK_LOSS_MIN: int = 2
RETREAT_BLOCK_LOSS_MAX: int = 3
RETREAT_COOLDOWN_SECONDS: int = 30 * 60   # 30 minutes


# ---------------------------------------------------------------------------
# Persisted boss HP / regen (boss revamp)
# ---------------------------------------------------------------------------
# Halved from "1 HP / hour" so damage actually persists across the new
# shorter free-dig cycle. A 24h hiatus now regens half a phase, not a full one.
BOSS_HP_REGEN_PER_2_HOURS: int = 1


# ---------------------------------------------------------------------------
# Decay Constants
# ---------------------------------------------------------------------------

# Blocks lost per day of inactivity, per layer (after first 24h)
DECAY_RATE_PER_DAY: dict[str, int] = {
    "Dirt": 1,
    "Stone": 2,
    "Crystal": 3,
    "Magma": 4,
    "Abyss": 5,
    "Fungal Depths": 6,
    "Frozen Core": 7,
    "The Hollow": 8,
}

DECAY_START_HOURS: int = 24                        # decay begins after this
DECAY_ACCELERATED_HOURS: int = 72                  # 2x rate after this
DECAY_ACCELERATED_MULTIPLIER: float = 2.0
DECAY_FLOOR_DEPTHS: list[int] = [25, 50, 75, 100, 150, 200]  # decay cannot cross these
DECAY_HELPER_REDUCTION: float = 0.5                 # per helper in last 24h
DECAY_HELPER_MIN_MULTIPLIER: float = 0.25           # floor on helper reduction


# ---------------------------------------------------------------------------
# Sabotage / Defense Constants
# ---------------------------------------------------------------------------

SABOTAGE_BASE_COST: int = 5
SABOTAGE_COST_DIVISOR: int = 5             # cost = max(SABOTAGE_BASE_COST, target_depth // SABOTAGE_COST_DIVISOR)
SABOTAGE_DAMAGE_MIN: int = 3
SABOTAGE_DAMAGE_MAX: int = 8
SABOTAGE_COOLDOWN_SECONDS: int = 43_200    # 12 hours

INSURANCE_BASE_COST: int = 5
INSURANCE_COST_DEPTH_DIVISOR: int = 25     # cost = INSURANCE_BASE_COST + depth // INSURANCE_COST_DEPTH_DIVISOR
INSURANCE_DURATION_SECONDS: int = 86_400   # 24 hours
INSURANCE_REDUCTION: float = 0.50
REINFORCEMENT_SABOTAGE_REDUCTION: float = 0.25
MAX_COMBINED_SABOTAGE_REDUCTION: float = 0.70

TRAP_BASE_COST: int = 8
TRAP_COST_DEPTH_DIVISOR: int = 4           # cost = max(TRAP_BASE_COST, depth // TRAP_COST_DEPTH_DIVISOR)

REVENGE_DISCOUNT_WINDOW_SECONDS: int = 3600   # 1 hour
REVENGE_FREE_WINDOW_SECONDS: int = 1800       # 30 minutes


# ---------------------------------------------------------------------------
# Cave-in Constants
# ---------------------------------------------------------------------------

CAVE_IN_BLOCK_LOSS_MIN: int = 6
CAVE_IN_BLOCK_LOSS_MAX: int = 14
CAVE_IN_STUN_HOURS_MIN: int = 2
CAVE_IN_STUN_HOURS_MAX: int = 4
CAVE_IN_MEDICAL_BILL_DIVISOR: int = 6       # cost = max(1, depth // divisor)
CAVE_IN_MEDICAL_BILL_MIN: int = 1


# ---------------------------------------------------------------------------
# Injuries
# ---------------------------------------------------------------------------

INJURY_TYPES: list[str] = ["reduced_advance", "slower_cooldown", "layer_debuff"]

INJURY_DURATIONS: dict[str, dict[str, int]] = {
    "reduced_advance": {"digs": 3},
    "slower_cooldown": {"hours": 24, "cooldown_hours": 6},
    "layer_debuff": {"digs": 3},
}


# ---------------------------------------------------------------------------
# Miscellaneous
# ---------------------------------------------------------------------------

MAX_INVENTORY_SLOTS: int = 8
ABANDON_COOLDOWN_SECONDS: int = 86_400     # 24 hours
ABANDON_MIN_DEPTH: int = 10
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


# ---------------------------------------------------------------------------
# Compatibility Aliases
# ---------------------------------------------------------------------------
# The service layer uses simpler dict-based lookups. These aliases bridge
# the gap between the structured dataclass definitions above and the
# dict-based access patterns used in dig_service.py.

# LAYERS as dicts for service-layer dict-style access
LAYERS: list[dict] = [
    {
        "name": ld.name, "min_depth": ld.depth_min, "max_depth": ld.depth_max,
        "cave_in_pct": ld.cave_in_pct, "jc_min": ld.jc_min, "jc_max": ld.jc_max,
        "advance_min": ld.advance_min, "advance_max": ld.advance_max, "emoji": ld.emoji,
    }
    for ld in _LAYERS_DEF
]

FREE_DIG_COOLDOWN: int = FREE_DIG_COOLDOWN_SECONDS
PAID_DIG_COSTS: list[int] = PAID_DIG_COSTS_PER_DAY
MAX_INVENTORY_SIZE: int = MAX_INVENTORY_SLOTS
INJURY_SLOW_COOLDOWN: int = 6 * 3600  # 6 hours in seconds (injury slower cooldown)

BOSS_BOUNDARIES: list[int] = LAYER_BOUNDARIES  # [25, 50, 75, 100, 150, 200, 275]
BOSS_DEPTHS: list[int] = LAYER_BOUNDARIES

# All encounter boundaries including the pinnacle. Used by service layer to
# detect when to trigger any boss (regular or pinnacle).
ALL_BOSS_BOUNDARIES: list[int] = LAYER_BOUNDARIES + [PINNACLE_DEPTH]

BOSS_NAMES: dict[int, str] = {d: b.name for d, b in BOSSES.items()}
BOSS_DIALOGUE: dict[int, list[str]] = {d: b.dialogue for d, b in BOSSES.items()}
BOSS_ASCII: dict[int, str] = {d: b.ascii_art for d, b in BOSSES.items()}


# ---------------------------------------------------------------------------
# Boss Dialogue V2 (boss revamp, pre-generated)
# ---------------------------------------------------------------------------
# Per-boss dialogue keyed by slot:
#   first_meet: line on first encounter this delve (resets on prestige)
#   after_defeat: last fight player won (boss may have been weakened)
#   after_retreat: last fight player retreated
#   after_close_win: last fight player won with low win-prob (<0.6)
#   after_scout: last action was scout
# Tokens are substituted at render time:
#   {streak}, {depth}, {prestige}, {killed_boss_name}.
BOSS_DIALOGUE_V2: dict[str, dict[str, list[str]]] = {
    # ---- Tier 25 -------------------------------------------------------
    "grothak": {
        "first_meet": [
            "I have stood here longer than you have been alive. Continue.",
            "You came down. Most go up. I respect this.",
            "I am Grothak. You are not. Begin.",
        ],
        "after_defeat": [
            "Again. I will not break. You might.",
            "Streak {streak} days, you say? I have stood here {streak} centuries.",
            "Round two. I have weight. You have intent.",
        ],
        "after_retreat": [
            "You left. The stone remembers.",
            "Patience. I am not hard to find.",
            "You will be back. They always are.",
        ],
        "after_close_win": [
            "A chip is not a crack. Try again.",
            "You bled me. Acceptable. Not enough.",
            "I felt that. I have not felt that in some time.",
        ],
        "after_scout": [
            "Looking? Look. I am unbothered.",
            "Note my stance. It will not change.",
        ],
    },
    "pudge": {
        "first_meet": [
            "You look stringy. Maybe with sauce.",
            "Fresh meat. Don't run. You'll just sweat.",
            "I haven't eaten today. You'll do.",
        ],
        "after_defeat": [
            "You! ...have you been working out?",
            "Lucky. The hook was wet.",
            "Round two. Bring friends. Or don't.",
        ],
        "after_retreat": [
            "Run! It only adds flavor.",
            "Coward soup. My favorite.",
            "Smart. I'd run from me too.",
        ],
        "after_close_win": [
            "Scratched. You're learning.",
            "Bleeding? Both of us. Cute.",
            "I almost had you. Almost.",
        ],
        "after_scout": [
            "Watching me eat? Weirdo.",
            "Take notes. There's a quiz.",
        ],
    },
    "ogre_magi": {
        "first_meet": [
            "Hi! ...wait, who are you?",
            "Left head says fight. Right head says snacks. Compromise: fight snack.",
            "FIRE! ...what was I doing?",
        ],
        "after_defeat": [
            "We saw you yesterday. We forgot you today. Hi again!",
            "Did you win last time? Don't tell us. We don't believe you.",
            "Streak {streak} days! Both heads agree we hate that.",
        ],
        "after_retreat": [
            "You ran! Or arrived! Hard to say!",
            "Goodbye! Or hello! Same thing!",
            "We won? We didn't win? Doesn't matter, FIRE!",
        ],
        "after_close_win": [
            "Multicast: ow ow ow.",
            "We meant to do that. Both heads agree. Probably.",
            "You're hard to forget. We'll work on it.",
        ],
        "after_scout": [
            "You're staring. We like staring. STARE BACK.",
            "Two heads, two opinions on you. Both bad.",
        ],
    },
    # ---- Tier 50 -------------------------------------------------------
    "crystalia": {
        "first_meet": [
            "Do you see how the light loves me? It will not love you.",
            "I have a thousand faces. None of them like yours.",
            "Approach. Watch yourself approach. Watch yourself approach. Watch yourself—",
        ],
        "after_defeat": [
            "You chipped me. The chip is more beautiful than you.",
            "Refracted again. The mirror reverses. So will I.",
            "{streak} days of digging and you bring this light to me. Tasteless.",
        ],
        "after_retreat": [
            "Run. The crystal reflects everything, including the back of your head.",
            "Half a hundred faces watched you flee. They will gossip.",
            "You return to surface daylight. I pity you.",
        ],
        "after_close_win": [
            "A facet broken. I have nine hundred and ninety-nine others.",
            "Light bleeds. I bleed. Cute symmetry.",
            "You're sharper than I thought. Not as sharp as me.",
        ],
        "after_scout": [
            "Gawker. Make a wish.",
            "I see you in fragments. Most of them are unflattering.",
        ],
    },
    "crystal_maiden": {
        "first_meet": [
            "Stand still. The cold finds the still.",
            "I'm small. The fields I cast are not.",
            "Wave hello. It'll be the last time you wave with both arms.",
        ],
        "after_defeat": [
            "You melted me. Rude. I'll re-form by Tuesday.",
            "I'll remember the warmth of your win. Briefly.",
            "Round two. Bring a coat.",
        ],
        "after_retreat": [
            "Run. I'll catch up. Frost is patient.",
            "Goodbye. The glaciers I made are still here.",
            "Coward! ...wait, sensible? I respect both.",
        ],
        "after_close_win": [
            "You felt the field! Now you're afraid.",
            "Survived? Lucky. The cold remembers your gait.",
            "Almost. Almost is colder than 'no'.",
        ],
        "after_scout": [
            "Studying my robes? They're insulated. Yours aren't.",
            "Don't blink. I freeze the eyelashes first.",
        ],
    },
    "tusk": {
        "first_meet": [
            "WALRUS PUNCH WARM-UP! You'll do as the tackle dummy.",
            "Hahaha! Fresh blood for the snowfield.",
            "You came down here in those? Bold. Stupid. I respect it.",
        ],
        "after_defeat": [
            "Round two! I've packed harder snowballs!",
            "{streak} days of digging and you've still got soft hands. Cute.",
            "You won. I respect winners. Now eat snow.",
        ],
        "after_retreat": [
            "Run! I'll roll downhill after you!",
            "Cold feet, eh? Mine never get cold.",
            "Tusk waits. Tusk is patient. Tusk is also bored.",
        ],
        "after_close_win": [
            "You took the punch! You stood up! Mostly!",
            "Bruised but proud. That's the way.",
            "I felt that. Want to feel mine?",
        ],
        "after_scout": [
            "Squinting? My armor is thick. Your eyes are not.",
            "Ho ho! A scout! Be sure to scout the fist.",
        ],
    },
    # ---- Tier 75 -------------------------------------------------------
    "magmus_rex": {
        "first_meet": [
            "Bow or burn. Either is acceptable.",
            "You bring iron into a furnace. Charming.",
            "I have been king longer than your line has had names.",
        ],
        "after_defeat": [
            "You scorched me. The throne has a new dent.",
            "Round two. The crown is heavier this time.",
            "Streak {streak} days and still you crawl back. Persistent rats are still rats.",
        ],
        "after_retreat": [
            "Withdraw. The lava has memory. So do I.",
            "Hot under your collar? Try mine.",
            "You will be back. The mantle pulls everything down eventually.",
        ],
        "after_close_win": [
            "A spark off my crown. The crown remains.",
            "Embers. You are bringing me embers. Adorable.",
            "I felt warmth. Strange — I am warmth.",
        ],
        "after_scout": [
            "Look. I am unconcerned. Look longer if you wish.",
            "Inspect my regalia. It survives diggers like you.",
        ],
    },
    "lina": {
        "first_meet": [
            "I've been waiting. Don't bore me.",
            "You're early. I haven't finished applying my eyeliner.",
            "Make this fun. Make this fast. Pick one.",
        ],
        "after_defeat": [
            "You won? Let me check. ...rude.",
            "Defeated by depth-{depth} dirt. The shame.",
            "Round two. I'm bringing the dragon this time.",
        ],
        "after_retreat": [
            "Run! I'll burn brighter while you're gone!",
            "Goodbye. Take your retreat with a side of fire.",
            "Patience is a fuel. I have plenty.",
        ],
        "after_close_win": [
            "Singed but standing. Cute outfit, by the way.",
            "Almost combusted. I respect almost.",
            "You'll need ointment. I have a recommendation.",
        ],
        "after_scout": [
            "Watch closely. The next one is faster.",
            "Don't blink. The flash blinds easily.",
        ],
    },
    "doom": {
        "first_meet": [
            "Hello. I am Doom. Goodbye.",
            "Your name. I'd like it for the list.",
            "You will burn. I'll wait while you process this.",
        ],
        "after_defeat": [
            "Last round you survived. This round, less likely.",
            "Mark renewed. Streak {streak} noted in the ledger.",
            "I underestimated you. I will adjust.",
        ],
        "after_retreat": [
            "Branded. You carry me with you now.",
            "Run. The mark catches up.",
            "You will return. Branded things always do.",
        ],
        "after_close_win": [
            "A scratch. The brand still burns under it.",
            "Almost. The list is patient.",
            "You bleed neatly. I appreciate that.",
        ],
        "after_scout": [
            "Look. The brand is patient.",
            "Memorize my face. It will be the last polite one you see.",
        ],
    },
    # ---- Tier 100 ------------------------------------------------------
    "void_warden": {
        "first_meet": [
            "I am between two thoughts. You arrived in the gap.",
            "Hello. Or have we already had this conversation. I forget the order.",
            "Step closer. Or further. The geometry is forgiving.",
        ],
        "after_defeat": [
            "I lost. Or I lost. Or I will lose. The verbs blur.",
            "Streak {streak} days. The streak is also a line. Lines fold.",
            "You won. The previous you won. The next you may not.",
        ],
        "after_retreat": [
            "You retreat. Ahead, behind. Same direction here.",
            "The void does not chase. It anticipates.",
            "Goodbye. Or hello in another moment.",
        ],
        "after_close_win": [
            "You bled the right amount. Coincidence is generous.",
            "I admit confusion. I admit it backwards too.",
            "Close. Closer than the math allowed.",
        ],
        "after_scout": [
            "You watch. I am also watching. We have always been watching.",
            "The geometry approves of your inspection. The Warden does not.",
        ],
    },
    "spectre": {
        "first_meet": [
            "...",
            "(The shade does not greet. It haunts.)",
            "You step into a doorway you did not see. There is no door.",
        ],
        "after_defeat": [
            "You ended me. I have been ended before. It does not stop me long.",
            "Streak {streak} days. You haunt the depths. So do I.",
            "Vengeance has been delayed. Not denied.",
        ],
        "after_retreat": [
            "You leave. I am already with you.",
            "Footsteps fade. Mine do not.",
            "The shade always follows.",
        ],
        "after_close_win": [
            "Surprised? I bleed shadow.",
            "Almost yours. Almost mine.",
            "A near miss. I have eternity.",
        ],
        "after_scout": [
            "Look closer. There is more of me than you see.",
            "I am behind you. And in front. Pick.",
        ],
    },
    "void_spirit": {
        "first_meet": [
            "Hi! Or — wait, is it 'bye'? Always confuses me.",
            "I'm the echo. The original is busy.",
            "Stand still! Or don't. Either way I'll be where you aren't.",
        ],
        "after_defeat": [
            "Caught me. The original will be embarrassed.",
            "{streak} days digging and you found a glitch. Nice.",
            "Score: you 1, the lattice 0. The lattice is stubborn.",
        ],
        "after_retreat": [
            "Bye! See you in the next chamber!",
            "You're rotating, but I'm rotating faster.",
            "Goodbye! Or hello! Both! Neither!",
        ],
        "after_close_win": [
            "Scratched the lattice. The lattice does not forget.",
            "Almost phased through me. Almost.",
            "I felt your edge. Mostly through your edge.",
        ],
        "after_scout": [
            "Hello, the watcher! Here, here, here, here.",
            "Pick a me. They're all valid.",
        ],
    },
    # ---- Tier 150 ------------------------------------------------------
    "sporeling_sovereign": {
        "first_meet": [
            "We are many. You are alone. We forgive this.",
            "Welcome to the bloom. Mind the spores. They mind you.",
            "Approach. The mycelium catalogs you.",
        ],
        "after_defeat": [
            "You harvested me. The spores remember harvest.",
            "Round two. We have re-bloomed in your absence.",
            "Streak {streak} days. We are a streak too. Older.",
        ],
        "after_retreat": [
            "Leave. We are also outside, in places you have walked.",
            "Goodbye. You take spores with you.",
            "Retreat, watered properly, becomes a return.",
        ],
        "after_close_win": [
            "Bruised the bloom. The bloom is patient.",
            "A petal lost. We have many petals.",
            "Closer than expected. We will adjust.",
        ],
        "after_scout": [
            "Watch the bloom. The bloom watches back.",
            "Inspect the spore-clouds. They take notes.",
        ],
    },
    "treant_protector": {
        "first_meet": [
            "Little digger. Why so deep?",
            "The roots heard you coming. Patience, child.",
            "Welcome. Try not to chop anything.",
        ],
        "after_defeat": [
            "You bested an elder. I am surprised. And amused.",
            "{streak} days underground and still strong. The sun would suit you.",
            "Round two. The grove has fed.",
        ],
        "after_retreat": [
            "Go. The grove is patient. Trees outlast.",
            "Roots remember. They are still under your boots.",
            "Return whenever. The grove will be here.",
        ],
        "after_close_win": [
            "A leaf fell. I have many leaves.",
            "You scraped bark. Bark grows back.",
            "Closer than I expected. Charmed.",
        ],
        "after_scout": [
            "Look. The grove is unchanged. Mostly.",
            "Examine the rings. There are many, like your scars.",
        ],
    },
    "broodmother": {
        "first_meet": [
            "Welcome, dear. The nest is a little sticky today.",
            "So small. So protein-rich.",
            "Don't mind the children. They mind themselves.",
        ],
        "after_defeat": [
            "You broke a thread. The web has many.",
            "Streak {streak}? Impressive. The children would like to study you. Closely.",
            "Round two. We are hungrier.",
        ],
        "after_retreat": [
            "Go. The web is sticky. You'll bring some with you.",
            "Bye-bye, little dinner. Tell your friends.",
            "Run. The little ones love a chase.",
        ],
        "after_close_win": [
            "Bit me, did you? Cheeky.",
            "A leg lost. I have eight. Plenty.",
            "Closer than we anticipated. The children are impressed.",
        ],
        "after_scout": [
            "Watch the nest. The nest watches you.",
            "Counting eggs? Don't. It's rude.",
        ],
    },
    # ---- Tier 200 ------------------------------------------------------
    "chronofrost": {
        "first_meet": [
            "I have been in this exact second for some time.",
            "You arrive. The second arrives also. They are the same.",
            "Welcome. The fight has already started, technically.",
        ],
        "after_defeat": [
            "You won at second 0.347. I have logged it.",
            "Streak {streak} days. I have streaks too. Mine are colder.",
            "Round two. The same second, refreshed.",
        ],
        "after_retreat": [
            "Leave. I am still in the second. I will be when you return.",
            "Time accommodates retreat. Time also accommodates pursuit.",
            "Goodbye. I will not move. I will not need to.",
        ],
        "after_close_win": [
            "A close second. Pun intended.",
            "You scratched 0.001 of me. The other 0.999 disagrees.",
            "Almost. The clock froze on 'almost'.",
        ],
        "after_scout": [
            "Observe. I am still. You are not.",
            "Take your time. I have all of it.",
        ],
    },
    "faceless_void": {
        "first_meet": [
            "...",
            "(The Timeless does not greet. The Timeless arrives.)",
            "You step into a stopped second. Adjust.",
        ],
        "after_defeat": [
            "You found a gap in the chronosphere. I will close it.",
            "{streak} days. A streak is a kind of timeline. I cut timelines.",
            "Round two. The clock will not be merciful.",
        ],
        "after_retreat": [
            "Backtrack. I do that for a living.",
            "Leave. The chronosphere closes anyway.",
            "Goodbye. Time does not chase. Time waits.",
        ],
        "after_close_win": [
            "Scratched. The damage is in the past now. Both pasts.",
            "Closer than the math. The math will adjust.",
            "Almost. Almost is its own dimension.",
        ],
        "after_scout": [
            "Look. The Timeless is unmoved.",
            "Watch closely. I will not blink because I will not.",
        ],
    },
    "weaver": {
        "first_meet": [
            "Stitch stitch stitch. You arrived in my pattern.",
            "Hi. Little weft, little warp, little you.",
            "The thread says you are interesting. I disagree.",
        ],
        "after_defeat": [
            "Pulled a stitch out of me! Naughty digger!",
            "Streak {streak} days. I have woven {streak} layers around your tunnel.",
            "Round two. The pattern has more knots now.",
        ],
        "after_retreat": [
            "Run! The thread comes with you! Pull, pull!",
            "Goodbye! Or hello, depending on which thread you take!",
            "Bye! I will be in another moment. Always am.",
        ],
        "after_close_win": [
            "A close weave. The pattern shivered.",
            "You almost fell out of time. You still might.",
            "Stitched yourself up. Cute. Mine looks like spaghetti.",
        ],
        "after_scout": [
            "Watching the threads? They watch back. They gossip.",
            "Don't pull on any. You'll regret which one.",
        ],
    },
    # ---- Tier 275 ------------------------------------------------------
    "nameless_depth": {
        "first_meet": [
            "I have no name. You will not provide one.",
            "Approach. Names fall off here.",
            "(Silence so heavy it weighs on your tongue.)",
        ],
        "after_defeat": [
            "You won. The verb does not survive me. Neither will the noun.",
            "Streak {streak}. The number is also forgotten now.",
            "Round two. The silence is louder.",
        ],
        "after_retreat": [
            "You retreat. The depth follows in your unspoken thoughts.",
            "Leave. The Nameless does not pursue. The Nameless waits.",
            "Goodbye. The word departs. The depth remains.",
        ],
        "after_close_win": [
            "A close end. Ends are my specialty.",
            "Almost the bottom. Almost is also a depth.",
            "You bled. The bleeding has no name either.",
        ],
        "after_scout": [
            "Look. There is nothing to see. Look longer.",
            "Inspect the silence. The silence inspects you.",
        ],
    },
    "oracle": {
        "first_meet": [
            "Hello. I knew you'd say nothing back. Disappointed but not surprised.",
            "You arrive. The omen said depth-{depth}. The omen is annoying.",
            "Sit. Not there. There. Yes. The vision said so.",
        ],
        "after_defeat": [
            "You won. Yes. I told myself.",
            "{streak} days of digging. The tea leaves predicted exactly that. Or nothing. Or both.",
            "Round two. I will lose differently this time.",
        ],
        "after_retreat": [
            "You leave. I saw this. Twice. Once with feeling.",
            "Goodbye. The omens are also leaving.",
            "Retreat. Foretold. Boring.",
        ],
        "after_close_win": [
            "Close. The vision said 'close'. Annoyingly accurate.",
            "Bled. The omens warned me. I ignored them.",
            "Almost. The omens warn me of all almosts.",
        ],
        "after_scout": [
            "Stare. I can't see. You're staring at the blindfold.",
            "Watching me? The blindfold watches you. Don't blink.",
        ],
    },
    "terrorblade": {
        "first_meet": [
            "Kneel. The throne is gone. The protocol remains.",
            "You disturb royalty. Royalty disapproves.",
            "I was a prince. Now I am a problem.",
        ],
        "after_defeat": [
            "You unmade my unmaking. The math is poetry.",
            "Streak {streak}. The crown survived longer.",
            "Round two. The illusions will be unkinder.",
        ],
        "after_retreat": [
            "Run. A prince is patient about pursuits.",
            "Goodbye. The illusion of you is still here, mocking.",
            "Coward, by my definition. By yours, sensible.",
        ],
        "after_close_win": [
            "A near-sundering. Of me, this time.",
            "Closer than I dignify.",
            "Almost. The almost has a kind of beauty.",
        ],
        "after_scout": [
            "Watching? The illusions also watch. They are catty.",
            "Inspect closely. Royalty rewards attention.",
        ],
    },
    # ---- Pinnacle pool (depth 300) -------------------------------------
    "forgotten_king": {
        "first_meet": [
            "Hello, child. You have walked far. Sit. No, stand. I forget which is the etiquette.",
            "I am a king without a kingdom. You are a digger without an end. We are family of a kind.",
            "Welcome to the throne. The throne is also the bottom of the mine. They are the same room.",
        ],
        "after_defeat": [
            "You ended a king. Streak {streak} of kings, perhaps.",
            "I lost. Royalty does not lose, except when it does.",
            "Round two. The crown is on tighter.",
        ],
        "after_retreat": [
            "You leave royalty mid-audience. Bold.",
            "Go. I will resume my soliloquy.",
            "The court does not chase. The court endures.",
        ],
        "after_close_win": [
            "A close court. The protocol wavered.",
            "You drew royal blood. Rare honor. Rude.",
            "Closer than I have come to ending in some time.",
        ],
        "after_scout": [
            "Inspect the throne. It is a chair. It is also a tomb.",
            "Look at me. The crown does not enjoy attention. I do.",
        ],
    },
    "hollowforged": {
        "first_meet": [
            "We are the mine. The mine has decided to talk back.",
            "You dig us. We dig you back.",
            "Welcome, surface thing. The walls have an opinion.",
        ],
        "after_defeat": [
            "You broke a wall. The wall reforms. The wall is patient.",
            "Round two. We have more walls.",
            "Streak {streak}. Walls also have streaks. Geological ones.",
        ],
        "after_retreat": [
            "Leave. The walls follow. They are slow but committed.",
            "Goodbye. The mine is endless. You will be back.",
            "Retreat is dug too. Welcome.",
        ],
        "after_close_win": [
            "Cracked, not collapsed.",
            "A near-cave-in. We respect the geometry.",
            "Almost. Almost is also a layer.",
        ],
        "after_scout": [
            "Examine the walls. The walls examine you back.",
            "Inspect the rocks. They are taking a head count.",
        ],
    },
    "first_digger": {
        "first_meet": [
            "Oh! Another one! Hello! Don't go up. Don't ever go up.",
            "I started this tunnel. You're in it. Lovely.",
            "First time, eh? Mine was a Tuesday. Long Tuesday.",
        ],
        "after_defeat": [
            "You won. I lost. Wait. Did I want to lose?",
            "{streak} days. Pretender. I have {streak} centuries.",
            "Round two. I dug while you slept.",
        ],
        "after_retreat": [
            "Going up? Don't. The light is wrong now.",
            "Goodbye. The tunnel is mine when you leave it.",
            "Retreat? I retreated once. Then I dug here. Look how that turned out.",
        ],
        "after_close_win": [
            "A close one. The pickaxe is hungry.",
            "Almost. Almost is the depth I prefer.",
            "Closer than I've been to surface in centuries.",
        ],
        "after_scout": [
            "Watching me dig? Take notes. Mostly: don't.",
            "Inspect. Yes. Inspect the hole. The hole inspects you.",
        ],
    },
}


# Subtle pinnacle foreshadowing lines for /dig info, post-T275 clear.
PINNACLE_FORESHADOW_LINES: tuple[str, ...] = (
    "Something stirs below.",
    "The dark hums in a frequency you can almost hear.",
    "A pressure builds in the rock ahead.",
    "Your lantern flame leans, like wind — but there is no wind.",
)

# Consumable items as dicts for service-layer lookups
CONSUMABLE_ITEMS: dict[str, dict] = {
    c.id: {"name": c.name, "cost": c.cost, "description": c.description, "params": c.params}
    for c in CONSUMABLES.values()
}
ITEM_PRICES: dict[str, int] = {c.id: c.cost for c in CONSUMABLES.values()}

# Artifact pool as dicts
ARTIFACT_POOL: list[dict] = [
    {
        "id": a.id, "name": a.name, "layer": a.layer, "rarity": a.rarity,
        "lore_text": a.lore_text, "is_relic": a.is_relic, "effect": a.effect,
    }
    for a in ALL_ARTIFACTS
]

# Events as dicts
def _choice_to_dict(c: EventChoice) -> dict:
    """Convert an EventChoice to a dict for service-layer access."""
    return {
        "label": c.label,
        "success": {"description": c.success.description, "advance": c.success.advance, "jc": c.success.jc, "cave_in": c.success.cave_in},
        "failure": {"description": c.failure.description, "advance": c.failure.advance, "jc": c.failure.jc, "cave_in": c.failure.cave_in} if c.failure else None,
        "success_chance": c.success_chance,
    }


# ---------------------------------------------------------------------------
# Event ASCII Art Registry (roguelike dungeon crawler style)
# ---------------------------------------------------------------------------

EVENT_ASCII_ART: dict[str, str] = {
    # --- Original events ---
    "underground_stream": (
        "##########.......##########\n"
        "#........~~~~~~~~~~~~....#\n"
        "#..@.....~~~~~~~~~~~~....#\n"
        "#........~~~~~~~~~~~~..$.#\n"
        "##########.......##########"
    ),
    "gas_pocket": (
        "###########################\n"
        "#..........*...*..........#\n"
        "#..@......*.*.*.*...???...#\n"
        "#.........*...*..........#\n"
        "###########################"
    ),
    "techies_cache": (
        "###########################\n"
        "#.........................#\n"
        "#..@.....[ ][ ][ ].......#\n"
        "#........[BOOM?!]........#\n"
        "#..........:)  :)........#\n"
        "###########################"
    ),
    "meepo_clones": (
        "###########################\n"
        "#.........................#\n"
        "#..@...M..M..M............#\n"
        "#.......'which way?'......#\n"
        "###########################"
    ),
    "cursed_chest": (
        "###########################\n"
        "#...........~~............#\n"
        "#..@.......[==]..........#\n"
        "#..........{$$}..........#\n"
        "#...........~~............#\n"
        "###########################"
    ),
    "lost_miner": (
        "###########################\n"
        "#.........................#\n"
        "#..@.........?............#\n"
        "#..........m...m..........#\n"
        "###########################"
    ),
    "crystal_golem": (
        "***************************\n"
        "*..........................*\n"
        "*..@.........G.............*\n"
        "*..........zzz.............*\n"
        "***************************"
    ),
    "mushroom_grove": (
        "#.........~*~*~...........#\n"
        "#.........*|*|*..........#\n"
        "#..@.....*|*|*|*.........#\n"
        "#........*|*|*|*|*.......#\n"
        "#.........~*~*~...........#"
    ),
    "magma_geyser": (
        "~~~~~~~~~~~~~~~~~~~~~~~~~~~\n"
        "~.........^^^^^...........~\n"
        "~..@......|||||...........~\n"
        "~.........vvvvv...........~\n"
        "~~~~~~~~~~~~~~~~~~~~~~~~~~~"
    ),
    "ancient_elevator": (
        "###########################\n"
        "#..........[||]...........#\n"
        "#..@.......[||]...........#\n"
        "#...........[v]...........#\n"
        "#..........  |  ..........#\n"
        "###########################"
    ),
    "void_whispers": (
        "............................\n"
        ".  . . . . . . . . . .  ..\n"
        ". ..@.. .whispers. . . . ..\n"
        ".  . . . . . . . . . .  ..\n"
        "............................"
    ),
    "friendly_mole": (
        "###########################\n"
        "#.........................#\n"
        "#..@........(m)...........#\n"
        "#...........*|*...........#\n"
        "###########################"
    ),
    # --- Expansion: Dota heroes ---
    "pudge_fishing": (
        "######............######\n"
        "#.....~~~~~~~~~~~~.....#\n"
        "#..@..~~~~~~~~~~~~..P..#\n"
        "#.....~~~~~~~~~~~~.....#\n"
        "######....=hook=..######"
    ),
    "tinker_workshop": (
        "###########################\n"
        "#..[]==[].gears..[]===[]..#\n"
        "#..||..||........||...||..#\n"
        "#..@...........T.........#\n"
        "#......$$..[wrench]......#\n"
        "###########################"
    ),
    "the_burrow": (
        "###########################\n"
        "#.......####..............#\n"
        "#..@....#  #.....N........#\n"
        "#.......####..............#\n"
        "#.........vvvvv...........#\n"
        "###########################"
    ),
    "arcanist_library": (
        "***************************\n"
        "*.[book][book][book][book].*\n"
        "*..........................*\n"
        "*..@.......oOo....I.......*\n"
        "*.........[tome]...........*\n"
        "***************************"
    ),
    "the_dark_rift": (
        "............................\n"
        "..######################...\n"
        "..#    ~~throne~~      #...\n"
        "..#       /|\\          #...\n"
        "..#..@................#...\n"
        "..######################..."
    ),
    "roshan_lair": (
        ".............................\n"
        "..########=======########...\n"
        "..#                     #...\n"
        "..#     /\\_/\\_/\\       #...\n"
        "..#    ( O     O )      #...\n"
        "..#     \\_______/       #...\n"
        "..#       |   |         #...\n"
        "..#                     #...\n"
        "..########=======########...\n"
        "..............@.............."
    ),
    # --- Expansion: Layer events ---
    "toll_keeper": (
        "###########################\n"
        "#.........................#\n"
        "#..@.......?..............#\n"
        "#........./|\\............#\n"
        "#.........TOLL............#\n"
        "###########################"
    ),
    "mirror_tunnel": (
        "***|***|***|***|***|***|***\n"
        "*..@.........@.........@..*\n"
        "*..........mirror.........*\n"
        "*..@.........@.........@..*\n"
        "***|***|***|***|***|***|***"
    ),
    "void_market": (
        "............................\n"
        ". .$..$..MARKET..$..$.  ..\n"
        ". ..@........$........  ..\n"
        ". .$..$..$$$$$..$..$.  ..\n"
        "............................"
    ),
    "time_eddy": (
        "***************************\n"
        "*......<<<>>><<<>>>.......*\n"
        "*..@...< t i m e >........*\n"
        "*......<<<>>><<<>>>.......*\n"
        "***************************"
    ),
    "paradox_loop": (
        "***************************\n"
        "*..@.................@....*\n"
        "*....?...........?........*\n"
        "*..@.................@....*\n"
        "***************************"
    ),
    "the_cartographer": (
        "............................\n"
        ".  [map][map][map][map]  ..\n"
        ".  ..@.......C.........  ..\n"
        ".  ......[quill]......   ..\n"
        "............................"
    ),
    "the_final_merchant": (
        "............................\n"
        ".  .....[$][$][$]......  ..\n"
        ".  ..@.......M.........  ..\n"
        ".  ......[???]........   ..\n"
        "............................"
    ),
    "spore_storm": (
        "~~~~~~~~~~~~~~~~~~~~~~~~~~~\n"
        "~..*..*..*..*..*..*..*..*..~\n"
        "~..@..*..*..*..*..*..*..*..~\n"
        "~..*..*..*..*..*..*..*..*..~\n"
        "~~~~~~~~~~~~~~~~~~~~~~~~~~~"
    ),
    "bioluminescent_cathedral": (
        "         * . * . *        \n"
        "      * . * . * . *      \n"
        "   * . * . * . * . * .   \n"
        "      * . * . * . *      \n"
        "         * . * . *        \n"
        "           @              "
    ),
    "frozen_ancient": (
        "***************************\n"
        "*..ice..ice..ice..ice..ice*\n"
        "*..@..........???.........*\n"
        "*..ice..ice..ice..ice..ice*\n"
        "***************************"
    ),
    "the_lightless_path": (
        "............................\n"
        "............................\n"
        "..@..>>..>>..>>..>>..>>...\n"
        "............................\n"
        "............................"
    ),
    # --- OSRS nods ---
    "sandwich_lady": (
        "###########################\n"
        "#.........................#\n"
        "#..@.......S..............#\n"
        "#........[====]...........#\n"
        "#.......'baguette'........#\n"
        "###########################"
    ),
    "gem_rock": (
        "***************************\n"
        "*..........................*\n"
        "*..@.......<>.<>.<>........*\n"
        "*.........[gems]...........*\n"
        "***************************"
    ),
    "shooting_star": (
        "~~~~~~~~~~~~~~~~~~~~~~~~~~~\n"
        "~.........\\|/..............~\n"
        "~..@.......*..........$...~\n"
        "~........./ \\..............~\n"
        "~~~~~~~~~~~~~~~~~~~~~~~~~~~"
    ),
    "rock_golem_encounter": (
        "###########################\n"
        "#.........................#\n"
        "#..@.........g.............#\n"
        "#..........(.)............#\n"
        "###########################"
    ),
    "buying_gf": (
        "............................\n"
        ".  @.......m...............\n"
        ".  ......'BUYING GF'......\n"
        ".  .........10k...........\n"
        "............................"
    ),
    # --- Darkness events ---
    "things_in_the_dark": (
        "............................\n"
        "............................\n"
        "..........@....?...........\n"
        "............................\n"
        "............................"
    ),
    "phosphor_vein": (
        "............................\n"
        "..###*****####.............\n"
        "..@..*****.................\n"
        "..###*****####.............\n"
        "............................"
    ),
    # --- Prestige Expansion Events ---
    "creeper_ambush": (
        "###########################\n"
        "#.........................#\n"
        "#..@.........SssSss.......#\n"
        "#...........[#_#]........#\n"
        "#............TNT..........#\n"
        "###########################"
    ),
    "abandoned_minecart": (
        "###########################\n"
        "#........=rails=..........#\n"
        "#..@.....[cart]>..........#\n"
        "#........=rails=..........#\n"
        "###########################"
    ),
    "enchanting_table": (
        "***************************\n"
        "*.......*..*..*..*..*.....*\n"
        "*..@......[E].............*\n"
        "*.......*..*..*..*..*.....*\n"
        "*........[book]...........*\n"
        "***************************"
    ),
    "villager_trade": (
        "###########################\n"
        "#.........................#\n"
        "#..@.........V............#\n"
        "#.........[====]..........#\n"
        "#.........'Hmm.'..........#\n"
        "###########################"
    ),
    "enderman_stare": (
        "***************************\n"
        "*...........|..............*\n"
        "*..@.......[ ]............*\n"
        "*...........|..............*\n"
        "*.........*..*.*..........*\n"
        "***************************"
    ),
    "mob_spawner": (
        "***************************\n"
        "*....[=]..{X}...[=]......*\n"
        "*..@......|s|.............*\n"
        "*........s.s.s............*\n"
        "***************************"
    ),
    "witch_cauldron": (
        "###########################\n"
        "#..........~~~............#\n"
        "#..@......(o o)...........#\n"
        "#.........[===]...........#\n"
        "#......[r] [b] [g]........#\n"
        "###########################"
    ),
    "azurite_deposit": (
        "***************************\n"
        "*..<><><><><><>...........*\n"
        "*..@...<><><>..AZURITE...*\n"
        "*..<><><><><><>...........*\n"
        "***************************"
    ),
    "crawler_breakdown": (
        "***************************\n"
        "*......[===||===].........*\n"
        "*..@...[  rig   ]..!!....*\n"
        "*......[===||===].........*\n"
        "*..........sparks.........*\n"
        "***************************"
    ),
    "fossil_cache": (
        "***************************\n"
        "*.......><>..<>............*\n"
        "*..@....()..><>............*\n"
        "*.......><>..<>............*\n"
        "***************************"
    ),
    "breach_encounter": (
        "............................\n"
        "..####\\  /####.............\n"
        "..@....\\/ BREACH...........\n"
        "..####/\\  ####.............\n"
        "............................"
    ),
    "vaal_side_area": (
        "***..........***...........\n"
        "*..########..............*\n"
        "*..@..[VAAL]..$$.........*\n"
        "*..########..............*\n"
        "***..........***...........\n"
    ),
    "syndicate_ambush": (
        "###########################\n"
        "#......v..v..v............#\n"
        "#..@...C..C..C............#\n"
        "#......'toll'..............#\n"
        "###########################"
    ),
    "delve_smuggler": (
        "###.......####.............\n"
        "#..@.....#    #............\n"
        "#........# ?? #............\n"
        "#........#    #............\n"
        "###.......####............."
    ),
    "brann_bronzebeard": (
        "###########################\n"
        "#.........................#\n"
        "#..@.........B............#\n"
        "#........./beard\\........#\n"
        "#.......'Well met!'......#\n"
        "###########################"
    ),
    "earthen_cache": (
        "~~~~~~~~~~~~~~~~~~~~~~~~~~~\n"
        "~.........................~\n"
        "~..@.......[==]...........~\n"
        "~.........{rune}..........~\n"
        "~~~~~~~~~~~~~~~~~~~~~~~~~~~"
    ),
    "campfire_rest": (
        "###########################\n"
        "#.........................#\n"
        "#..@........*^*...........#\n"
        "#...........|.|...........#\n"
        "#........[bedroll]........#\n"
        "###########################"
    ),
    "zekvir_shadow": (
        "............................\n"
        "...../\\  /\\  /\\............\n"
        "..@.( o  o  o  o )........\n"
        ".....\\/  \\/  \\/............\n"
        "............................"
    ),
    "dark_rider": (
        "~~~~~~~~~~~~~~~~~~~~~~~~~~~\n"
        "~.........../|\\...........~\n"
        "~..@.......[R]............~\n"
        "~.........//|\\\\..........~\n"
        "~........[horse]..........~\n"
        "~~~~~~~~~~~~~~~~~~~~~~~~~~~"
    ),
    "titan_relic": (
        "***************************\n"
        "*........[?????]...........*\n"
        "*..@.....(R)(B)(G)........*\n"
        "*........[?????]...........*\n"
        "***************************"
    ),
    "candle_glow": (
        "............................\n"
        "...........*..............\n"
        "..@........|..............\n"
        "..........[=]..............\n"
        "............................"
    ),
    "olympian_boon": (
        "...........\\|/..............\n"
        "............*...............\n"
        "..@......./|\\...............\n"
        "..........CHOOSE............\n"
        "............................"
    ),
    "charon_toll": (
        "###########################\n"
        "#.....~~~~~~~~~~~~........#\n"
        "#..@..[__boat__]..C......#\n"
        "#.....~~~~~~~~~~~~........#\n"
        "###########################"
    ),
    "sisyphus_boulder": (
        "###########################\n"
        "#................/........#\n"
        "#..@..........(O)/........#\n"
        "#.............m./..........#\n"
        "###########################"
    ),
    "infernal_gate": (
        "###########################\n"
        "#......[=GATE=]...........#\n"
        "#..@...[chains]...........#\n"
        "#......[=GATE=]...........#\n"
        "###########################"
    ),
    "riki_ambush": (
        "............................\n"
        "..........  ~  ............\n"
        "..@....... ~ ...............\n"
        "..........  ~  ............\n"
        "............................"
    ),
    "bounty_rune": (
        "###########################\n"
        "#.........................#\n"
        "#..@........(*)...........#\n"
        "#.........bounty..........#\n"
        "###########################"
    ),
    "aghanim_trial": (
        "***************************\n"
        "*.........|||..............*\n"
        "*..@......|A|..............*\n"
        "*.........|||..............*\n"
        "*.......'TRIAL'............*\n"
        "***************************"
    ),
    "tormentor_encounter": (
        "###########################\n"
        "#.......zz[===]zz.........#\n"
        "#..@...zz[     ]zz........#\n"
        "#.......zz[===]zz.........#\n"
        "#........TORMENTOR........#\n"
        "###########################"
    ),
    "neutral_item_drop": (
        "###########################\n"
        "#.........................#\n"
        "#..@........x.............#\n"
        "#..........[?]............#\n"
        "###########################"
    ),
    "gambling_den": (
        "###########################\n"
        "#.....G..G..G..............#\n"
        "#..@..[table].............#\n"
        "#.....[cards].............#\n"
        "#......$$$$...............#\n"
        "###########################"
    ),
    "item_goblin": (
        "###########################\n"
        "#.........................#\n"
        "#..@..........g>>>........#\n"
        "#...........$.$.$........#\n"
        "###########################"
    ),
    "mystery_lever": (
        "###########################\n"
        "#.........................#\n"
        "#..@........|/............#\n"
        "#........[PULL ME]........#\n"
        "###########################"
    ),
    "identity_thief": (
        "###########################\n"
        "#.........................#\n"
        "#..@..........@...........#\n"
        "#..........'who?'.........#\n"
        "###########################"
    ),
    "neow_blessing": (
        "...........***.............\n"
        "..........*...*............\n"
        "..@......* ?? *............\n"
        "..........*...*............\n"
        "...........***............."
    ),
}


EVENT_POOL: list[dict] = [
    {
        "id": e.id, "name": e.name, "description": e.description,
        "min_depth": e.min_depth, "max_depth": e.max_depth,
        "safe_option": _choice_to_dict(e.safe_option),
        "risky_option": _choice_to_dict(e.risky_option),
        "complexity": e.complexity,
        "layer": e.layer,
        "rarity": e.rarity,
        "requires_dark": e.requires_dark,
        "social": e.social,
        "ascii_art": e.ascii_art or EVENT_ASCII_ART.get(e.id),
        "buff_on_success": {
            "id": e.buff_on_success.id,
            "name": e.buff_on_success.name,
            "duration_digs": e.buff_on_success.duration_digs,
            "effect": dict(e.buff_on_success.effect),
        } if e.buff_on_success else None,
        "desperate_option": _choice_to_dict(e.desperate_option) if e.desperate_option else None,
        "boon_options": [
            {"id": b.id, "name": b.name, "duration_digs": b.duration_digs, "effect": dict(b.effect)}
            for b in e.boon_options
        ] if e.boon_options else None,
        "min_prestige": e.min_prestige,
        "splash": {
            "strategy": e.splash.strategy,
            "victim_count": e.splash.victim_count,
            "penalty_jc": e.splash.penalty_jc,
            "trigger": e.splash.trigger,
            "mode": e.splash.mode,
        } if e.splash else None,
    }
    for e in RANDOM_EVENTS
]

# Tips as dicts
DIG_TIPS: list[dict] = [
    {"min_depth": t[0], "max_depth": t[1], "text": t[2]}
    for t in PROGRESSIVE_TIPS
]

# Tunnel name titles (title format: "X of Y")
TUNNEL_NAME_TITLES: list[str] = [
    f"{x} of {y}" for x in TUNNEL_NAME_TITLE_X for y in TUNNEL_NAME_TITLE_Y
]

# Achievements as list for service-layer iteration
ACHIEVEMENTS: list[dict] = []
for _a in VISIBLE_ACHIEVEMENTS:
    for _t in _a.tiers:
        ACHIEVEMENTS.append({
            "id": f"{_a.id}_{_t.tier.lower()}",
            "name": f"{_a.name} ({_t.tier})",
            "description": _a.description,
            "hidden": False,
            "condition": {"type": _a.id, "value": _t.threshold},
        })
for _h in HIDDEN_ACHIEVEMENTS:
    ACHIEVEMENTS.append({
        "id": _h.id,
        "name": _h.name,
        "description": _h.description,
        "hint": _h.hint,
        "hidden": True,
        "condition": {"type": _h.id},
    })
