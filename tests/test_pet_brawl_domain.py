"""Tests for the pure pet-brawl combat engine."""

import random
from dataclasses import replace

import pytest

from domain.models.pet import PetStage
from domain.pet_brawl import (
    BASE_CRIT_PCT,
    BRAWL_TRAITS,
    HUNKER_COUNTER,
    HUNKER_HEAL,
    MAX_ROUNDS,
    MOVE_BLURBS,
    MOVE_EMOJI,
    MYSTIC_COUNTER,
    SAFE_MOVE,
    SPITTER_CRIT_PCT,
    STAMPEDE_MISS_PCT,
    TRAIT_BLURBS,
    BrawlTraits,
    PetBrawlMove,
    brawl_traits,
    build_duelist,
    initial_state,
    move_name,
    resolve_round,
)
from domain.pet_constants import SPECIES


class FakeRng:
    """Scripted randint values, consumed in call order."""

    def __init__(self, values):
        self.values = list(values)

    def randint(self, low, high):
        value = self.values.pop(0)
        assert low <= value <= high, f"scripted {value} outside [{low}, {high}]"
        return value


def mk(
    species="common_cama",
    *,
    pet_id=1,
    owner_id=100,
    name=None,
    adult=True,
    hunger=100,
    happy=False,
    aegis_used=0,
):
    return build_duelist(
        pet_id=pet_id,
        owner_id=owner_id,
        name=name or species,
        species_id=species,
        stage=PetStage.ADULT if adult else PetStage.BABY,
        hunger=hunger,
        happy=happy,
        aegis_used=aegis_used,
    )


class TestBuildDuelist:
    def test_hp_and_power_derivation(self):
        cases = [
            # species, adult, hunger, happy -> max_hp, hp, power
            ("common_cama", True, 100, False, 120, 120, 0),
            ("common_cama", False, 100, False, 100, 100, 0),
            ("common_cama", True, 0, False, 120, 100, 0),
            ("common_cama", True, 55, True, 120, 111, 1),
            ("jopacama", True, 100, False, 120, 120, 1),
            ("aegis_cama", False, 40, False, 100, 88, 1),
            ("rama", True, 100, True, 120, 120, 3),
        ]
        for species, adult, hunger, happy, max_hp, hp, power in cases:
            d = mk(species, adult=adult, hunger=hunger, happy=happy)
            assert (d.max_hp, d.hp, d.power) == (max_hp, hp, power), species

    def test_shield_gated_on_unspent_aegis(self):
        assert mk("aegis_cama").shield_available
        assert not mk("aegis_cama", aegis_used=1).shield_available
        assert not mk("common_cama").shield_available

    def test_unknown_species_uses_fallback(self):
        d = mk("retired_species")
        assert d.tier == "common"
        assert d.power == 0


class TestMoveFlavor:
    def test_species_flavored_names(self):
        assert move_name("rama", PetBrawlMove.SPIT) == "Royal Spit"
        assert move_name("crystal_cama", PetBrawlMove.STAMPEDE) == "Blizzard Stampede"

    def test_unknown_species_falls_back_to_defaults(self):
        assert move_name("retired_species", PetBrawlMove.SPIT) == "Spit"
        assert move_name("retired_species", PetBrawlMove.HUNKER) == "Hunker Down"

    def test_every_move_has_emoji(self):
        assert set(MOVE_EMOJI) == set(PetBrawlMove)

    def test_every_move_has_a_blurb(self):
        assert set(MOVE_BLURBS) == set(PetBrawlMove)


class TestBrawlTraits:
    def test_every_species_has_an_explicit_entry(self):
        """Lock the table's coverage: adding a species to pet_constants
        requires an explicit BRAWL_TRAITS entry (no-trait entries count)."""
        assert set(BRAWL_TRAITS) == set(SPECIES)

    def test_unknown_species_falls_back_to_no_traits(self):
        assert brawl_traits("retired_species") == BrawlTraits()

    def test_trait_blurbs_cover_exactly_the_quirked_species(self):
        """A typo'd or missing TRAIT_BLURBS key fails silently (the quirk
        line just never renders), so lock it to the quirked trait set."""
        quirked = {
            sid for sid, traits in BRAWL_TRAITS.items() if traits != BrawlTraits()
        }
        assert set(TRAIT_BLURBS) == quirked


class TestResolveRound:
    def test_spit_deals_scripted_damage_both_ways(self):
        state = initial_state(mk(name="A"), mk(name="B"))
        # a: dmg 10, no crit; b: dmg 12, no crit.
        rng = FakeRng([10, 100, 12, 100])
        new, log = resolve_round(state, SAFE_MOVE, SAFE_MOVE, rng)
        assert new.a.hp == 120 - 12
        assert new.b.hp == 120 - 10
        assert new.winner is None
        assert new.round_no == 1
        assert any("hits for 10" in line for line in log)

    def test_crit_doubles_damage(self):
        state = initial_state(mk(name="A"), mk(name="B"))
        rng = FakeRng([10, 1, 12, 100])  # a crits
        new, log = resolve_round(state, SAFE_MOVE, SAFE_MOVE, rng)
        assert new.b.hp == 120 - 20
        assert any("CRITICAL" in line for line in log)

    def test_stampede_can_miss(self):
        state = initial_state(mk(name="A"), mk(name="B"))
        # a's stampede miss roll 35 (<=35 misses); b spits 8, no crit.
        rng = FakeRng([35, 8, 100])
        new, log = resolve_round(state, PetBrawlMove.STAMPEDE, SAFE_MOVE, rng)
        assert new.b.hp == 120
        assert any("misses" in line for line in log)

    def test_hunker_halves_heals_and_counters(self):
        state = initial_state(mk(name="A"), mk(name="B"))
        # a spits 11 no crit; b hunkers, heal 7.
        rng = FakeRng([11, 100, 7])
        new, _ = resolve_round(state, SAFE_MOVE, PetBrawlMove.HUNKER, rng)
        # b takes ceil(11/2)=6, heals 7 (capped at max) -> 120 - 6 + 7 -> 120 cap.
        assert new.b.hp == 120
        # a takes the counter: 5 + power 0.
        assert new.a.hp == 120 - HUNKER_COUNTER

    def test_hunker_counter_only_when_attack_lands(self):
        state = initial_state(mk(name="A"), mk(name="B"))
        # a's stampede misses (roll 1); b hunkers, heal 5.
        rng = FakeRng([1, 5])
        new, _ = resolve_round(state, PetBrawlMove.STAMPEDE, PetBrawlMove.HUNKER, rng)
        assert new.a.hp == 120  # no counter
        assert new.b.hp == 120

    def test_mystic_counter_is_stronger(self):
        state = initial_state(mk(name="A"), mk("invoker_cama", name="B"))
        rng = FakeRng([11, 100, 7])
        new, _ = resolve_round(state, SAFE_MOVE, PetBrawlMove.HUNKER, rng)
        # Mystic is rare tier: counter 8 + power 1.
        assert new.a.hp == 120 - (MYSTIC_COUNTER + 1)

    def test_dune_reduces_damage_with_floor_one(self):
        state = initial_state(mk(name="A"), mk("dromedary_cross", name="B"))
        rng = FakeRng([8, 100, 8, 100])
        new, _ = resolve_round(state, SAFE_MOVE, SAFE_MOVE, rng)
        assert new.b.hp == 120 - 7  # 8 - 1 hardy
        # Hardy reduction applies after hunker halving, never below 1.
        b = replace(mk("dromedary_cross", name="B"), hp=50)
        state = initial_state(mk(name="A"), b)
        rng = FakeRng([8, 100, 5])  # a spits 8; b hunkers, heals 5
        new, _ = resolve_round(state, SAFE_MOVE, PetBrawlMove.HUNKER, rng)
        assert new.b.hp == 50 - 3 + 5  # ceil(8/2)=4, -1 hardy = 3

    def test_ravenous_deals_and_takes_extra(self):
        state = initial_state(mk("pudge_cama", name="A"), mk(name="B"))
        rng = FakeRng([10, 100, 10, 100])
        new, _ = resolve_round(state, SAFE_MOVE, SAFE_MOVE, rng)
        assert new.b.hp == 120 - (10 + 1 + 1)  # +1 power(uncommon)? no: dmg+power
        # Ravenous is uncommon: damage = 10 + power 1 + ravenous 1 = 12.
        assert new.a.hp == 120 - (10 + 1)  # b's 10 + ravenous taken 1

    def test_ravenous_hunker_heals_extra(self):
        state = initial_state(mk(name="A"), mk("pudge_cama", name="B", hunger=0))
        # b starts at 100 hp (hunger 0 -> -20). a stampede misses; b heals 5+3.
        rng = FakeRng([1, 5])
        new, _ = resolve_round(state, PetBrawlMove.STAMPEDE, PetBrawlMove.HUNKER, rng)
        assert new.b.hp == 100 + 8

    def test_frostwool_makes_stampedes_miss_more(self):
        state = initial_state(mk(name="A"), mk("crystal_cama", name="B"))
        # Roll of 45 misses only with the +10pp chill bonus.
        rng = FakeRng([STAMPEDE_MISS_PCT + 10, 8, 100])
        new, _ = resolve_round(state, PetBrawlMove.STAMPEDE, SAFE_MOVE, rng)
        assert new.b.hp == 120

    def test_shellback_shield_saves_once(self):
        a = mk(name="A")
        b = replace(mk("aegis_cama", name="B"), hp=5)
        state = initial_state(a, b)
        rng = FakeRng([10, 100, 8, 100])
        new, log = resolve_round(state, SAFE_MOVE, SAFE_MOVE, rng)
        assert new.b.hp == 1
        assert not new.b.shield_available
        assert any("survives on 1 HP" in line for line in log)
        # Second lethal hit goes through.
        rng = FakeRng([10, 100, 8, 100])
        new2, _ = resolve_round(new, SAFE_MOVE, SAFE_MOVE, rng)
        assert new2.winner == "a"

    def test_shellback_with_spent_aegis_has_no_shield(self):
        b = replace(mk("aegis_cama", name="B", aegis_used=1), hp=5)
        state = initial_state(mk(name="A"), b)
        rng = FakeRng([10, 100, 8, 100])
        new, _ = resolve_round(state, SAFE_MOVE, SAFE_MOVE, rng)
        assert new.winner == "a"

    def test_simultaneous_ko_less_negative_hp_wins(self):
        a = replace(mk(name="A"), hp=5)
        b = replace(mk(name="B"), hp=10)
        state = initial_state(a, b)
        rng = FakeRng([16, 100, 16, 100])  # both take 16
        new, log = resolve_round(state, SAFE_MOVE, SAFE_MOVE, rng)
        # a at -11, b at -6 -> b wins.
        assert new.winner == "b"
        assert any("Double knockout" in line for line in log)

    def test_simultaneous_ko_exact_tie_coin_flips(self):
        a = replace(mk(name="A"), hp=5)
        b = replace(mk(name="B"), hp=5)
        state = initial_state(a, b)
        rng = FakeRng([16, 100, 16, 100, 1])  # coin flip -> b
        new, _ = resolve_round(state, SAFE_MOVE, SAFE_MOVE, rng)
        assert new.winner == "b"

    def test_round_cap_forces_winner_by_hp_pct(self):
        a = replace(mk(name="A"), hp=100)
        b = replace(mk(name="B"), hp=50)
        state = initial_state(a, b)
        state = type(state)(a=state.a, b=state.b, round_no=MAX_ROUNDS - 1, winner=None)
        rng = FakeRng([10, 100, 10, 100])
        new, log = resolve_round(state, SAFE_MOVE, SAFE_MOVE, rng)
        assert new.winner == "a"
        assert any("judges" in line for line in log)

    def test_resolve_on_finished_state_raises(self):
        state = initial_state(mk(name="A"), mk(name="B"))
        finished = type(state)(a=state.a, b=state.b, round_no=3, winner="a")
        with pytest.raises(ValueError):
            resolve_round(finished, SAFE_MOVE, SAFE_MOVE, FakeRng([]))


class TestTermination:
    def test_any_seed_terminates_by_max_rounds(self):
        moves = list(PetBrawlMove)
        for seed in range(60):
            rng = random.Random(seed)
            state = initial_state(
                mk("rama", name="A", happy=True),
                mk("aegis_cama", name="B"),
            )
            rounds = 0
            while state.winner is None:
                state, _ = resolve_round(
                    state, rng.choice(moves), rng.choice(moves), rng
                )
                rounds += 1
                assert rounds <= MAX_ROUNDS
            assert state.winner in ("a", "b")


class TestStatisticalTuning:
    def test_stampede_miss_rate_near_tuning(self):
        rng = random.Random(7)
        misses = 0
        trials = 3000
        for _ in range(trials):
            state = initial_state(mk(name="A"), mk(name="B"))
            _, log = resolve_round(state, PetBrawlMove.STAMPEDE, SAFE_MOVE, rng)
            if any("misses" in line for line in log):
                misses += 1
        assert abs(misses / trials - STAMPEDE_MISS_PCT / 100) < 0.04

    def test_rama_crits_more_than_base(self):
        rng = random.Random(11)

        def crit_rate(species):
            crits = 0
            trials = 3000
            for _ in range(trials):
                state = initial_state(mk(species, name="A"), mk(name="B"))
                _, log = resolve_round(state, SAFE_MOVE, PetBrawlMove.HUNKER, rng)
                if any("CRITICAL" in line for line in log):
                    crits += 1
            return crits / trials

        assert abs(crit_rate("rama") - SPITTER_CRIT_PCT / 100) < 0.04
        assert abs(crit_rate("common_cama") - BASE_CRIT_PCT / 100) < 0.04

    def test_spit_always_lands_and_draws_the_counter(self):
        rng = random.Random(3)
        for _ in range(200):
            state = initial_state(mk(name="A"), mk(name="B"))
            new, _ = resolve_round(state, SAFE_MOVE, PetBrawlMove.HUNKER, rng)
            assert new.b.hp <= 120  # heal capped at max
            assert 120 - new.a.hp == HUNKER_COUNTER  # spit never misses

    def test_hunker_heal_within_range(self):
        rng = random.Random(5)
        _, high = HUNKER_HEAL
        for _ in range(200):
            a = replace(mk(name="A"), hp=50)
            state = initial_state(a, mk(name="B"))
            # b's spit may or may not crit; a heals afterwards.
            new, _ = resolve_round(state, PetBrawlMove.HUNKER, SAFE_MOVE, rng)
            # Heal amount is bounded, so hp change from heal alone is in range.
            assert new.a.hp <= 50 + high
