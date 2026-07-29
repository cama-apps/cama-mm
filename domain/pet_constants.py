"""Balance constants for the cama pets (tamagotchi) feature.

Game-balance data lives here, not in config.py: these numbers define the game,
not the deployment. The one env-tunable knob (base hunger decay) is in config
and passed into domain functions as a parameter, because domain modules must
not import config.

All quirk multipliers are integer percentages (100 = no quirk) so hunger math
stays exact integer arithmetic.
"""

from __future__ import annotations

from dataclasses import dataclass

MAX_HUNGER = 100
DEFAULT_HUNGER_DECAY_PER_DAY = 20  # full -> dead in 5 days; config may override

EGG_HATCH_SECONDS = 24 * 3600
ADULT_AGE_SECONDS = 7 * 24 * 3600
UNHATCHED_SPECIES = "unhatched"

# Mood bands on current hunger (lower bound of each band).
MOOD_HAPPY_MIN = 70
MOOD_CONTENT_MIN = 40
MOOD_HUNGRY_MIN = 15
WARNING_HUNGER = 30

PET_NAME_MAX_LEN = 32
ADOPTION_FEES = (20, 50, 75, 100)  # indexed by prior deaths in guild, clamped
RENAME_COST = 10
SUPPLY_STACK_CAP = 99
MAX_BUY_QTY = 25
FEED_CAP_PER_DAY = 4
MATCH_WIN_HUNGER = 10
AEGIS_REVIVE_HUNGER = 10
SALT_LICK_DURATION_SECONDS = 12 * 3600
REFUND_MULT_MIN = 50
REFUND_MULT_MAX = 200

# Passive mining stores integer work units so accrual remains exact without
# floating-point timestamps. One block is one day's worth of units, making
# the per-second rates equal to the advertised blocks-per-day rates.
DIG_WORK_UNITS_PER_BLOCK = 86400
DIG_WORK_RATE_HAPPY = 12
DIG_WORK_RATE_CONTENT = 8
DIG_WORK_RATE_HUNGRY = 3
DIG_WORK_CAP_BLOCKS = 36
DIG_WORK_PER_DIG_CAP_BLOCKS = 12

SPECIES_WEIGHT_TOTAL = 10_000


@dataclass(frozen=True, slots=True)
class PetFood:
    item_id: str
    display_name: str
    cost: int
    restore: int
    emoji: str
    blurb: str


# Tuned so weekly upkeep lands around 50 JC: at 20 hunger/day a pet eats
# 140 points/week, so ~0.30-0.36 JC per point across the tiers. Mild bulk
# discount still rewards planning.
FOOD_ITEMS: dict[str, PetFood] = {
    "tango": PetFood(
        "tango", "Tango", cost=9, restore=25, emoji="🌿",
        blurb="Classic starter regen. Your cama eats a tree.",
    ),
    "alfalfa": PetFood(
        "alfalfa", "Alfalfa Bale", cost=17, restore=50, emoji="🌾",
        blurb="Hearty greens for a hearty hybrid.",
    ),
    "cheese": PetFood(
        "cheese", "Roshan's Cheese", cost=30, restore=100, emoji="🧀",
        blurb="Dropped on third death. Smells incredible.",
    ),
}

SALT_LICK = PetFood(
    "salt_lick", "Salt Lick", cost=8, restore=0, emoji="🧂",
    blurb="No nutrition whatsoever. Pure bliss for 12 hours.",
)


@dataclass(frozen=True, slots=True)
class PetSpecies:
    """One species entry. Quirk fields default to 'no quirk'.

    decay_pct / restore_pct / salt_lick_pct / food_cost_pct are integer
    percentages; refund_bonus_pp is added to the weekly refund roll's
    percentage; match_feed_bonus is added on top of MATCH_WIN_HUNGER;
    spit_one_in=N means 1-in-N feeds are spat out (0 = never).
    """

    species_id: str
    display_name: str
    tier: str  # "common" | "uncommon" | "rare" | "legendary"
    weight: int  # out of SPECIES_WEIGHT_TOTAL
    blurb: str
    decay_pct: int = 100
    restore_pct: int = 100
    refund_bonus_pp: int = 0
    match_feed_bonus: int = 0
    salt_lick_pct: int = 100
    food_cost_pct: int = 100
    spit_one_in: int = 0
    has_aegis: bool = False


# Display names are deliberately subtle; the references live in the ids,
# the art, and the quirks. Flavor draws on real camelid lore (the first
# cama, Rama, was bred in Dubai in 1998 and had a famously poor temperament).
SPECIES: dict[str, PetSpecies] = {
    "common_cama": PetSpecies(
        "common_cama", "Common Cama", "common", 1500,
        "A camel-llama hybrid of no particular distinction. Its quirk is "
        "having none.",
    ),
    "dromedary_cross": PetSpecies(
        "dromedary_cross", "Dune Cama", "common", 1500,
        "Takes after its dromedary side: heroic eyelashes, closable "
        "nostrils, and a modest fat reserve where a hump should be.",
        decay_pct=90,
    ),
    "banana_ears": PetSpecies(
        "banana_ears", "Humming Cama", "common", 1500,
        "Llama-leaning, with the characteristic banana ears. Hums "
        "constantly. Nobody knows why. It is communication.",
    ),
    "embergear_cama": PetSpecies(
        "embergear_cama", "Embergear Cama", "common", 1500,
        "A brass harness hums beneath soot-dark wool. It gathers speed "
        "whenever a match gets loud, then insists the tiny exhaust puff "
        "was a sneeze.",
        match_feed_bonus=2,
    ),
    "jopacama": PetSpecies(
        "jopacama", "Gilded Cama", "uncommon", 750,
        "Its wool grows in faint coin-shaped whorls. Auditors are "
        "suspicious but can prove nothing.",
        refund_bonus_pp=10,
    ),
    "pudge_cama": PetSpecies(
        "pudge_cama", "Ravenous Cama", "uncommon", 750,
        "Round, perpetually famished, and faster at dinner than anything "
        "its size should be. Something metal glints deep in its wool.",
        decay_pct=110,
        restore_pct=110,
    ),
    "courier_cama": PetSpecies(
        "courier_cama", "Pack Cama", "uncommon", 750,
        "Saddlebags, a little flag, big anxious eyes. Lives to deliver. "
        "Everyone screams when it dies.",
        match_feed_bonus=5,
    ),
    "riverglow_cama": PetSpecies(
        "riverglow_cama", "Riverglow Cama", "uncommon", 750,
        "Its hooves leave streaks of light on damp ground. It loiters near "
        "rivers and keeps trying to climb into empty bottles.",
        salt_lick_pct=125,
    ),
    "aegis_cama": PetSpecies(
        "aegis_cama", "Shellback Cama", "rare", 225,
        "Serene, with a faintly glowing shell across its back. Local "
        "legend says it gets one more chance than everything else.",
        has_aegis=True,
    ),
    "invoker_cama": PetSpecies(
        "invoker_cama", "Mystic Cama", "rare", 225,
        "Violet-robed, insufferably learned. Three small orbs circle its "
        "head. Enjoys salt more than most scholars admit.",
        salt_lick_pct=150,
    ),
    "crystal_cama": PetSpecies(
        "crystal_cama", "Frostwool Cama", "rare", 225,
        "Frost-blue wool that never melts. Somehow always finds the "
        "snacks at a discount. A true support.",
        food_cost_pct=90,
    ),
    "prismwool_cama": PetSpecies(
        "prismwool_cama", "Prismwool Cama", "rare", 225,
        "Its fleece catches light in colors that were not there a moment "
        "ago. Turning it slightly improves lunch for reasons nobody can "
        "explain.",
        restore_pct=105,
    ),
    "rama": PetSpecies(
        "rama", "Rama the First", "legendary", 100,
        "The original. Bred in Dubai in 1998 and described by science as "
        "'a behavioral disappointment, displaying an extremely poor "
        "temperament'. Only five ever existed. Yours spits.",
        spit_one_in=20,
    ),
}

_FALLBACK_SPECIES = PetSpecies("unknown", "Cama of Unknown Origin", "common", 0, "")

# --- Gacha layer -----------------------------------------------------------

# Gilded Egg: premium adoption with no commons in the pool. Priced as a
# flat premium ON TOP of the player's current (escalating) adoption fee.
GILDED_EGG_PREMIUM = 230
GILDED_TIER_WEIGHTS = {"uncommon": 65, "rare": 30, "legendary": 5}

# Pity: after this many consecutive common hatches on standard eggs, the
# next standard egg rolls from a no-common pool.
PITY_THRESHOLD = 3
PITY_TIER_WEIGHTS = {"uncommon": 80, "rare": 18, "legendary": 2}

TRINKET_COST = 25
TRINKET_DUPE_REFUND = 10  # net 15 JC sink when the roll is a duplicate

# --- Pet brawls ------------------------------------------------------------

# Hunger settlement. A brawl must never kill: the loser's hunger is floored,
# and settlement never touches died_at. Winner gain mirrors match wins
# (MATCH_WIN_HUNGER) including the Pack Cama bonus, but only for the first
# few wins each game-day so a throwaway pet can't become a free feeder.
BRAWL_WIN_HUNGER = 10
BRAWL_WIN_HUNGER_DAILY_CAP = 3  # rewarded wins per pet per game-day
BRAWL_LOSS_HUNGER = 15
BRAWL_LOSS_FLOOR = 10  # a loss never leaves hunger below this
BRAWL_INSURANCE_REDUCTION = 5  # Gilded Cama (refund_bonus_pp > 0) loses less
BRAWL_WIN_XP = 2
BRAWL_LOSS_XP = 1
BRAWL_XP_DAILY_CAP = 3
BRAWL_XP_PER_LEVEL = 5
BRAWL_TRAINING_XP_CAP = 20
BRAWL_TRAINING_STAT_CAP = 2
BRAWL_TRAINING_LEVEL_CAP = 4
SOLO_TRAINING_SESSION_CAP = 3
SOLO_TRAINING_XP_PER_SESSION = 1

PET_BRAWL_ACCEPT_SECONDS = 180
PET_BRAWL_TURN_SECONDS = 30
PET_BRAWL_ACTIVE_TTL_SECONDS = 1800  # sweep voids battles stuck this long

# --- Altar (sacrifice reroll) ----------------------------------------------

# Upgraded reroll odds keyed by (sacrificed tier, was_adult), values are
# percentage weights per tier pool. Every entry strictly dominates the
# standard 60/30/9/1 pool, adult sacrifices dominate baby ones of the same
# tier, and rare+ pools drop commons entirely — you never reroll a rare
# into a guaranteed-worse pool. The fee side of the brake is that the
# sacrificed pet still counts toward the escalating adoption fee.
SACRIFICE_TIER_WEIGHTS: dict[tuple[str, bool], dict[str, int]] = {
    ("common", False): {"common": 55, "uncommon": 33, "rare": 10, "legendary": 2},
    ("common", True): {"common": 40, "uncommon": 42, "rare": 15, "legendary": 3},
    ("uncommon", False): {"common": 30, "uncommon": 45, "rare": 20, "legendary": 5},
    ("uncommon", True): {"common": 15, "uncommon": 50, "rare": 27, "legendary": 8},
    ("rare", False): {"uncommon": 55, "rare": 35, "legendary": 10},
    ("rare", True): {"uncommon": 40, "rare": 45, "legendary": 15},
    ("legendary", False): {"uncommon": 40, "rare": 40, "legendary": 20},
    ("legendary", True): {"uncommon": 30, "rare": 45, "legendary": 25},
}


@dataclass(frozen=True, slots=True)
class PetAccessory:
    accessory_id: str
    display_name: str
    tier: str
    weight: int  # out of the ACCESSORIES total
    emoji: str
    blurb: str
    # Semantic mount used by the compositor. Each creature sidecar supplies
    # independent headwear, neck, and chest anchors so unlike body shapes do
    # not force every accessory through one generic head transform.
    anchor: str = "headwear"


ACCESSORIES: dict[str, PetAccessory] = {
    "red_bow": PetAccessory(
        "red_bow", "Red Bow", "common", 250, "🎀",
        "A classic. Ties the whole cama together."),
    "straw_hat": PetAccessory(
        "straw_hat", "Straw Hat", "common", 250, "👒",
        "Practical headwear for the discerning grazer."),
    "bell_collar": PetAccessory(
        "bell_collar", "Bell Collar", "common", 250, "🔔",
        "Now you always know where it is. It hates that.", anchor="neck"),
    "wool_scarf": PetAccessory(
        "wool_scarf", "Wool Scarf", "uncommon", 90, "🧣",
        "Wool on wool. Scandalous, cozy.", anchor="neck"),
    "flower_crown": PetAccessory(
        "flower_crown", "Flower Crown", "uncommon", 90, "🌸",
        "Woven from tombstone-garden flowers. Don't tell it that."),
    "top_hat": PetAccessory(
        "top_hat", "Top Hat", "rare", 30, "🎩",
        "Immediately becomes the most distinguished animal in the league."),
    "aegis_charm": PetAccessory(
        "aegis_charm", "Aegis Charm", "rare", 30, "🛡️",
        "A tiny replica. Grants nothing but confidence.", anchor="chest"),
    "divine_rapier_pin": PetAccessory(
        "divine_rapier_pin", "Divine Rapier Pin", "legendary", 10, "⚔️",
        "So shiny it feels like a throw waiting to happen.", anchor="chest"),
}

_FALLBACK_ACCESSORY = PetAccessory("unknown", "Curious Trinket", "common", 0, "🎁", "")


def get_accessory(accessory_id: str) -> PetAccessory:
    return ACCESSORIES.get(accessory_id, _FALLBACK_ACCESSORY)


def species_ids_by_tier(tier: str) -> list[str]:
    return [s.species_id for s in SPECIES.values() if s.tier == tier]


def get_species(species_id: str) -> PetSpecies:
    """Look up a species, tolerating ids that predate the current roster."""
    return SPECIES.get(species_id, _FALLBACK_SPECIES)


def adoption_fee(prior_deaths: int) -> int:
    """Escalating adoption fee, clamped to the final tier."""
    index = min(max(prior_deaths, 0), len(ADOPTION_FEES) - 1)
    return ADOPTION_FEES[index]


def food_cost(species_id: str, item_id: str) -> int:
    """Item cost for a given owner species (Frostwool discount applies)."""
    item = SALT_LICK if item_id == SALT_LICK.item_id else FOOD_ITEMS[item_id]
    return max(1, item.cost * get_species(species_id).food_cost_pct // 100)
