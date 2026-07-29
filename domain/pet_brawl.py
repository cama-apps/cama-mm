"""Pure combat engine for pet brawls.

Everything here is deterministic given an injected `random.Random`, so the
whole fight is unit-testable. No I/O, no clocks, no config —
duelists are built from a snapshot of pet state taken once at accept time,
and nothing in a brawl mutates a pet (settlement happens elsewhere).

Balance philosophy (user-tuned): care matters fully (hunger docks starting
HP, a pampered pet gets +1 power), rarity tier is nearly flat so quirks and
move choices dominate, and the RNG is deliberately swingy — wide damage
ranges and loud crits make better drama than a tight tactical meta.
"""

from __future__ import annotations

import random
from dataclasses import dataclass, replace
from enum import StrEnum

from domain.models.pet import PetStage
from domain.pet_constants import get_species

MAX_ROUNDS = 8

# Near-flat tier edge (~55/45 up-tier): power adds to attack and counter
# damage only. Legendaries feel a touch stronger, never dominant.
TIER_POWER = {"common": 0, "uncommon": 1, "rare": 1, "legendary": 2}

BASE_HP = 80
ADULT_HP_BONUS = 20  # adults 100 max HP, babies 80
HUNGER_HP_PENALTY_DIV = 5  # start_hp = max_hp - (100 - hunger) // 5

SPIT_DMG = (8, 16)  # safe move, never misses
STAMPEDE_DMG = (16, 32)
STAMPEDE_MISS_PCT = 55
HUNKER_HEAL = (2, 4)
HUNKER_COUNTER = 5  # + power; 8 base for the Mystic Cama
HUNKER_DAMAGE_TAKEN_PCT = 50
HUNKER_STAMPEDE_DAMAGE_TAKEN_PCT = 60
MYSTIC_COUNTER = 8
BASE_CRIT_PCT = 10  # crit = double damage
SPITTER_CRIT_PCT = 25  # Rama
CHILL_MISS_BONUS_PP = 10  # Frostwool: opponent's Stampede misses more
RAVENOUS_HEAL_BONUS = 3  # Ravenous: hunker heals a little extra
TRAINING_STR_DAMAGE = 1
TRAINING_STR_STAMPEDE_BONUS = 1
TRAINING_DEX_CRIT_PP = 2
TRAINING_DEX_SPIT_CRIT_BONUS_PP = 2
TRAINING_DEX_DODGE_PP = 2
TRAINING_INT_MITIGATION = 1
TRAINING_INT_HUNKER_BONUS = 1


class PetBrawlMove(StrEnum):
    SPIT = "spit"
    STAMPEDE = "stampede"
    HUNKER = "hunker"


SAFE_MOVE = PetBrawlMove.SPIT

MOVE_EMOJI = {
    PetBrawlMove.SPIT: "💦",
    PetBrawlMove.STAMPEDE: "🐫",
    PetBrawlMove.HUNKER: "🛡️",
}

# Player-facing move explanations, shown in the round-1 battle embed.
MOVE_BLURBS = {
    PetBrawlMove.SPIT: "reliable chip damage, never misses.",
    PetBrawlMove.STAMPEDE: (
        f"big damage, {STAMPEDE_MISS_PCT}% chance to whiff, punches through Hunker."
    ),
    PetBrawlMove.HUNKER: (
        "halve Spit damage, soften Stampede, heal a little, counter any hit."
    ),
}

_DEFAULT_MOVE_NAMES = {
    PetBrawlMove.SPIT: "Spit",
    PetBrawlMove.STAMPEDE: "Stampede",
    PetBrawlMove.HUNKER: "Hunker Down",
}

# Mechanics are identical for everyone; only the words change. Unknown or
# retired species fall through to the defaults, same philosophy as
# get_species.
MOVE_FLAVOR: dict[tuple[str, PetBrawlMove], str] = {
    ("common_cama", PetBrawlMove.SPIT): "Unremarkable Spit",
    ("common_cama", PetBrawlMove.STAMPEDE): "Ordinary Stampede",
    ("common_cama", PetBrawlMove.HUNKER): "Stand There",
    ("dromedary_cross", PetBrawlMove.SPIT): "Sandy Spit",
    ("dromedary_cross", PetBrawlMove.STAMPEDE): "Dune Charge",
    ("dromedary_cross", PetBrawlMove.HUNKER): "Close Nostrils",
    ("banana_ears", PetBrawlMove.SPIT): "Melodic Spit",
    ("banana_ears", PetBrawlMove.STAMPEDE): "Humming Charge",
    ("banana_ears", PetBrawlMove.HUNKER): "Hum Defensively",
    ("embergear_cama", PetBrawlMove.SPIT): "Spark Spit",
    ("embergear_cama", PetBrawlMove.STAMPEDE): "Overdrive",
    ("embergear_cama", PetBrawlMove.HUNKER): "Vent Heat",
    ("jopacama", PetBrawlMove.SPIT): "Coin-Flecked Spit",
    ("jopacama", PetBrawlMove.STAMPEDE): "Market Crash",
    ("jopacama", PetBrawlMove.HUNKER): "Audit Defense",
    ("pudge_cama", PetBrawlMove.SPIT): "Sloppy Spit",
    ("pudge_cama", PetBrawlMove.STAMPEDE): "Dinner Rush",
    ("pudge_cama", PetBrawlMove.HUNKER): "Digest",
    ("courier_cama", PetBrawlMove.SPIT): "Special Delivery",
    ("courier_cama", PetBrawlMove.STAMPEDE): "Rush Delivery",
    ("courier_cama", PetBrawlMove.HUNKER): "Hide Behind Saddlebags",
    ("riverglow_cama", PetBrawlMove.SPIT): "Charged Spit",
    ("riverglow_cama", PetBrawlMove.STAMPEDE): "River Rush",
    ("riverglow_cama", PetBrawlMove.HUNKER): "Bottle Up",
    ("aegis_cama", PetBrawlMove.SPIT): "Serene Spit",
    ("aegis_cama", PetBrawlMove.STAMPEDE): "Shell Rush",
    ("aegis_cama", PetBrawlMove.HUNKER): "Shell Up",
    ("invoker_cama", PetBrawlMove.SPIT): "Forged Spit",
    ("invoker_cama", PetBrawlMove.STAMPEDE): "Orb-Powered Charge",
    ("invoker_cama", PetBrawlMove.HUNKER): "Quas Ward",
    ("crystal_cama", PetBrawlMove.SPIT): "Frost Spit",
    ("crystal_cama", PetBrawlMove.STAMPEDE): "Blizzard Stampede",
    ("crystal_cama", PetBrawlMove.HUNKER): "Frozen Stance",
    ("prismwool_cama", PetBrawlMove.SPIT): "Prismatic Spit",
    ("prismwool_cama", PetBrawlMove.STAMPEDE): "Linked Charge",
    ("prismwool_cama", PetBrawlMove.HUNKER): "Wool Ward",
    ("rama", PetBrawlMove.SPIT): "Royal Spit",
    ("rama", PetBrawlMove.STAMPEDE): "Temperamental Rampage",
    ("rama", PetBrawlMove.HUNKER): "Refuse to Cooperate",
}


def move_name(species_id: str, move: PetBrawlMove) -> str:
    return MOVE_FLAVOR.get((species_id, move), _DEFAULT_MOVE_NAMES[move])


@dataclass(frozen=True, slots=True)
class BrawlTraits:
    """One species' combat quirks. Every field defaults to 'no quirk'."""

    dodge_bonus_pp: int = 0  # added to the opponent's Stampede miss chance
    crit_pct: int = BASE_CRIT_PCT
    damage_dealt_bonus: int = 0  # flat, applied before hunker reduction
    damage_taken_mod: int = 0  # flat; a negative mod never drops a hit below 1
    counter_base: int = HUNKER_COUNTER  # power is added on top
    heal_bonus: int = 0  # added to hunker healing
    loss_insurance: bool = False  # settles a brawl loss for less hunger


_NO_TRAITS = BrawlTraits()

# Explicit per-species combat quirks, keyed by species_id like MOVE_FLAVOR.
# Every species in pet_constants.SPECIES has an entry (_NO_TRAITS = explicit
# no-quirk); unknown or retired species fall through to _NO_TRAITS. The
# Shellback's shell save is not a trait: it rides on real Aegis state and is
# handled in build_duelist.
BRAWL_TRAITS: dict[str, BrawlTraits] = {
    "common_cama": _NO_TRAITS,
    "dromedary_cross": BrawlTraits(damage_taken_mod=-1),  # Dune is hardy
    "banana_ears": _NO_TRAITS,
    "embergear_cama": _NO_TRAITS,
    "jopacama": BrawlTraits(loss_insurance=True),
    "pudge_cama": BrawlTraits(  # Ravenous glass cannon
        damage_dealt_bonus=1,
        damage_taken_mod=1,
        heal_bonus=RAVENOUS_HEAL_BONUS,
    ),
    "courier_cama": _NO_TRAITS,
    "riverglow_cama": _NO_TRAITS,
    "aegis_cama": _NO_TRAITS,
    "invoker_cama": BrawlTraits(counter_base=MYSTIC_COUNTER),
    "crystal_cama": BrawlTraits(dodge_bonus_pp=CHILL_MISS_BONUS_PP),
    "prismwool_cama": _NO_TRAITS,
    "rama": BrawlTraits(crit_pct=SPITTER_CRIT_PCT),
}


def brawl_traits(species_id: str) -> BrawlTraits:
    return BRAWL_TRAITS.get(species_id, _NO_TRAITS)


# Player-facing one-liners for BRAWL_TRAITS, shown in the round-1 battle
# embed so quirks aren't invisible. Quirkless species are simply absent.
TRAIT_BLURBS: dict[str, str] = {
    "dromedary_cross": "hardy — shrugs a point off every hit",
    "jopacama": "insured — loses less hunger in defeat",
    "pudge_cama": "ravenous — hits harder, takes more, heals extra hunkered",
    "invoker_cama": "mystic — counters harder while hunkered",
    "crystal_cama": "chilling — enemy Stampedes whiff more often",
    "rama": "royal temper — crits far more often",
}

# Shellback save (rides on Aegis state, see build_duelist), shown per-duelist
# when the shield is actually available this brawl.
SHELL_BLURB = "shelled — survives the first knockout on 1 HP"


@dataclass(frozen=True, slots=True)
class Duelist:
    pet_id: int
    owner_id: int
    name: str
    species_id: str
    tier: str
    is_adult: bool
    power: int
    training_str: int
    training_int: int
    training_dex: int
    max_hp: int
    hp: int
    shield_available: bool  # per-brawl Shellback save; never burns the real Aegis


@dataclass(frozen=True, slots=True)
class PetBrawlState:
    a: Duelist
    b: Duelist
    round_no: int  # completed rounds
    winner: str | None  # "a" | "b" | None while the fight is live


def build_duelist(
    *,
    pet_id: int,
    owner_id: int,
    name: str,
    species_id: str,
    stage: PetStage,
    hunger: int,
    happy: bool,
    aegis_used: int,
    training_str: int = 0,
    training_int: int = 0,
    training_dex: int = 0,
) -> Duelist:
    """Snapshot a pet into a duelist. `happy` = mood HAPPY or pampered."""
    species = get_species(species_id)
    is_adult = stage is PetStage.ADULT
    max_hp = BASE_HP + (ADULT_HP_BONUS if is_adult else 0)
    hp = max_hp - (100 - max(0, min(100, hunger))) // HUNGER_HP_PENALTY_DIV
    power = TIER_POWER.get(species.tier, 0) + (1 if happy else 0)
    return Duelist(
        pet_id=pet_id,
        owner_id=owner_id,
        name=name,
        species_id=species_id,
        tier=species.tier,
        is_adult=is_adult,
        power=power,
        training_str=training_str,
        training_int=training_int,
        training_dex=training_dex,
        max_hp=max_hp,
        hp=hp,
        shield_available=species.has_aegis and not aegis_used,
    )


def initial_state(a: Duelist, b: Duelist) -> PetBrawlState:
    return PetBrawlState(a=a, b=b, round_no=0, winner=None)


def _attack_damage(
    attacker: Duelist,
    defender: Duelist,
    move: PetBrawlMove,
    defender_hunkers: bool,
    rng: random.Random,
) -> tuple[int, bool, bool]:
    """Return (damage, landed, crit) for one attack, defender mods applied."""
    atk = brawl_traits(attacker.species_id)
    dfn = brawl_traits(defender.species_id)
    if move is PetBrawlMove.HUNKER:
        return 0, False, False
    if move is PetBrawlMove.STAMPEDE:
        miss_pct = (
            STAMPEDE_MISS_PCT
            + dfn.dodge_bonus_pp
            + defender.training_dex * TRAINING_DEX_DODGE_PP
        )
        if rng.randint(1, 100) <= miss_pct:
            return 0, False, False
        low, high = STAMPEDE_DMG
    else:
        low, high = SPIT_DMG
    strength_damage = attacker.training_str * TRAINING_STR_DAMAGE
    if move is PetBrawlMove.STAMPEDE:
        strength_damage += (
            attacker.training_str * TRAINING_STR_STAMPEDE_BONUS
        )
    dmg = rng.randint(low, high) + attacker.power + strength_damage
    crit_pct = atk.crit_pct + attacker.training_dex * TRAINING_DEX_CRIT_PP
    if move is PetBrawlMove.SPIT:
        crit_pct += (
            attacker.training_dex * TRAINING_DEX_SPIT_CRIT_BONUS_PP
        )
    crit = rng.randint(1, 100) <= crit_pct
    if crit:
        dmg *= 2
    dmg += atk.damage_dealt_bonus  # Ravenous hits harder...
    if defender_hunkers:
        damage_taken_pct = (
            HUNKER_STAMPEDE_DAMAGE_TAKEN_PCT
            if move is PetBrawlMove.STAMPEDE
            else HUNKER_DAMAGE_TAKEN_PCT
        )
        dmg = -(-(dmg * damage_taken_pct) // 100)
    dmg += (
        dfn.damage_taken_mod
        - defender.training_int * TRAINING_INT_MITIGATION
    )
    if dfn.damage_taken_mod < 0 or defender.training_int:
        dmg = max(1, dmg)
    return dmg, True, crit


def _counter_damage(hunkerer: Duelist) -> int:
    return (
        brawl_traits(hunkerer.species_id).counter_base
        + hunkerer.power
        + hunkerer.training_int * TRAINING_INT_HUNKER_BONUS
    )


def _hunker_heal(hunkerer: Duelist, rng: random.Random) -> int:
    return (
        rng.randint(*HUNKER_HEAL)
        + brawl_traits(hunkerer.species_id).heal_bonus
        + hunkerer.training_int * TRAINING_INT_HUNKER_BONUS
    )


def _hp_pct(d: Duelist) -> int:
    return d.hp * 100 // d.max_hp


def resolve_round(
    state: PetBrawlState,
    move_a: PetBrawlMove,
    move_b: PetBrawlMove,
    rng: random.Random,
) -> tuple[PetBrawlState, tuple[str, ...]]:
    """Resolve one simultaneous round. Guaranteed winner by MAX_ROUNDS."""
    if state.winner is not None:
        raise ValueError("brawl already resolved")
    a, b = state.a, state.b
    log: list[str] = []

    # Both attacks read the round-start state; damage lands simultaneously.
    dmg_to_b, a_landed, a_crit = _attack_damage(
        a, b, move_a, move_b is PetBrawlMove.HUNKER, rng
    )
    dmg_to_a, b_landed, b_crit = _attack_damage(
        b, a, move_b, move_a is PetBrawlMove.HUNKER, rng
    )
    heal_a = _hunker_heal(a, rng) if move_a is PetBrawlMove.HUNKER else 0
    heal_b = _hunker_heal(b, rng) if move_b is PetBrawlMove.HUNKER else 0
    if move_a is PetBrawlMove.HUNKER and b_landed:
        dmg_to_b += _counter_damage(a)
    if move_b is PetBrawlMove.HUNKER and a_landed:
        dmg_to_a += _counter_damage(b)

    for atk, dfn, move, landed, crit, dmg in (
        (a, b, move_a, a_landed, a_crit, dmg_to_b),
        (b, a, move_b, b_landed, b_crit, dmg_to_a),
    ):
        label = move_name(atk.species_id, move)
        if move is PetBrawlMove.HUNKER:
            log.append(f"🛡️ **{atk.name}** hunkers down ({label}).")
        elif not landed:
            log.append(f"💨 **{atk.name}**'s {label} misses entirely!")
        elif crit:
            log.append(f"💥 CRITICAL! **{atk.name}**'s {label} devastates for {dmg}!")
        else:
            log.append(f"{MOVE_EMOJI[move]} **{atk.name}**'s {label} hits for {dmg}.")
    if heal_a:
        log.append(f"💚 **{a.name}** recovers {heal_a} HP.")
    if heal_b:
        log.append(f"💚 **{b.name}** recovers {heal_b} HP.")
    if move_a is PetBrawlMove.HUNKER and b_landed:
        log.append(f"⚡ **{a.name}** counters for {_counter_damage(a)}!")
    if move_b is PetBrawlMove.HUNKER and a_landed:
        log.append(f"⚡ **{b.name}** counters for {_counter_damage(b)}!")

    new_hp_a = min(a.max_hp, a.hp - dmg_to_a + heal_a)
    new_hp_b = min(b.max_hp, b.hp - dmg_to_b + heal_b)
    shield_a, shield_b = a.shield_available, b.shield_available
    if new_hp_a <= 0 and shield_a:
        new_hp_a, shield_a = 1, False
        log.append(f"🛡️ **{a.name}**'s shell flashes — it survives on 1 HP!")
    if new_hp_b <= 0 and shield_b:
        new_hp_b, shield_b = 1, False
        log.append(f"🛡️ **{b.name}**'s shell flashes — it survives on 1 HP!")

    a = replace(a, hp=new_hp_a, shield_available=shield_a)
    b = replace(b, hp=new_hp_b, shield_available=shield_b)
    round_no = state.round_no + 1
    winner: str | None = None

    if new_hp_a <= 0 and new_hp_b <= 0:
        if new_hp_a != new_hp_b:
            winner = "a" if new_hp_a > new_hp_b else "b"
        else:
            winner = "a" if rng.randint(0, 1) == 0 else "b"
        standing = a if winner == "a" else b
        log.append(f"☄️ Double knockout! **{standing.name}** staggers up last!")
    elif new_hp_b <= 0:
        winner = "a"
        log.append(f"🏆 **{b.name}** is knocked out!")
    elif new_hp_a <= 0:
        winner = "b"
        log.append(f"🏆 **{a.name}** is knocked out!")
    elif round_no >= MAX_ROUNDS:
        pct_a, pct_b = _hp_pct(a), _hp_pct(b)
        if pct_a != pct_b:
            winner = "a" if pct_a > pct_b else "b"
        else:
            winner = "a" if rng.randint(0, 1) == 0 else "b"
        judge = a if winner == "a" else b
        log.append(
            f"🔔 The judges call it after {MAX_ROUNDS} rounds — "
            f"**{judge.name}** takes it!"
        )

    return PetBrawlState(a=a, b=b, round_no=round_no, winner=winner), tuple(log)
