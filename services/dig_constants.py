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
    LayerDef("Dirt",    0,   25,  0.05, 0, 1, 1, 3, "\U0001f7eb"),   # brown square
    LayerDef("Stone",   26,  50,  0.10, 0, 2, 1, 3, "\u2b1c"),       # gray (white square)
    LayerDef("Crystal", 51,  75,  0.18, 1, 3, 1, 2, "\U0001f48e"),   # diamond
    LayerDef("Magma",   76,  100, 0.25, 1, 5, 1, 2, "\U0001f525"),   # fire
    LayerDef("Abyss",   101, None, 0.35, 2, 8, 1, 2, "\U0001f573\ufe0f"),  # hole
]

LAYER_BOUNDARIES: list[int] = [25, 50, 75, 100]


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
}

# Streak rewards: consecutive-day count -> JC bonus
STREAKS: dict[int, int] = {
    3: 1,
    7: 3,
    14: 5,
    30: 10,
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
    PickaxeTier("Wooden",  0, 0.0,  0, depth_required=0,  jc_cost=0,   prestige_required=0),
    PickaxeTier("Stone",   1, 0.0,  0, depth_required=25, jc_cost=15,  prestige_required=0),
    PickaxeTier("Iron",    0, 0.05, 0, depth_required=50, jc_cost=50,  prestige_required=1),
    PickaxeTier("Diamond", 0, 0.0,  2, depth_required=75, jc_cost=150, prestige_required=2),
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
            "  ,-^-.\n"
            " / o o \\\n"
            "| (___) |\n"
            " \\ === /\n"
            "  '---'"
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
class RandomEvent:
    """Immutable definition for a random tunnel event."""
    id: str
    name: str
    description: str
    min_depth: int | None           # None = any depth
    max_depth: int | None           # None = any depth
    safe_option: EventChoice
    risky_option: EventChoice


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
}


# ---------------------------------------------------------------------------
# Prestige Constants
# ---------------------------------------------------------------------------

MAX_PRESTIGE: int = 5

PRESTIGE_CROWNS: dict[int, str] = {
    0: "",
    1: "\u26cf\ufe0f",      # pick
    2: "\U0001f48e",         # gem
    3: "\U0001f451",         # crown
    4: "\U0001f4a0",         # diamond with dot
    5: "\u2b50",             # star
}

RELIC_SLOTS_BASE: int = 1  # relic_slots = prestige_level + RELIC_SLOTS_BASE

PRESTIGE_PERKS: list[str] = [
    "advance_boost",
    "cave_in_resistance",
    "loot_multiplier",
    "mixed_bonus",
]

# Per-prestige-level bonuses for each perk
PRESTIGE_PERK_VALUES: dict[str, dict[str, float]] = {
    "advance_boost": {"advance_min_bonus": 1.0},
    "cave_in_resistance": {"cave_in_reduction": 0.05},
    "loot_multiplier": {"jc_bonus": 1.0},
    "mixed_bonus": {"advance_min_bonus": 0.5, "cave_in_reduction": 0.02, "jc_bonus": 0.5},
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
}

DECAY_START_HOURS: int = 24                        # decay begins after this
DECAY_ACCELERATED_HOURS: int = 72                  # 2x rate after this
DECAY_ACCELERATED_MULTIPLIER: float = 2.0
DECAY_FLOOR_DEPTHS: list[int] = [25, 50, 75]       # decay cannot cross these
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

MAX_INVENTORY_SLOTS: int = 5
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

BOSS_BOUNDARIES: list[int] = LAYER_BOUNDARIES  # [25, 50, 75, 100]
BOSS_DEPTHS: list[int] = LAYER_BOUNDARIES

BOSS_NAMES: dict[int, str] = {d: b.name for d, b in BOSSES.items()}
BOSS_DIALOGUE: dict[int, list[str]] = {d: b.dialogue for d, b in BOSSES.items()}
BOSS_ASCII: dict[int, str] = {d: b.ascii_art for d, b in BOSSES.items()}
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
EVENT_POOL: list[dict] = [
    {
        "id": e.id, "name": e.name, "description": e.description,
        "min_depth": e.min_depth, "max_depth": e.max_depth,
        "safe_option": {
            "label": e.safe_option.label,
            "success": {"description": e.safe_option.success.description, "advance": e.safe_option.success.advance, "jc": e.safe_option.success.jc, "cave_in": e.safe_option.success.cave_in},
            "failure": {"description": e.safe_option.failure.description, "advance": e.safe_option.failure.advance, "jc": e.safe_option.failure.jc, "cave_in": e.safe_option.failure.cave_in} if e.safe_option.failure else None,
            "success_chance": e.safe_option.success_chance,
        },
        "risky_option": {
            "label": e.risky_option.label,
            "success": {"description": e.risky_option.success.description, "advance": e.risky_option.success.advance, "jc": e.risky_option.success.jc, "cave_in": e.risky_option.success.cave_in},
            "failure": {"description": e.risky_option.failure.description, "advance": e.risky_option.failure.advance, "jc": e.risky_option.failure.jc, "cave_in": e.risky_option.failure.cave_in} if e.risky_option.failure else None,
            "success_chance": e.risky_option.success_chance,
        },
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
