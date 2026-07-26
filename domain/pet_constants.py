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

SPECIES_WEIGHT_TOTAL = 10_000


@dataclass(frozen=True, slots=True)
class PetFood:
    item_id: str
    display_name: str
    cost: int
    restore: int
    emoji: str
    blurb: str


# Sized against the wider economy: dig consumables run 4-8 JC, an inhouse
# night pays ~15-30 JC. Mild bulk discount rewards planning.
FOOD_ITEMS: dict[str, PetFood] = {
    "tango": PetFood(
        "tango", "Tango", cost=5, restore=25, emoji="🌿",
        blurb="Classic starter regen. Your cama eats a tree.",
    ),
    "alfalfa": PetFood(
        "alfalfa", "Alfalfa Bale", cost=9, restore=50, emoji="🌾",
        blurb="Hearty greens for a hearty hybrid.",
    ),
    "cheese": PetFood(
        "cheese", "Roshan's Cheese", cost=16, restore=100, emoji="🧀",
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
        "common_cama", "Common Cama", "common", 2000,
        "A camel-llama hybrid of no particular distinction. Its quirk is "
        "having none.",
    ),
    "dromedary_cross": PetSpecies(
        "dromedary_cross", "Dune Cama", "common", 2000,
        "Takes after its dromedary side: heroic eyelashes, closable "
        "nostrils, and a modest fat reserve where a hump should be.",
        decay_pct=90,
    ),
    "banana_ears": PetSpecies(
        "banana_ears", "Humming Cama", "common", 2000,
        "Llama-leaning, with the characteristic banana ears. Hums "
        "constantly. Nobody knows why. It is communication.",
    ),
    "jopacama": PetSpecies(
        "jopacama", "Gilded Cama", "uncommon", 1000,
        "Its wool grows in faint coin-shaped whorls. Auditors are "
        "suspicious but can prove nothing.",
        refund_bonus_pp=10,
    ),
    "pudge_cama": PetSpecies(
        "pudge_cama", "Ravenous Cama", "uncommon", 1000,
        "Round, perpetually famished, and faster at dinner than anything "
        "its size should be. Something metal glints deep in its wool.",
        decay_pct=110,
        restore_pct=110,
    ),
    "courier_cama": PetSpecies(
        "courier_cama", "Pack Cama", "uncommon", 1000,
        "Saddlebags, a little flag, big anxious eyes. Lives to deliver. "
        "Everyone screams when it dies.",
        match_feed_bonus=5,
    ),
    "aegis_cama": PetSpecies(
        "aegis_cama", "Shellback Cama", "rare", 300,
        "Serene, with a faintly glowing shell across its back. Local "
        "legend says it gets one more chance than everything else.",
        has_aegis=True,
    ),
    "invoker_cama": PetSpecies(
        "invoker_cama", "Mystic Cama", "rare", 300,
        "Violet-robed, insufferably learned. Three small orbs circle its "
        "head. Enjoys salt more than most scholars admit.",
        salt_lick_pct=150,
    ),
    "crystal_cama": PetSpecies(
        "crystal_cama", "Frostwool Cama", "rare", 300,
        "Frost-blue wool that never melts. Somehow always finds the "
        "snacks at a discount. A true support.",
        food_cost_pct=90,
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
