"""
Constants for the tunnel digging minigame.

Pure data/constants file with no external dependencies.
Organized into sections: layers, pacing, pickaxes, consumables, bosses,
artifacts, events, achievements, naming, ASCII art, prestige, decay,
sabotage/defense, injuries, and tips.
"""

from __future__ import annotations

from dataclasses import dataclass, field

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
    LayerDef("Stone",         26,  50,  0.10, 0,  2,  1, 3, "\u2b1c"),            # gray (white square)
    LayerDef("Crystal",       51,  75,  0.18, 1,  3,  1, 2, "\U0001f48e"),        # diamond
    LayerDef("Magma",         76,  100, 0.25, 1,  5,  1, 2, "\U0001f525"),        # fire
    LayerDef("Abyss",         101, 150, 0.35, 2,  8,  1, 2, "\U0001f573\ufe0f"),  # hole
    LayerDef("Fungal Depths", 151, 200, 0.40, 3,  10, 1, 2, "\U0001f344"),        # mushroom
    LayerDef("Frozen Core",   201, 275, 0.45, 4,  12, 1, 2, "\u2744\ufe0f"),      # snowflake
    LayerDef("The Hollow",    276, None, 0.50, 5,  15, 1, 1, "\u26ab"),            # black circle
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

FREE_DIG_COOLDOWN_SECONDS: int = 14_400          # 4 hours

PAID_DIG_COSTS_PER_DAY: list[int] = [3, 6, 12, 24, 48]
PAID_DIG_COST_CAP: int = 48

# First dig guarantees
FIRST_DIG_ADVANCE_MIN: int = 3
FIRST_DIG_ADVANCE_MAX: int = 7
FIRST_DIG_JC_MIN: int = 1
FIRST_DIG_JC_MAX: int = 5
FIRST_DIG_CAVE_IN: bool = False

# Milestone rewards: depth -> JC bonus
MILESTONES: dict[int, int] = {
    25: 5,
    50: 10,
    75: 20,
    100: 50,
    150: 75,
    200: 150,
    275: 300,
    300: 500,
    400: 1000,
}

# Streak rewards: consecutive-day count -> JC bonus
STREAKS: dict[int, int] = {
    3: 2,
    7: 5,
    14: 10,
    30: 20,
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
    PickaxeTier("Iron",         0, 0.05, 0, depth_required=50,  jc_cost=50,   prestige_required=0),
    PickaxeTier("Diamond",      0, 0.0,  2, depth_required=75,  jc_cost=150,  prestige_required=0),
    PickaxeTier("Obsidian",     1, 0.05, 0, depth_required=100, jc_cost=300,  prestige_required=3),
    PickaxeTier("Frostforged",  2, 0.0,  1, depth_required=200, jc_cost=600,  prestige_required=5),
    PickaxeTier("Void-Touched", 2, 0.10, 2, depth_required=275, jc_cost=1200, prestige_required=8),
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
    ),
}

# Boss fight odds ────────────────────────────────────────────────
# Strategy -> base win probability (before depth/prestige scaling)
BOSS_WIN_ODDS: dict[str, float] = {
    "cautious": 0.75,
    "bold": 0.45,
    "reckless": 0.20,
}

BOSS_FREE_FIGHT_ODDS: dict[str, float] = {
    "cautious": 0.50,
    "bold": 0.25,
    "reckless": 0.10,
}

BOSS_DEPTH_SCALING_PER_25: float = 0.05            # -5% per 25 depth
BOSS_PRESTIGE_SCALING_PER_LEVEL: float = 0.05       # -5% per prestige

# Payouts: depth -> (cautious_multiplier, bold_multiplier, reckless_multiplier)
BOSS_PAYOUTS: dict[int, tuple[float, float, float]] = {
    25: (1.5, 3.0, 6.0),
    50: (2.0, 4.0, 8.0),
    75: (2.5, 5.0, 10.0),
    100: (3.0, 6.0, 12.0),
    150: (3.5, 7.0, 14.0),
    200: (4.0, 8.0, 16.0),
    275: (5.0, 10.0, 20.0),
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
    description: str
    choices: list[EventChoice] = field(default_factory=list)


@dataclass(frozen=True)
class RandomEvent:
    """Immutable definition for a random tunnel event."""
    id: str
    name: str
    description: str
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


RANDOM_EVENTS: list[RandomEvent] = [
    RandomEvent(
        id="underground_stream",
        name="Underground Stream",
        description="You break through into a cavern with a rushing underground river.",
        min_depth=None, max_depth=None,
        safe_option=EventChoice(
            "Wade through carefully",
            success=EventOutcome("You cross safely and find coins on the far bank.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Dive in and search the riverbed",
            success=EventOutcome("You surface clutching a fistful of treasure!", 0, 5, False),
            failure=EventOutcome("The current drags you back. You lose ground.", -3, 0, False),
            success_chance=0.50,
        ),
    ),
    RandomEvent(
        id="gas_pocket",
        name="Gas Pocket",
        description="A foul-smelling green gas seeps from a crack in the wall.",
        min_depth=10, max_depth=None,
        safe_option=EventChoice(
            "Retreat and ventilate",
            success=EventOutcome("You wait it out. Fresh air returns.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Dig through the gas",
            success=EventOutcome("The gas was harmless! And behind it—gems!", 2, 3, False),
            failure=EventOutcome("The gas ignites! BOOM!", -5, -2, True),
            success_chance=0.40,
        ),
    ),
    RandomEvent(
        id="techies_cache",
        name="Techies' Hidden Cache",
        description="A suspicious pile of barrels marked with a smiley face. Squee, Spleen, and Spoon were here.",
        min_depth=15, max_depth=75,
        safe_option=EventChoice(
            "Leave the explosives alone",
            success=EventOutcome("Smart choice. You hear a muffled 'aww' from behind the wall.", 0, 1, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Repurpose the explosives",
            success=EventOutcome("KABOOM! A new passage opens up—and some coins fly out!", 5, 4, False),
            failure=EventOutcome("KABOOM! ...in the wrong direction. Techies sends his regards.", -6, -3, True),
            success_chance=0.35,
        ),
    ),
    RandomEvent(
        id="meepo_clones",
        name="Meepo's Lost Clones",
        description="Three identical Meepos are arguing about which way is up.",
        min_depth=None, max_depth=50,
        safe_option=EventChoice(
            "Give them directions",
            success=EventOutcome("They thank you and leave a tip. 'We were never here.'", 0, 3, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Convince them to dig for you",
            success=EventOutcome("Four Meepos carve through rock at terrifying speed!", 4, 2, False),
            failure=EventOutcome("They all net you. It takes an hour to untangle.", -2, 0, False),
            success_chance=0.55,
        ),
    ),
    RandomEvent(
        id="cursed_chest",
        name="Cursed Chest",
        description="An ornate chest sits in the middle of the tunnel, glowing faintly purple.",
        min_depth=25, max_depth=None,
        safe_option=EventChoice(
            "Walk past it",
            success=EventOutcome("Wisdom is its own reward. (You find a coin on the ground.)", 0, 1, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Open the chest",
            success=EventOutcome("Jackpot! The 'curse' was just mood lighting!", 0, 8, False),
            failure=EventOutcome("The chest bites you. Yes, bites.", -4, -3, False),
            success_chance=0.40,
        ),
    ),
    RandomEvent(
        id="lost_miner",
        name="Lost Miner",
        description="A bewildered NPC miner is wandering in circles, muttering about 'the surface.'",
        min_depth=None, max_depth=None,
        safe_option=EventChoice(
            "Point them toward the exit",
            success=EventOutcome("They give you a grateful nod and a couple coins.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Ask them to help you dig deeper",
            success=EventOutcome("Together you make great progress!", 3, 1, False),
            failure=EventOutcome("They panic and cause a minor collapse.", -2, 0, True),
            success_chance=0.60,
        ),
    ),
    RandomEvent(
        id="crystal_golem",
        name="Crystal Golem",
        description="A golem made of shimmering crystals blocks the path. It seems to be... sleeping?",
        min_depth=40, max_depth=90,
        safe_option=EventChoice(
            "Tiptoe around it",
            success=EventOutcome("You sneak past. A crystal chip falls—finders keepers.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Mine the golem for crystals",
            success=EventOutcome("You harvest rare crystals before it wakes!", 0, 6, False),
            failure=EventOutcome("It wakes up. It's not happy.", -5, -2, True),
            success_chance=0.30,
        ),
    ),
    RandomEvent(
        id="mushroom_grove",
        name="Mushroom Grove",
        description="A bioluminescent mushroom grove fills a side chamber with soft blue light.",
        min_depth=5, max_depth=60,
        safe_option=EventChoice(
            "Admire and move on",
            success=EventOutcome("The beauty inspires you. Somehow you dig better.", 1, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Eat the glowing mushroom",
            success=EventOutcome("Incredible! You can see through walls! ...temporarily.", 3, 2, False),
            failure=EventOutcome("Your stomach disagrees violently. Time lost.", -1, -1, False),
            success_chance=0.50,
        ),
    ),
    RandomEvent(
        id="magma_geyser",
        name="Magma Geyser",
        description="The ground rumbles. A vent of superheated steam shoots up ahead.",
        min_depth=60, max_depth=None,
        safe_option=EventChoice(
            "Wait for it to subside",
            success=EventOutcome("Patience pays off. You continue safely.", 1, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Sprint through between eruptions",
            success=EventOutcome("You dash through and find a treasure cache behind!", 3, 5, False),
            failure=EventOutcome("Terrible timing. You get steamed like a dumpling.", -4, -2, True),
            success_chance=0.35,
        ),
    ),
    RandomEvent(
        id="ancient_elevator",
        name="Ancient Elevator",
        description="A rickety mine elevator with frayed ropes. A sign reads: 'Use at own risk.'",
        min_depth=20, max_depth=None,
        safe_option=EventChoice(
            "Take the stairs",
            success=EventOutcome("Slow and steady. You find a coin on a step.", 1, 1, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Ride the elevator",
            success=EventOutcome("WHOOSH! Express to the depths!", 6, 0, False),
            failure=EventOutcome("The rope snaps. You tumble.", -3, -1, True),
            success_chance=0.45,
        ),
    ),
    RandomEvent(
        id="void_whispers",
        name="Void Whispers",
        description="Unintelligible whispers echo from a crack in the abyss wall. They're... offering a deal?",
        min_depth=80, max_depth=None,
        safe_option=EventChoice(
            "Ignore the voices",
            success=EventOutcome("The whispers fade. Probably for the best.", 0, 1, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Accept the void's bargain",
            success=EventOutcome("The void is generous today. Dark riches are yours!", 2, 8, False),
            failure=EventOutcome("The void takes more than it gives.", -6, -4, True),
            success_chance=0.30,
        ),
    ),
    RandomEvent(
        id="friendly_mole",
        name="Friendly Mole",
        description="A large mole pokes its nose out of the wall and squeaks at you encouragingly.",
        min_depth=None, max_depth=40,
        safe_option=EventChoice(
            "Pet the mole",
            success=EventOutcome("The mole is delighted! It shows you a shortcut.", 2, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Follow the mole into its burrow",
            success=EventOutcome("The mole leads you to its treasure stash!", 3, 4, False),
            failure=EventOutcome("You get stuck in the burrow. Embarrassing.", -2, 0, False),
            success_chance=0.55,
        ),
    ),

    # ===================================================================
    # EXPANSION EVENTS — Dirt Layer (0-25)
    # ===================================================================

    RandomEvent(
        id="worm_council",
        name="Worm Council",
        description="A circle of earthworms convenes before you. They appear to be voting on something important.",
        min_depth=None, max_depth=25,
        safe_option=EventChoice(
            "Observe respectfully",
            success=EventOutcome("The motion passes. You are declared an honorary annelid.", 0, 1, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Cast a vote",
            success=EventOutcome("Your vote tips the balance. The worms are grateful.", 1, 2, False),
            failure=EventOutcome("You voted wrong. The worms are displeased.", -1, 0, False),
            success_chance=0.65,
        ),
        complexity="simple", layer="Dirt",
    ),
    RandomEvent(
        id="buried_lunch_box",
        name="Buried Lunch Box",
        description="A perfectly preserved lunch box from the surface. The sandwich inside is... questionable.",
        min_depth=None, max_depth=25,
        safe_option=EventChoice(
            "Sell the vintage box",
            success=EventOutcome("A collector would pay good money for this.", 0, 1, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Eat the ancient sandwich",
            success=EventOutcome("Aged like fine wine. You feel invigorated.", 1, 3, False),
            failure=EventOutcome("Food poisoning. Your ancestors are disappointed.", 0, -1, False),
            success_chance=0.60,
        ),
        layer="Dirt",
    ),
    RandomEvent(
        id="dig_dog",
        name="The Dig Dog",
        description="A dog is digging enthusiastically nearby. It is objectively better at this than you.",
        min_depth=None, max_depth=25,
        safe_option=EventChoice(
            "Watch and learn",
            success=EventOutcome("The dog teaches you a new technique. Good boy.", 2, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Race the dog",
            success=EventOutcome("You win! The dog doesn't care. It's just happy to dig.", 3, 1, False),
            failure=EventOutcome("The dog laps you twice. Humbling.", 0, 0, False),
            success_chance=0.40,
        ),
        complexity="simple", layer="Dirt",
    ),
    RandomEvent(
        id="root_maze",
        name="Root Maze",
        description="A tangle of ancient roots blocks the path. Something metallic glints deep inside.",
        min_depth=5, max_depth=25,
        safe_option=EventChoice(
            "Hack a narrow path",
            success=EventOutcome("Slow but steady. You squeeze through.", 1, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Reach for the glint",
            success=EventOutcome("A handful of buried coins! Worth the scratches.", 3, 2, False),
            failure=EventOutcome("Tangled. It takes an hour to free yourself.", -2, 0, False),
            success_chance=0.50,
        ),
        layer="Dirt",
    ),
    RandomEvent(
        id="pickaxe_head_flies_off",
        name="Pickaxe Head Flies Off",
        description="Your pickaxe head detaches mid-swing and sails into the darkness. You hear it land... somewhere.",
        min_depth=None, max_depth=30,
        safe_option=EventChoice(
            "Go find it",
            success=EventOutcome("Found it! And some coins along the way.", -1, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Dig with your hands",
            success=EventOutcome("Surprisingly effective. You find the head AND a geode.", 1, 3, False),
            failure=EventOutcome("Your hands disagree with this approach.", -2, 0, False),
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
        description="A spectral figure blocks the path. It holds out one translucent hand. 'Toll or riddle. Your choice.'",
        min_depth=26, max_depth=55,
        safe_option=EventChoice(
            "Pay the toll (3 JC)",
            success=EventOutcome("The ghost pockets the coins somehow and steps aside.", 2, -3, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Answer the riddle",
            success=EventOutcome("'Correct,' it says, sounding disappointed. It drops its own toll.", 3, 5, False),
            failure=EventOutcome("Wrong. 'Everyone says fire,' it sighs. It takes your coins anyway.", 0, -2, False),
            success_chance=0.50,
        ),
        complexity="complex", layer="Stone",
    ),
    RandomEvent(
        id="gravity_pocket",
        name="Gravity Pocket",
        description="An area where gravity is... optional. Your tools float. You float. Everything floats down here.",
        min_depth=26, max_depth=55,
        safe_option=EventChoice(
            "Wait for gravity to return",
            success=EventOutcome("It does. Eventually. You feel vaguely insulted.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Swim downward through the void",
            success=EventOutcome("You swim through solid rock. Best not to think about it.", 4, 0, False),
            failure=EventOutcome("You float up instead. Ceiling is farther than you thought.", -3, 0, False),
            success_chance=0.55,
        ),
        layer="Stone",
    ),
    RandomEvent(
        id="fossil_argument",
        name="Fossil Argument",
        description="Two fossils embedded in the wall face each other. They look like they were fighting when they died.",
        min_depth=26, max_depth=55,
        safe_option=EventChoice(
            "Leave them to their eternal dispute",
            success=EventOutcome("Some arguments aren't worth getting between.", 0, 1, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Extract both fossils",
            success=EventOutcome("Museum quality! Separate display cases, obviously.", 0, 4, False),
            failure=EventOutcome("They crumble to dust the moment you touch them. Of course.", -3, 0, False),
            success_chance=0.45,
        ),
        layer="Stone",
    ),
    RandomEvent(
        id="sandwich_lady",
        name="The Sandwich Lady",
        description="A woman appears from behind a stalagmite carrying a tray of baguettes. 'I didn't ask for this,' you think.",
        min_depth=26, max_depth=55,
        safe_option=EventChoice(
            "Accept the baguette",
            success=EventOutcome("The baguette is surprisingly good. She vanishes.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Decline politely",
            success=EventOutcome("She shrugs and leaves a tip jar. It has coins in it.", 0, 3, False),
            failure=EventOutcome("She doesn't take rejection well. Baguette to the face.", -1, 0, False),
            success_chance=0.70,
        ),
        complexity="simple", layer="Stone",
    ),
    RandomEvent(
        id="echo_chamber",
        name="Echo Chamber",
        description="Your footsteps echo in impossibly complex patterns. For a moment, you hear the steps of every miner who has ever been here.",
        min_depth=26, max_depth=55,
        safe_option=EventChoice(
            "Listen",
            success=EventOutcome("The echoes fade. You feel less alone. Somehow that's worse.", 0, 1, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Shout into the chamber",
            success=EventOutcome("Your echo comes back with directions. And coins.", 2, 2, False),
            failure=EventOutcome("Your echo comes back louder. Much louder. Cave-in.", -3, 0, True),
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
        description="Crystal walls reflect infinite copies of you. One of the reflections waves. You didn't wave.",
        min_depth=51, max_depth=80,
        safe_option=EventChoice(
            "Ignore it and move on",
            success=EventOutcome("Smart. The reflection looks disappointed.", 0, 1, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Wave back",
            success=EventOutcome("The reflection smiles and points to a hidden cache.", 2, 4, False),
            failure=EventOutcome("It grabs your hand and tries to pull you through. You wrench free.", -3, -1, False),
            success_chance=0.50,
        ),
        complexity="complex", layer="Crystal",
    ),
    RandomEvent(
        id="resonance_cascade",
        name="Resonance Cascade",
        description="Crystals vibrate at increasing frequency. They're about to shatter. The air tastes like ozone.",
        min_depth=51, max_depth=80,
        safe_option=EventChoice(
            "Run",
            success=EventOutcome("You make it out just as the cascade peaks. Glass everywhere.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Stand your ground",
            success=EventOutcome("The shatter opens a massive cavern. You're the first one here.", 5, 4, False),
            failure=EventOutcome("The shatter opens nothing. Except your skin. Medical bill incoming.", -5, -2, True),
            success_chance=0.35,
        ),
        layer="Crystal",
    ),
    RandomEvent(
        id="crystal_garden",
        name="Crystal Garden",
        description="Someone has been cultivating crystals here. Tiny signs read 'do not touch' and 'water weekly.'",
        min_depth=51, max_depth=80,
        safe_option=EventChoice(
            "Admire the garden",
            success=EventOutcome("You find a donation jar. It's been a while since anyone else visited.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Harvest a crystal",
            success=EventOutcome("The gardener isn't coming back. These are yours now.", 0, 5, False),
            failure=EventOutcome("The crystals fight back. They're pricklier than they look.", -2, -1, False),
            success_chance=0.45,
        ),
        complexity="simple", layer="Crystal",
    ),
    RandomEvent(
        id="gem_rock",
        name="Gem Rock",
        description="A rock glitters with embedded gems. Mining it is risky — these rocks are known to fight back.",
        min_depth=51, max_depth=80,
        safe_option=EventChoice(
            "Chip carefully at the edges",
            success=EventOutcome("A modest haul. Patience rewarded.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Swing hard",
            success=EventOutcome("Uncut diamond! ...wait, wrong game. Still valuable though.", 0, 6, False),
            failure=EventOutcome("The rock was a mimic. Why is everything down here a mimic.", -2, -2, False),
            success_chance=0.40,
        ),
        layer="Crystal",
    ),
    RandomEvent(
        id="prism_trap",
        name="Prism Trap",
        description="Light bends through crystal prisms creating a dazzling but disorienting maze of rainbows.",
        min_depth=51, max_depth=80,
        safe_option=EventChoice(
            "Close your eyes and feel the walls",
            success=EventOutcome("Slow but effective. You emerge on the other side.", 1, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Follow the brightest beam",
            success=EventOutcome("The light leads you true. A shortcut reveals itself.", 3, 2, False),
            failure=EventOutcome("You walk in circles for an hour. The prisms mock you.", -3, 0, False),
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
        description="A chunk of obsidian floats on a lava river. It's just barely big enough to stand on.",
        min_depth=76, max_depth=105,
        safe_option=EventChoice(
            "Go around",
            success=EventOutcome("The long way. Safe, boring, and slightly damp.", 1, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Surf the lava",
            success=EventOutcome("You ride the rapids like a champion. The lava respects you.", 6, 2, False),
            failure=EventOutcome("You fall in. It's exactly as hot as you'd expect.", -4, -3, True),
            success_chance=0.40,
        ),
        layer="Magma",
    ),
    RandomEvent(
        id="forge_spirit",
        name="Forge Spirit",
        description="A fire elemental hammers at an anvil. It notices you. 'Trade or fight. I don't do small talk.'",
        min_depth=76, max_depth=105,
        safe_option=EventChoice(
            "Trade (5 JC)",
            success=EventOutcome("It hands you something warm. Your next digs feel... enhanced.", 0, -5, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Fight it",
            success=EventOutcome("You shatter the elemental. Its core is worth a fortune.", 0, 8, False),
            failure=EventOutcome("Fire hurts. Who knew.", -4, -2, True),
            success_chance=0.40,
        ),
        complexity="complex", layer="Magma",
        buff_on_success=TempBuff("forged", "Forge-Tempered", 3, {"advance_bonus": 2}),
    ),
    RandomEvent(
        id="volcanic_vent_gambit",
        name="Volcanic Vent Gambit",
        description="A volcanic vent cycles between eruptions. The gap between bursts is exactly three heartbeats.",
        min_depth=76, max_depth=None,
        safe_option=EventChoice(
            "Wait for it to subside",
            success=EventOutcome("Patience. The vent cools after an hour.", 0, 1, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Sprint through between eruptions",
            success=EventOutcome("Three heartbeats. You made it in two.", 8, 3, False),
            failure=EventOutcome("Terrible timing. Cooked.", -6, -3, True),
            success_chance=0.30,
        ),
        layer="Magma",
    ),
    RandomEvent(
        id="heat_mirage",
        name="Heat Mirage",
        description="You see an oasis ahead. With a swimming pool. And a bartender. It's 100% not real.",
        min_depth=76, max_depth=105,
        safe_option=EventChoice(
            "Keep walking",
            success=EventOutcome("You knew it wasn't real but you're still disappointed.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Walk toward it anyway",
            success=EventOutcome("Wait. The bartender WAS real? Free drinks.", 0, 5, False),
            failure=EventOutcome("It was never real. You wasted an hour.", -2, 0, False),
            success_chance=0.25,
        ),
        complexity="simple", layer="Magma",
    ),
    RandomEvent(
        id="shooting_star",
        name="Shooting Star",
        description="A blazing rock tears through the cavern wall and embeds itself in the floor. It radiates warmth and value.",
        min_depth=76, max_depth=None,
        safe_option=EventChoice(
            "Mine the star",
            success=EventOutcome("Star fragments. Warm to the touch and surprisingly dense.", 0, 4, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Crack it open",
            success=EventOutcome("The core is pure stardust. Priceless.", 0, 8, False),
            failure=EventOutcome("It explodes. Stars do that sometimes.", -3, -1, True),
            success_chance=0.35,
        ),
        complexity="simple", layer="Magma", rarity="uncommon",
    ),

    # ===================================================================
    # EXPANSION EVENTS — Dota Hero Encounters
    # ===================================================================

    RandomEvent(
        id="pudge_fishing",
        name="Pudge's Fishing Hole",
        description="A butcher on the far side of a chasm is fishing with a meat hook. 'Fresh meat delivery service!' he calls. 'First ride's free.'",
        min_depth=26, max_depth=None,
        safe_option=EventChoice(
            "Climb around the chasm",
            success=EventOutcome("The long way. You find coins in the crevices.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Grab the hook",
            success=EventOutcome("WHEEE! You arrive covered in something but 6 blocks deeper.", 6, 0, False),
            failure=EventOutcome("The hook misses. You fall. Pudge waves sadly from above.", -4, -2, True),
            success_chance=0.50,
        ),
        rarity="uncommon",
    ),
    RandomEvent(
        id="tinker_workshop",
        name="Tinker's Abandoned Workshop",
        description="A cluttered workshop full of half-finished gadgets. A sign reads: 'Gone to rearm. Back in 5 minutes.' The sign is 400 years old.",
        min_depth=51, max_depth=None,
        safe_option=EventChoice(
            "Scavenge for parts",
            success=EventOutcome("Spare parts. Not glamorous, but sellable.", 0, 3, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Repair a gadget",
            success=EventOutcome("It works! A drill that practically digs itself.", 0, 0, False),
            failure=EventOutcome("It doesn't work. It also sparks. And now your eyebrows are gone.", -2, -1, False),
            success_chance=0.45,
        ),
        complexity="complex", layer="Crystal", rarity="uncommon",
        buff_on_success=TempBuff("tinker_drill", "Rearm Protocol", 3, {"advance_bonus": 2}),
    ),
    RandomEvent(
        id="the_burrow",
        name="The Burrow",
        description="The ground cracks beneath you. Something chitinous scuttles below. It's been waiting.",
        min_depth=101, max_depth=None,
        safe_option=EventChoice(
            "Retreat slowly",
            success=EventOutcome("It lets you go. This time.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Investigate the cracks",
            success=EventOutcome("An ancient intelligence offers knowledge. Your depth for its wisdom.", 0, 8, False),
            failure=EventOutcome("It didn't want to talk. It wanted lunch.", -5, -3, True),
            success_chance=0.35,
        ),
        complexity="complex", rarity="uncommon",
        buff_on_success=TempBuff("nyx_insight", "Vendetta's Whisper", 2, {"cave_in_reduction": 0.10}),
    ),
    RandomEvent(
        id="arcanist_library",
        name="The Arcanist's Library",
        description="A chamber lined with tomes. An impossibly old man surrounded by floating orbs doesn't look up. 'I know why you're here,' he says. He sounds bored.",
        min_depth=201, max_depth=None,
        safe_option=EventChoice(
            "Browse the shelves",
            success=EventOutcome("You find a primer on geological theory. +30 luminosity restored.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Ask about the orbs",
            success=EventOutcome("He almost smiles. 'Choose: cold, energy, or fire.' Cold restores your light. The others... well.", 3, 8, False),
            failure=EventOutcome("He waves his hand. You're outside. You don't remember leaving.", -4, 0, False),
            success_chance=0.40,
        ),
        complexity="complex", layer="Frozen Core", rarity="rare",
    ),
    RandomEvent(
        id="the_dark_rift",
        name="The Dark Rift",
        description="The tunnel opens into a vast underground kingdom. A horned figure on a throne of basalt offers passage. 'Everything has a price,' it says. 'Even the ground you stand on.'",
        min_depth=101, max_depth=155,
        safe_option=EventChoice(
            "Bow and withdraw",
            success=EventOutcome("It nods. Respect costs nothing. You find coins by the exit.", 0, 3, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Challenge for passage",
            success=EventOutcome("You win the challenge. The throne room collapses into a shortcut.", 8, 5, False),
            failure=EventOutcome("It wasn't really a challenge. More of a demonstration.", -6, -4, True),
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
        description="Shadowy merchants materialize from nothing. They trade in concepts. 'Depth for wealth? Wealth for depth? Information for blocks?'",
        min_depth=101, max_depth=155,
        safe_option=EventChoice(
            "Trade depth for wealth (3 blocks for 6 JC)",
            success=EventOutcome("Fair trade. Your tunnel shortens. Your wallet fattens.", -3, 6, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Trade wealth for depth (6 JC for 5 blocks)",
            success=EventOutcome("The shadows accept. You feel your tunnel extend.", 5, -6, False),
            failure=EventOutcome("The shadows take your coins and vanish. No refunds in the void.", 0, -6, False),
            success_chance=0.75,
        ),
        complexity="complex", layer="Abyss", rarity="uncommon",
    ),
    RandomEvent(
        id="abyssal_fishing",
        name="Abyssal Fishing",
        description="A luminous pool of liquid void. Something moves beneath the surface. It might be valuable. It might be alive.",
        min_depth=101, max_depth=155,
        safe_option=EventChoice(
            "Peer into the depths",
            success=EventOutcome("You see your reflection. It winks. You earn 2 JC for your trouble.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Reach in",
            success=EventOutcome("Your hand closes around something cold and heavy. Treasure.", 0, 8, False),
            failure=EventOutcome("Whatever it was, it pulled your coin purse in. And your glove.", 0, -4, False),
            success_chance=0.50,
        ),
        layer="Abyss", rarity="uncommon",
    ),
    RandomEvent(
        id="gravity_inversion",
        name="Gravity Inversion",
        description="You are suddenly falling upward. Then sideways. Then in a direction that doesn't have a name.",
        min_depth=101, max_depth=None,
        safe_option=EventChoice(
            "Grab hold of something",
            success=EventOutcome("You cling to a stalactite until reality sorts itself out.", 1, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Fall sideways on purpose",
            success=EventOutcome("You fall through solid rock. The physics down here are generous.", 5, 2, False),
            failure=EventOutcome("You fall up, then down, then up again. Nauseating.", -4, -1, False),
            success_chance=0.45,
        ),
        layer="Abyss",
    ),
    RandomEvent(
        id="whispering_walls_extended",
        name="Whispering Walls",
        description="The void's whispers form coherent sentences. 'We remember when this stone was sky.'",
        min_depth=101, max_depth=155,
        safe_option=EventChoice(
            "Cover your ears",
            success=EventOutcome("The whispers fade. Some things are better left unheard.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Listen carefully",
            success=EventOutcome("'...and the hero bought a clarity and walked into the trees.' Wait, what? Still, the void pays well for listeners.", 2, 10, False),
            failure=EventOutcome("The whispers get inside your head. They rearrange things.", -6, -4, True),
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
        description="A cloud of bioluminescent spores erupts from the fungal walls. Beautiful. Also, you're inhaling them.",
        min_depth=151, max_depth=205,
        safe_option=EventChoice(
            "Retreat and breathe",
            success=EventOutcome("Fresh air. Relatively speaking. Everything down here is questionable.", 1, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Breathe deep",
            success=EventOutcome("The spores are... enhancing? You can see further and dig faster.", 4, 3, False),
            failure=EventOutcome("Spore cough. Your lungs file a formal complaint.", -3, -1, False),
            success_chance=0.45,
        ),
        layer="Fungal Depths",
        buff_on_success=TempBuff("spore_enhanced", "Spore-Enhanced", 2, {"advance_bonus": 1}),
    ),
    RandomEvent(
        id="mycelium_network",
        name="Mycelium Network",
        description="The fungal root network pulses with light. Data flows through it — the locations of other tunnels.",
        min_depth=151, max_depth=205,
        safe_option=EventChoice(
            "Just watch",
            success=EventOutcome("The network hums. You feel connected to something vast.", 0, 3, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Tap in",
            success=EventOutcome("You see the depths of other miners. Knowledge is power.", 0, 5, False),
            failure=EventOutcome("The network rejects you. A psychic headache.", -2, 0, False),
            success_chance=0.55,
        ),
        complexity="complex", layer="Fungal Depths", rarity="uncommon", social=True,
    ),
    RandomEvent(
        id="sporewalker",
        name="The Sporewalker",
        description="A humanoid made entirely of mushrooms waves at you. It offers a glowing fungal cap with the enthusiasm of someone who has never been told 'no.'",
        min_depth=151, max_depth=205,
        safe_option=EventChoice(
            "Accept the cap",
            success=EventOutcome("It tastes like earth and starlight. Your tunnel walls feel more solid.", 0, 3, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Ask for the premium cap",
            success=EventOutcome("The Sporewalker beams. The premium cap glows brighter.", 2, 5, False),
            failure=EventOutcome("There is no premium cap. The Sporewalker is confused. And hurt.", 0, 1, False),
            success_chance=0.50,
        ),
        complexity="simple", layer="Fungal Depths",
        buff_on_success=TempBuff("spore_shield", "Fungal Fortitude", 3, {"cave_in_reduction": 0.05}),
    ),
    RandomEvent(
        id="bioluminescent_cathedral",
        name="Bioluminescent Cathedral",
        description="You emerge into a cavern so vast your lantern is unnecessary. The ceiling is a galaxy of living light.",
        min_depth=151, max_depth=205,
        safe_option=EventChoice(
            "Sit and watch",
            success=EventOutcome("For a moment, the weight of all that stone above you doesn't matter.", 0, 5, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Climb toward the lights",
            success=EventOutcome("You reach the bioluminescent canopy. Up close, it's even more beautiful.", 3, 8, False),
            failure=EventOutcome("You slip. The fall is long but the mushrooms are soft.", -4, 0, False),
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
        description="A pocket where time runs differently. You can see your past digs playing out like ghosts ahead of you.",
        min_depth=201, max_depth=280,
        safe_option=EventChoice(
            "Observe from outside",
            success=EventOutcome("You learn from watching your past mistakes. The future feels clearer.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Step inside",
            success=EventOutcome("Time bends around you. For a moment you exist in two places at once. Both of them dig.", 4, 4, False),
            failure=EventOutcome("You step out and three hours have passed. Time is cruel.", -3, -2, False),
            success_chance=0.45,
        ),
        complexity="complex", layer="Frozen Core",
    ),
    RandomEvent(
        id="frozen_ancient",
        name="Frozen Ancient",
        description="Something enormous is frozen in the ice. It has too many limbs. Most of them are wrong.",
        min_depth=201, max_depth=280,
        safe_option=EventChoice(
            "Photograph it",
            success=EventOutcome("For posterity. And selling to tabloids.", 0, 3, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Chip a piece free",
            success=EventOutcome("It's warm. Organic. Worth a fortune to the right buyer.", 0, 10, False),
            failure=EventOutcome("It moved. IT MOVED.", -6, -3, True),
            success_chance=0.35,
        ),
        layer="Frozen Core", rarity="uncommon",
    ),
    RandomEvent(
        id="the_still_point",
        name="The Still Point",
        description="Absolute silence. Absolute cold. Absolute peace. For one perfect moment, nothing decays.",
        min_depth=201, max_depth=280,
        safe_option=EventChoice(
            "Be still",
            success=EventOutcome("The moment passes. But something lingers — your tunnel feels more permanent.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Try to hold the moment",
            success=EventOutcome("You can't. But trying earned you something.", 0, 5, False),
            failure=EventOutcome("The moment shatters. Cold rushes in.", -2, 0, False),
            success_chance=0.50,
        ),
        complexity="simple", layer="Frozen Core", rarity="rare",
        buff_on_success=TempBuff("still_point", "Temporal Anchor", 5, {"cave_in_reduction": 0.08}),
    ),
    RandomEvent(
        id="paradox_loop",
        name="Paradox Loop",
        description="You meet yourself coming the other direction. You both stop. Neither of you seems surprised.",
        min_depth=201, max_depth=None,
        safe_option=EventChoice(
            "Offer yourself a coin",
            success=EventOutcome("Your other self accepts. You feel generous and broke simultaneously.", 3, -1, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Fight yourself",
            success=EventOutcome("You win. Or you lost. The distinction is academic at this depth.", 5, 3, False),
            failure=EventOutcome("You lose to yourself. The humiliation is paradoxically doubled.", -5, -2, False),
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
        description="A figure with no face draws maps on the walls. The maps are of places that shouldn't exist. Some of them are accurate.",
        min_depth=276, max_depth=None,
        safe_option=EventChoice(
            "Help draw",
            success=EventOutcome("Your contribution is... noted. The Cartographer pays in concepts that translate to coins.", 2, 5, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Buy a map (10 JC)",
            success=EventOutcome("The map shows paths that shouldn't exist. They do anyway.", 5, -10, False),
            failure=EventOutcome("The map is blank. 'That IS The Hollow,' the Cartographer explains.", 0, -10, False),
            success_chance=0.60,
        ),
        complexity="complex", layer="The Hollow", rarity="rare",
    ),
    RandomEvent(
        id="the_final_merchant",
        name="The Final Merchant",
        description="A merchant who sells only one thing, and it changes every time you meet them. They seem tired of existing.",
        min_depth=276, max_depth=None,
        safe_option=EventChoice(
            "Browse and leave",
            success=EventOutcome("'Everyone browses. Nobody buys,' the merchant says. 'Story of my life.'", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Haggle",
            success=EventOutcome("'Fine. Half price. Just take it.' The merchant looks relieved.", 3, 8, False),
            failure=EventOutcome("The price doubles. 'That's what you get for haggling with the void.'", 0, -5, False),
            success_chance=0.40,
        ),
        complexity="complex", layer="The Hollow", rarity="legendary",
    ),
    RandomEvent(
        id="memory_of_the_surface",
        name="Memory of the Surface",
        description="For a moment, you remember what sunlight feels like. The warmth. The color. It hurts more than you expected.",
        min_depth=276, max_depth=None,
        safe_option=EventChoice(
            "Let it pass",
            success=EventOutcome("You dig with renewed purpose. Or possibly spite. Hard to tell the difference down here.", 0, 5, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Hold onto the memory",
            success=EventOutcome("The light stays a little longer. Your pickaxe feels lighter.", 3, 8, False),
            failure=EventOutcome("The memory dissolves. You feel emptier than before.", -3, 0, False),
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
        description="You can't see what's touching your shoulder. It's warm. It might be friendly. It might not.",
        min_depth=76, max_depth=None,
        safe_option=EventChoice(
            "Stand very still",
            success=EventOutcome("It leaves. You'll never know what it was. Maybe that's better.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Reach out",
            success=EventOutcome("It was trying to give you something. In the dark. Normally, somehow.", 0, 10, False),
            failure=EventOutcome("It wasn't friendly.", -5, -3, True),
            success_chance=0.50,
        ),
        requires_dark=True,
    ),
    RandomEvent(
        id="the_lightless_path",
        name="The Lightless Path",
        description="In total darkness, your other senses sharpen. You can hear a path the light would never reveal.",
        min_depth=76, max_depth=None,
        safe_option=EventChoice(
            "Stay put",
            success=EventOutcome("The sound fades. You wait for the light to return.", 0, 1, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Follow the sound",
            success=EventOutcome("The path opens into a chamber. You don't need light to feel the treasure.", 5, 5, False),
            failure=EventOutcome("You followed the wrong sound. Deeper darkness.", -4, -2, False),
            success_chance=0.45,
        ),
        complexity="complex", requires_dark=True,
    ),
    RandomEvent(
        id="phosphor_vein",
        name="Phosphor Vein",
        description="Your pickaxe strikes something that glows. A vein of phosphorescent mineral splits the darkness.",
        min_depth=76, max_depth=None,
        safe_option=EventChoice(
            "Mine the vein",
            success=EventOutcome("Light floods back. The darkness retreats. Luminosity restored.", 0, 3, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Follow the vein deeper",
            success=EventOutcome("It leads to a mother lode. Light AND wealth.", 3, 6, False),
            failure=EventOutcome("The vein dead-ends. But at least you can see again.", 0, 1, False),
            success_chance=0.55,
        ),
        complexity="simple", requires_dark=True, rarity="uncommon",
    ),

    # ===================================================================
    # EXPANSION EVENTS — Roshan Superboss
    # ===================================================================

    RandomEvent(
        id="roshan_lair",
        name="Roshan's Lair",
        description="An ancient pit radiates power that makes your bones vibrate. Something immense stirs in the darkness. It has been here since before the stone was stone.",
        min_depth=276, max_depth=None,
        safe_option=EventChoice(
            "Flee",
            success=EventOutcome("Discretion is the better part of valor. The pit remembers you left.", 0, 5, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Challenge the ancient",
            success=EventOutcome("Against all odds, the ancient falls. An aegis shard clatters to the ground. The pit is silent.", 10, 25, False),
            failure=EventOutcome("The ancient demonstrates why it has survived since the world was young.", -10, -8, True),
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
        description="A miner is shouting into the void with impressive conviction. 'Buying GF 10k!' The void does not respond.",
        min_depth=101, max_depth=None,
        safe_option=EventChoice(
            "Walk away slowly",
            success=EventOutcome("The shouting fades behind you. Some battles are unwinnable.", 0, 1, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Offer business advice",
            success=EventOutcome("They diversify into gem trading. You get a referral bonus.", 0, 4, False),
            failure=EventOutcome("They counter-offer. You are now somehow the one buying the GF.", 0, -3, False),
            success_chance=0.50,
        ),
        complexity="simple", rarity="uncommon",
    ),
    RandomEvent(
        id="rock_golem_encounter",
        name="Rock Golem",
        description="A tiny golem made of living rock follows you. It seems to have chosen you specifically. It cannot be discouraged.",
        min_depth=50, max_depth=None,
        safe_option=EventChoice(
            "Accept your new companion",
            success=EventOutcome("It settles on your shoulder. It's heavier than it looks.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Try to feed it",
            success=EventOutcome("It eats a rock and purrs. You didn't know rocks could purr.", 1, 3, False),
            failure=EventOutcome("It bites your hand. Rock teeth. Actual rock teeth.", -1, 0, False),
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
        description="A familiar hissing sound. The walls flash green. You have seconds.",
        min_depth=0, max_depth=75,
        safe_option=EventChoice(
            "Sprint back",
            success=EventOutcome("You dive clear as the blast craters the tunnel behind you.", -1, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Shield with pickaxe",
            success=EventOutcome("Your pickaxe absorbs the blast. The debris opens a new passage!", 2, 0, False),
            failure=EventOutcome("The explosion sends you tumbling. Everything goes dark.", -4, 0, True),
            success_chance=0.60,
        ),
        desperate_option=EventChoice(
            "Punch the creeper",
            success=EventOutcome("One punch. The creeper detonates harmlessly. Gunpowder everywhere — and coins!", 6, 4, False),
            failure=EventOutcome("You punch it. It detonates. You are not Steve.", -8, 0, True),
            success_chance=0.15,
        ),
    ),
    RandomEvent(
        id="abandoned_minecart",
        name="Abandoned Minecart",
        description="A rusted minecart sits on ancient rails. Something rattles inside.",
        min_depth=0, max_depth=75,
        safe_option=EventChoice(
            "Search the cart",
            success=EventOutcome("A single coin wedged in the seat. Better than nothing.", 0, 1, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Ride the rails",
            success=EventOutcome("WHOOSH! The cart rockets through the darkness and deposits you deeper!", 5, 0, False),
            failure=EventOutcome("The rails end abruptly. You and the cart part ways.", -3, 0, False),
            success_chance=0.50,
        ),
    ),
    RandomEvent(
        id="enchanting_table",
        name="Enchanting Table",
        description="Purple particles drift from a floating book. Strange symbols orbit the air.",
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
        description="Hmm. A villager in a brown robe offers emeralds for... dirt blocks?",
        min_depth=0, max_depth=80,
        safe_option=EventChoice(
            "Trade politely",
            success=EventOutcome("A fair deal. The villager nods approvingly. 'Hmm.'", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Haggle aggressively",
            success=EventOutcome("The villager sweats. You drive a hard bargain. Emeralds rain.", 0, 5, False),
            failure=EventOutcome("The villager slams the trading window shut. You lose your deposit.", 0, -2, False),
            success_chance=0.45,
        ),
    ),
    RandomEvent(
        id="enderman_stare",
        name="The Staring Contest",
        description="A tall dark figure stands motionless. Purple particles shimmer. It's watching.",
        min_depth=0, max_depth=80,
        safe_option=EventChoice(
            "Look away slowly",
            success=EventOutcome("It teleports away. You exhale.", 1, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Stare back",
            success=EventOutcome("It flinches first. It drops something valuable before vanishing.", 0, 4, False),
            failure=EventOutcome("It shrieks. Reality bends. You are somewhere else now.", -5, 0, False),
            success_chance=0.40,
        ),
        desperate_option=EventChoice(
            "Charge it",
            success=EventOutcome("You tackle an Enderman. It drops a pearl and its dignity.", 3, 8, False),
            failure=EventOutcome("It teleports behind you. 'Nothing personnel, kid.' Cave-in.", -6, 0, True),
            success_chance=0.12,
        ),
        rarity="uncommon",
    ),
    RandomEvent(
        id="mob_spawner",
        name="Mob Spawner",
        description="A cage spins in the corner, spawning skeletal shapes. Treasure chests flank it.",
        min_depth=0, max_depth=75,
        safe_option=EventChoice(
            "Sneak past",
            success=EventOutcome("You slip through unnoticed. The skeletons argue among themselves.", 1, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Grab a chest and run",
            success=EventOutcome("You snatch a chest. Arrows whiz past your ears. Worth it.", 0, 4, False),
            failure=EventOutcome("An arrow finds your knee. The adventuring is over for today.", -3, 0, False),
            success_chance=0.50,
        ),
        desperate_option=EventChoice(
            "Destroy the spawner",
            success=EventOutcome("The cage shatters! Both chests are yours. The skeletons dissolve.", 2, 6, False),
            failure=EventOutcome("The spawner surges. More skeletons than you can count.", -6, 0, True),
            success_chance=0.20,
        ),
        rarity="uncommon",
        buff_on_success=TempBuff("mob_farm", "Mob Farm", 2, {"jc_bonus": 2}),
    ),
    RandomEvent(
        id="witch_cauldron",
        name="Witch's Cauldron",
        description="A bubbling cauldron sits unattended. Three potion bottles labeled in illegible handwriting.",
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
        description="A vein of deep blue crystal hums with stored energy. Cartographers would kill for this.",
        min_depth=40, max_depth=120,
        safe_option=EventChoice(
            "Mine carefully",
            success=EventOutcome("You chip out a few clean crystals. Respectable haul.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Blast it open",
            success=EventOutcome("The vein cracks wide. Azurite shards scatter like blue rain!", 0, 6, False),
            failure=EventOutcome("The blast destabilizes the passage. Crystals shatter worthlessly.", -3, 0, False),
            success_chance=0.45,
        ),
    ),
    RandomEvent(
        id="crawler_breakdown",
        name="Crawler Breakdown",
        description="Your mining rig shudders and sparks. The darkness closes in around you.",
        min_depth=40, max_depth=120,
        safe_option=EventChoice(
            "Repair with spare parts",
            success=EventOutcome("A patch job. It'll hold. You lost some parts though.", 1, -2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Hotwire it",
            success=EventOutcome("Sparks fly — then the engine roars. It's running better than before!", 4, 0, False),
            failure=EventOutcome("The rig catches fire. You abandon the burning wreck.", -4, 0, False),
            success_chance=0.50,
        ),
        desperate_option=EventChoice(
            "Abandon rig and sprint",
            success=EventOutcome("You outrun the darkness. Barely. Your legs will remember this.", 7, 0, False),
            failure=EventOutcome("The darkness catches you. Something in it has claws.", -6, 0, True),
            success_chance=0.15,
        ),
        rarity="uncommon",
    ),
    RandomEvent(
        id="fossil_cache",
        name="Fossil Cache",
        description="Fossilized remains embedded in crystal. They could be valuable... or fragile.",
        min_depth=40, max_depth=120,
        safe_option=EventChoice(
            "Photograph and catalog",
            success=EventOutcome("Careful documentation. A small finder's fee.", 1, 1, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Extract fossils",
            success=EventOutcome("Pristine specimens! Collectors will pay handsomely.", 0, 4, False),
            failure=EventOutcome("They crumble to dust the moment you touch them. Ancient and fragile.", -3, 0, False),
            success_chance=0.55,
        ),
    ),
    RandomEvent(
        id="breach_encounter",
        name="Breach Encounter",
        description="A hand-shaped tear in reality splits open. Things pour through. Purple. Endless. Hungry.",
        min_depth=40, max_depth=170,
        safe_option=EventChoice(
            "Retreat and seal",
            success=EventOutcome("You back away. The breach closes. Wise.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Hold the line",
            success=EventOutcome("You stand your ground. The breach yields treasure as it closes.", 2, 5, False),
            failure=EventOutcome("The breach consumes ground you'd already claimed.", -5, 0, False),
            success_chance=0.40,
        ),
        desperate_option=EventChoice(
            "Dive into the breach",
            success=EventOutcome("You pass through and emerge deeper. Much deeper. With loot.", 5, 12, False),
            failure=EventOutcome("The breach spits you out. Hard. Very, very hard.", -8, 0, True),
            success_chance=0.10,
        ),
        rarity="rare",
    ),
    RandomEvent(
        id="vaal_side_area",
        name="Vaal Side Area",
        description="A blood-red door pulses in the crystal. Corrupted inscriptions promise treasure and death.",
        min_depth=40, max_depth=170,
        safe_option=EventChoice(
            "Walk past",
            success=EventOutcome("Discretion. Valor. You keep walking.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Enter cautiously",
            success=EventOutcome("The corruption parts for you. Ancient Vaal gold awaits.", 0, 6, False),
            failure=EventOutcome("The corruption bites. Your coin pouch feels lighter.", 0, -4, False),
            success_chance=0.40,
        ),
        desperate_option=EventChoice(
            "Smash the corruption altar",
            success=EventOutcome("The altar shatters! A flood of uncorrupted treasure!", 4, 10, False),
            failure=EventOutcome("The altar explodes. Corruption everywhere. In you. On you.", -7, 0, True),
            success_chance=0.15,
        ),
        rarity="rare",
    ),
    RandomEvent(
        id="syndicate_ambush",
        name="Syndicate Ambush",
        description="Cloaked figures drop from the ceiling. 'Your tunnel or your coins.'",
        min_depth=40, max_depth=170,
        safe_option=EventChoice(
            "Pay the toll",
            success=EventOutcome("They take your coins and let you pass. Professional thieves.", 2, -3, False),
            failure=EventOutcome("They take your coins AND push you back. Unprofessional thieves.", 0, -5, False),
            success_chance=0.90,
        ),
        risky_option=EventChoice(
            "Fight back",
            success=EventOutcome("You scatter the syndicate. They drop their own loot!", 0, 5, False),
            failure=EventOutcome("They outnumber you. The beating is educational.", -4, 0, False),
            success_chance=0.45,
        ),
        rarity="uncommon",
    ),
    RandomEvent(
        id="delve_smuggler",
        name="Delve Smuggler",
        description="A figure from a hidden passage. 'Psst. Got the good stuff. Fell off a crawler, innit.'",
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
        description="'Well met, adventurer! Brann Bronzebeard, at yer service. Been mapping these depths for decades.'",
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
        description="A stone chest sealed with dwarven runes. The lock is complex but cracked with age.",
        min_depth=130, max_depth=290,
        safe_option=EventChoice(
            "Force open carefully",
            success=EventOutcome("The lock gives way. Modest treasure inside.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Smash it",
            success=EventOutcome("The chest shatters. Dwarven gold scatters everywhere!", 0, 5, False),
            failure=EventOutcome("The runes were a trap. The blast sends you reeling.", -3, 0, False),
            success_chance=0.50,
        ),
    ),
    RandomEvent(
        id="campfire_rest",
        name="Campfire Rest",
        description="A warm campfire glows in an alcove. A bedroll and whetstone sit nearby.",
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
        description="A massive spider silhouette blocks the tunnel. Eight eyes gleam red.",
        min_depth=130, max_depth=290,
        safe_option=EventChoice(
            "Find another way",
            success=EventOutcome("A detour. Longer, but you keep your limbs.", -1, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Distract and sneak past",
            success=EventOutcome("You toss a rock. It turns. You sprint. Heart hammering.", 3, 0, False),
            failure=EventOutcome("Eight eyes. All looking at you. It pounces.", -5, 0, False),
            success_chance=0.35,
        ),
        desperate_option=EventChoice(
            "Fight Zekvir",
            success=EventOutcome("Against all odds, you drive it back! Legendary loot drops!", 5, 10, False),
            failure=EventOutcome("Zekvir wraps you in silk. The ceiling collapses.", -8, 0, True),
            success_chance=0.12,
        ),
        rarity="rare",
    ),
    RandomEvent(
        id="dark_rider",
        name="The Dark Rider",
        description="A cloaked rider on a skeletal mount emerges from fungal mist. It extends a gauntlet.",
        min_depth=130, max_depth=290,
        safe_option=EventChoice(
            "Bow and let it pass",
            success=EventOutcome("It drops a coin as tribute. Respect, it seems, has value.", 0, 1, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Accept the handshake",
            success=EventOutcome("A cold grip. Knowledge flows into you. Secrets of the deep.", 0, 5, False),
            failure=EventOutcome("The grip tightens. It takes something from you.", 0, -3, False),
            success_chance=0.45,
        ),
        desperate_option=EventChoice(
            "Challenge the rider",
            success=EventOutcome("Your pickaxe meets spectral steel. You win. The mount is yours — briefly.", 3, 8, False),
            failure=EventOutcome("The rider's lance finds its mark. You crumple.", -6, 0, False),
            success_chance=0.18,
        ),
        rarity="uncommon",
    ),
    RandomEvent(
        id="titan_relic",
        name="Titan Relic",
        description="An ancient device of impossible complexity. Three runes glow: red, blue, gold.",
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
        description="A single candle burns impossibly in the frozen dark. Its light is warm. It shouldn't be here.",
        min_depth=130, max_depth=290,
        safe_option=EventChoice(
            "Light your torch from it",
            success=EventOutcome("The flame passes to your torch. The candle keeps burning.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Take the candle",
            success=EventOutcome("It comes free. The warmth follows you. Two coins fall from the wax.", 0, 2, False),
            failure=EventOutcome("It won't move. Wax drips on your hand. The flame flickers reproachfully.", 0, 0, False),
            success_chance=0.55,
        ),
    ),

    # --- Hades-themed — Deep (depth 100+) ---

    RandomEvent(
        id="olympian_boon",
        name="Olympian Boon",
        description="A divine light pierces the darkness. 'Choose wisely, mortal. My gift is not given lightly.'",
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
        description="A boat drifts on an underground river. The ferryman extends a bony hand.",
        min_depth=100, max_depth=None,
        safe_option=EventChoice(
            "Pay the toll",
            success=EventOutcome("Coins clink into skeletal fingers. The boat glides forward.", 3, -4, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Negotiate",
            success=EventOutcome("The ferryman tilts his head. 'Amusing.' He lets you ride free.", 3, 0, False),
            failure=EventOutcome("'No.' The boat drifts away. You wasted your time.", 0, -2, False),
            success_chance=0.40,
        ),
        desperate_option=EventChoice(
            "Steal the oar",
            success=EventOutcome("You yank the oar and paddle furiously. Charon watches, impressed.", 5, 2, False),
            failure=EventOutcome("Charon capsizes the boat. You sink. You surface somewhere worse.", -6, 0, True),
            success_chance=0.15,
        ),
        rarity="uncommon",
    ),
    RandomEvent(
        id="sisyphus_boulder",
        name="Sisyphus's Request",
        description="A man pushes an enormous boulder uphill. 'Help me,' he wheezes. 'Just one more push.'",
        min_depth=100, max_depth=None,
        safe_option=EventChoice(
            "Help push",
            success=EventOutcome("Together, you move it an inch. He smiles. 'That's enough.' He tips you.", 1, 1, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Loot his camp",
            success=EventOutcome("While he pushes, you find his stash. He won't notice for eternity.", 0, 4, False),
            failure=EventOutcome("The boulder rolls back — onto your foot.", -3, 0, False),
            success_chance=0.55,
        ),
    ),
    RandomEvent(
        id="infernal_gate",
        name="Infernal Gate",
        description="A gate of black iron sealed with chains. Beyond: combat sounds and clinking gold.",
        min_depth=100, max_depth=None,
        safe_option=EventChoice(
            "Leave it sealed",
            success=EventOutcome("Some doors are best left closed.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Open carefully",
            success=EventOutcome("The chains fall. Gold spills through the gap.", 0, 7, False),
            failure=EventOutcome("Something reaches through the gap. It pulls you in.", -5, 0, True),
            success_chance=0.30,
        ),
        desperate_option=EventChoice(
            "Shatter the chains",
            success=EventOutcome("The gate flies open. An arena of riches, and you've already won.", 4, 12, False),
            failure=EventOutcome("The gate opens both ways. What was inside is now outside. With you.", -8, 0, True),
            success_chance=0.12,
        ),
        rarity="rare",
    ),

    # --- Dota 2-themed — All depths ---

    RandomEvent(
        id="riki_ambush",
        name="Riki's Ambush",
        description="You feel watched. A shimmer in the air. Suddenly — a blade at your throat. 'Surprise.'",
        min_depth=None, max_depth=None,
        safe_option=EventChoice(
            "Drop coins and run",
            success=EventOutcome("He takes the coins and vanishes. At least you're alive.", 0, -2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Throw dust to reveal him",
            success=EventOutcome("Dust of Appearance! Riki panics. He drops his stash.", 0, 4, False),
            failure=EventOutcome("He blinks behind you. Classic Riki.", -4, 0, False),
            success_chance=0.50,
        ),
        desperate_option=EventChoice(
            "Grab the blade barehanded",
            success=EventOutcome("You catch the blade and twist. Riki squeaks and flees, leaving everything.", 2, 7, False),
            failure=EventOutcome("Blades are sharp. This is not a revelation. It is a reminder.", -6, 0, False),
            success_chance=0.15,
        ),
        rarity="uncommon",
    ),
    RandomEvent(
        id="bounty_rune",
        name="Bounty Rune",
        description="A golden rune hovers in a side chamber, spinning gently. It pulses with energy.",
        min_depth=None, max_depth=None,
        safe_option=EventChoice(
            "Grab it",
            success=EventOutcome("Gold flows into your pockets. Easy money.", 0, 3, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Wait for it to multiply",
            success=EventOutcome("It splits into three! Triple bounty!", 0, 8, False),
            failure=EventOutcome("It fizzles. You get the scraps.", 0, 1, False),
            success_chance=0.45,
        ),
    ),
    RandomEvent(
        id="aghanim_trial",
        name="Aghanim's Trial",
        description="Aghanim materializes in a flash of arcane energy. 'Another one. Let's see what you're made of.'",
        min_depth=None, max_depth=None,
        safe_option=EventChoice(
            "Decline the trial",
            success=EventOutcome("Aghanim shrugs. 'Your loss.' He vanishes in a puff of condescension.", 0, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Accept the trial",
            success=EventOutcome("You survive the gauntlet. Aghanim slow-claps. 'Not bad.'", 3, 6, False),
            failure=EventOutcome("The trial overwhelms you. Aghanim yawns.", -4, 0, False),
            success_chance=0.35,
        ),
        desperate_option=EventChoice(
            "Demand the scepter",
            success=EventOutcome("'Bold.' He hands it over. Power surges through you.", 5, 10, False),
            failure=EventOutcome("'Greedy.' He snaps his fingers. You are elsewhere.", -8, 0, True),
            success_chance=0.10,
        ),
        rarity="rare",
        buff_on_success=TempBuff("aghanim_blessing", "Aghanim's Blessing", 2, {"advance_bonus": 3, "jc_bonus": 2}),
    ),
    RandomEvent(
        id="tormentor_encounter",
        name="Tormentor",
        description="A colossal stone figure blocks the path. Lightning crackles across its surface. It does not move. Yet.",
        min_depth=75, max_depth=None,
        safe_option=EventChoice(
            "Take the long way around",
            success=EventOutcome("You give it a wide berth. It watches you leave.", -2, 0, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Attack while dormant",
            success=EventOutcome("Critical hit! The Tormentor crumbles. Its core is priceless.", 3, 8, False),
            failure=EventOutcome("It wakes up. Lightning everywhere. You regret this.", -6, 0, True),
            success_chance=0.30,
        ),
        desperate_option=EventChoice(
            "Channel everything",
            success=EventOutcome("You pour everything into one strike. The Tormentor EXPLODES.", 5, 15, False),
            failure=EventOutcome("It absorbs your energy and uses it against you. Badly.", -10, 0, True),
            success_chance=0.12,
        ),
        rarity="rare",
    ),
    RandomEvent(
        id="neutral_item_drop",
        name="Neutral Item",
        description="A defeated jungle creep lies in the corner. Something glints in the debris.",
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
        description="A cave lit by guttering candles. Three goblins run a card game. 'Sit down,' one grins.",
        min_depth=None, max_depth=None,
        safe_option=EventChoice(
            "Watch a round",
            success=EventOutcome("You observe. One goblin slips you a coin for the audience.", 0, 1, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Play a hand",
            success=EventOutcome("Full house! The goblins pay up, grudgingly.", 0, 5, False),
            failure=EventOutcome("The goblins cheat. Of course they cheat.", 0, -4, False),
            success_chance=0.45,
        ),
        desperate_option=EventChoice(
            "Go all in",
            success=EventOutcome("You stare down three goblins and win. They flip the table.", 0, 15, False),
            failure=EventOutcome("You lose everything. A goblin pats your shoulder. 'Better luck next time.'", 0, -8, False),
            success_chance=0.15,
        ),
        rarity="uncommon",
    ),
    RandomEvent(
        id="item_goblin",
        name="Item Goblin",
        description="A small green creature sprints past carrying a bulging sack. Gold coins scatter.",
        min_depth=None, max_depth=None,
        safe_option=EventChoice(
            "Pick up dropped coins",
            success=EventOutcome("Free money on the ground. No complaints.", 0, 2, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Chase it",
            success=EventOutcome("You corner it! The goblin surrenders the sack.", 0, 5, False),
            failure=EventOutcome("It's faster than it looks. You trip on a stalagmite.", -2, 0, False),
            success_chance=0.50,
        ),
        desperate_option=EventChoice(
            "Flying tackle",
            success=EventOutcome("WHAM! You and the goblin slide across the cave floor. The sack is yours!", 0, 8, False),
            failure=EventOutcome("You miss. You slide into a wall. The goblin laughs.", -3, -2, False),
            success_chance=0.20,
        ),
        rarity="uncommon",
    ),
    RandomEvent(
        id="mystery_lever",
        name="The Mystery Lever",
        description="A lever protrudes from the wall. It's labeled 'PULL ME' in crayon. This can't end well.",
        min_depth=None, max_depth=None,
        safe_option=EventChoice(
            "Pull it cautiously",
            success=EventOutcome("A hidden panel opens. Coins spill out. Crayon was right!", 0, 3, False),
            failure=EventOutcome("A bucket of water falls on your head. A sign drops: 'GOTCHA'. One coin as consolation.", 0, -1, False),
            success_chance=0.80,
        ),
        risky_option=EventChoice(
            "Yank it hard",
            success=EventOutcome("The wall rotates! A secret passage opens, blasting you forward!", 6, 0, False),
            failure=EventOutcome("The lever breaks. The ceiling groans. Run.", -5, 0, False),
            success_chance=0.40,
        ),
    ),
    RandomEvent(
        id="identity_thief",
        name="The Identity Thief",
        description="A shapeshifter has stolen your face. It's telling other miners YOUR tunnel is abandoned.",
        min_depth=None, max_depth=None,
        safe_option=EventChoice(
            "Report it to authorities",
            success=EventOutcome("Bureaucracy wins. The shapeshifter is fined. You get a small settlement.", 0, 1, False),
            failure=None, success_chance=1.0,
        ),
        risky_option=EventChoice(
            "Confront the imposter",
            success=EventOutcome("'Which one is the real one?' You prove it. The crowd sides with you.", 0, 4, False),
            failure=EventOutcome("The crowd picks the other one. You are escorted away from your own tunnel.", 0, -3, False),
            success_chance=0.50,
        ),
        rarity="uncommon", social=True,
    ),
    RandomEvent(
        id="neow_blessing",
        name="Ancient Blessing",
        description="A voice older than stone: 'You have walked this path before. I remember. Choose your gift.'",
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
        penalty="Advance -1 per dig",
        reward="JC loot +15%",
        effects={"advance_penalty": 1, "jc_multiplier": 0.15},
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

EVENT_CHAIN_CHANCE: float = 0.25
EVENT_CHAIN_JC_MULTIPLIER: float = 1.5


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

CAVE_IN_BLOCK_LOSS_MIN: int = 3
CAVE_IN_BLOCK_LOSS_MAX: int = 8
CAVE_IN_STUN_HOURS_MIN: int = 1
CAVE_IN_STUN_HOURS_MAX: int = 2
CAVE_IN_MEDICAL_BILL_DIVISOR: int = 10      # cost = max(1, depth // divisor)
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

BOSS_NAMES: dict[int, str] = {d: b.name for d, b in BOSSES.items()}
BOSS_DIALOGUE: dict[int, list[str]] = {d: b.dialogue for d, b in BOSSES.items()}
BOSS_ASCII: dict[int, str] = {d: b.ascii_art for d, b in BOSSES.items()}
BOSS_SLUGS: dict[int, str] = {
    25: "grothak", 50: "crystalia", 75: "magmus",
    100: "void_warden", 150: "sporeling", 200: "chronofrost", 275: "nameless",
}
PICKAXE_SLUGS: list[str] = [
    "wooden", "stone", "iron", "diamond", "obsidian", "frostforged", "void_touched",
]
BOSS_ODDS: dict[str, dict] = {
    tier: {"base": odds, "free": BOSS_FREE_FIGHT_ODDS[tier]}
    for tier, odds in BOSS_WIN_ODDS.items()
}

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
