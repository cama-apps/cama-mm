"""Tests for the pure pet-brawl combat engine."""

import random
from dataclasses import replace

import pytest

from domain.models.pet import PetStage
from domain.models.pet_brawl import PetBrawl
from domain.pet_brawl import (
    BASE_CRIT_PCT,
    BRAWL_TRAITS,
    HUNKER_HEAL,
    MAX_ROUNDS,
    MOVE_BLURBS,
    MOVE_EMOJI,
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


def test_pet_brawl_economy_and_training_fields_default_to_free_and_unawarded():
    brawl = PetBrawl(
        brawl_id=1,
        guild_id=100,
        channel_id=5,
        challenger_id=10,
        recipient_id=20,
        challenger_pet_id=1000,
        recipient_pet_id=None,
        status="pending",
        created_at=1_000,
        expires_at=1_300,
        resolved_at=None,
        winner_id=None,
        winner_pet_id=None,
        loser_pet_id=None,
        rounds=None,
        winner_hunger_delta=0,
        loser_hunger_delta=0,
    )

    assert brawl.wager == 0
    assert brawl.fee == 0
    assert brawl.challenger_xp_delta == 0
    assert brawl.recipient_xp_delta == 0
    assert brawl.challenger_stat_gain is None
    assert brawl.recipient_stat_gain is None
    assert brawl.personality_event_key is None


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
    training_str=0,
    training_int=0,
    training_dex=0,
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
        training_str=training_str,
        training_int=training_int,
        training_dex=training_dex,
    )


class TestBuildDuelist:
    def test_hp_and_power_derivation(self):
        cases = [
            # species, adult, hunger, happy -> max_hp, hp, power
            ("common_cama", True, 100, False, 100, 100, 0),
            ("common_cama", False, 100, False, 80, 80, 0),
            ("common_cama", True, 0, False, 100, 80, 0),
            ("common_cama", True, 55, True, 100, 91, 1),
            ("jopacama", True, 100, False, 100, 100, 1),
            ("aegis_cama", False, 40, False, 80, 68, 1),
            ("rama", True, 100, True, 100, 100, 3),
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

    def test_training_stats_are_snapshotted_without_changing_base_power(self):
        d = mk(training_str=2, training_int=1, training_dex=2)

        assert (d.training_str, d.training_int, d.training_dex) == (2, 1, 2)
        assert d.power == 0


class TestMoveFlavor:
    def test_species_flavored_names(self):
        assert move_name("rama", PetBrawlMove.SPIT) == "Royal Spit"
        assert move_name("crystal_cama", PetBrawlMove.STAMPEDE) == "Blizzard Stampede"

    def test_unknown_species_falls_back_to_defaults(self):
        assert move_name("retired_species", PetBrawlMove.SPIT) == "Spit"
        assert move_name("retired_species", PetBrawlMove.HUNKER) == "Hunker Down"

    def test_new_species_have_themed_move_names(self):
        expected = {
            ("embergear_cama", PetBrawlMove.SPIT): "Spark Spit",
            ("embergear_cama", PetBrawlMove.STAMPEDE): "Overdrive",
            ("embergear_cama", PetBrawlMove.HUNKER): "Vent Heat",
            ("riverglow_cama", PetBrawlMove.SPIT): "Charged Spit",
            ("riverglow_cama", PetBrawlMove.STAMPEDE): "River Rush",
            ("riverglow_cama", PetBrawlMove.HUNKER): "Bottle Up",
            ("prismwool_cama", PetBrawlMove.SPIT): "Prismatic Spit",
            ("prismwool_cama", PetBrawlMove.STAMPEDE): "Linked Charge",
            ("prismwool_cama", PetBrawlMove.HUNKER): "Wool Ward",
            ("moondrift_cama", PetBrawlMove.SPIT): "Moonlit Spit",
            ("moondrift_cama", PetBrawlMove.STAMPEDE): "Tidal Rush",
            ("moondrift_cama", PetBrawlMove.HUNKER): "Eclipse",
            ("sunspun_cama", PetBrawlMove.SPIT): "Solar Flare",
            ("sunspun_cama", PetBrawlMove.STAMPEDE): "Dawn Charge",
            ("sunspun_cama", PetBrawlMove.HUNKER): "Corona Guard",
        }

        assert {
            key: move_name(*key)
            for key in expected
        } == expected

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

    def test_new_legendary_species_have_distinct_traits(self):
        assert brawl_traits("moondrift_cama") == BrawlTraits(dodge_bonus_pp=15)
        assert brawl_traits("sunspun_cama") == BrawlTraits(counter_base=9)

    def test_trait_blurbs_cover_exactly_the_quirked_species(self):
        """A typo'd or missing TRAIT_BLURBS key fails silently (the quirk
        line just never renders), so lock it to the quirked trait set."""
        quirked = {
            sid for sid, traits in BRAWL_TRAITS.items() if traits != BrawlTraits()
        }
        assert set(TRAIT_BLURBS) == quirked


class TestResolveRound:
    def test_moondrift_evades_more_stampedes(self):
        state = initial_state(mk(name="A"), mk("moondrift_cama", name="B"))

        resolved, _ = resolve_round(
            state,
            PetBrawlMove.STAMPEDE,
            SAFE_MOVE,
            FakeRng([70, 16, 100, 8, 100]),
        )

        assert resolved.b.hp == 100

    def test_sunspun_hunkers_with_a_stronger_counter(self):
        state = initial_state(mk(name="A"), mk("sunspun_cama", name="B"))

        resolved, _ = resolve_round(
            state,
            SAFE_MOVE,
            PetBrawlMove.HUNKER,
            FakeRng([8, 100, 2]),
        )

        assert resolved.a.hp == 89

    def test_strength_adds_all_attack_damage_and_extra_stampede_damage(self):
        spit_state = initial_state(mk(name="A", training_str=1), mk(name="B"))
        spit, _ = resolve_round(spit_state, SAFE_MOVE, SAFE_MOVE, FakeRng([8, 100, 8, 100]))
        assert spit.b.hp == 100 - 9

        stampede_state = initial_state(mk(name="A", training_str=1), mk(name="B"))
        stampede, _ = resolve_round(
            stampede_state,
            PetBrawlMove.STAMPEDE,
            SAFE_MOVE,
            FakeRng([100, 16, 100, 8, 100]),
        )
        assert stampede.b.hp == 100 - 18

    def test_dexterity_improves_general_crit_spit_crit_and_stampede_evasion(self):
        spit_state = initial_state(mk(name="A", training_dex=1), mk(name="B"))
        spit, _ = resolve_round(spit_state, SAFE_MOVE, SAFE_MOVE, FakeRng([8, 14, 8, 100]))
        assert spit.b.hp == 100 - 16

        dodge_state = initial_state(mk(name="A"), mk(name="B", training_dex=1))
        dodge, _ = resolve_round(
            dodge_state,
            PetBrawlMove.STAMPEDE,
            SAFE_MOVE,
            FakeRng([STAMPEDE_MISS_PCT + 1, 8, 100]),
        )
        assert dodge.b.hp == 100

    def test_intelligence_reduces_damage_and_improves_hunker(self):
        state = initial_state(
            mk(name="A"),
            replace(mk(name="B", training_int=1), hp=70),
        )
        resolved, _ = resolve_round(
            state,
            SAFE_MOVE,
            PetBrawlMove.HUNKER,
            FakeRng([8, 100, 2]),
        )

        # Incoming 8 is halved to 4, then INT mitigates 1; Hunker heals 2+1.
        assert resolved.b.hp == 70 - 3 + 3
        assert resolved.a.hp == 100

    def test_spit_deals_scripted_damage_both_ways(self):
        state = initial_state(mk(name="A"), mk(name="B"))
        # a: dmg 10, no crit; b: dmg 12, no crit.
        rng = FakeRng([10, 100, 12, 100])
        new, log = resolve_round(state, SAFE_MOVE, SAFE_MOVE, rng)
        assert new.a.hp == 100 - 12
        assert new.b.hp == 100 - 10
        assert new.winner is None
        assert new.round_no == 1
        assert any("hits for 10" in line for line in log)

    def test_crit_doubles_damage(self):
        state = initial_state(mk(name="A"), mk(name="B"))
        rng = FakeRng([10, 1, 12, 100])  # a crits
        new, log = resolve_round(state, SAFE_MOVE, SAFE_MOVE, rng)
        assert new.b.hp == 100 - 20
        assert any("CRITICAL" in line for line in log)

    def test_stampede_can_miss(self):
        state = initial_state(mk(name="A"), mk(name="B"))
        # A roll at the configured threshold misses; b spits 8, no crit.
        rng = FakeRng([STAMPEDE_MISS_PCT, 8, 100])
        new, log = resolve_round(state, PetBrawlMove.STAMPEDE, SAFE_MOVE, rng)
        assert new.b.hp == 100
        assert any("misses" in line for line in log)

    def test_hunker_halves_spit_and_heals_without_countering(self):
        state = initial_state(mk(name="A"), mk(name="B"))
        # a spits 11 no crit; b hunkers, heal 4.
        rng = FakeRng([11, 100, 4])
        new, _ = resolve_round(state, SAFE_MOVE, PetBrawlMove.HUNKER, rng)
        # b takes ceil(11/2)=6, heals 4 -> 98.
        assert new.b.hp == 98
        assert new.a.hp == 100

    def test_stampede_cannot_miss_a_hunkered_opponent(self):
        state = initial_state(
            mk(name="A"),
            replace(mk(name="B"), hp=50),
        )
        # Seed 1's first accuracy roll normally misses. A stationary,
        # hunkered opponent must still be hit.
        new, log = resolve_round(
            state,
            PetBrawlMove.STAMPEDE,
            PetBrawlMove.HUNKER,
            random.Random(1),
        )
        assert new.b.hp < 50
        assert new.a.hp == 100
        assert not any("misses" in line for line in log)

    def test_mystic_hunker_is_also_pure_defense(self):
        state = initial_state(mk(name="A"), mk("invoker_cama", name="B"))
        rng = FakeRng([11, 100, 4])
        new, _ = resolve_round(state, SAFE_MOVE, PetBrawlMove.HUNKER, rng)
        assert new.a.hp == 100

    def test_dune_reduces_damage_with_floor_one(self):
        state = initial_state(mk(name="A"), mk("dromedary_cross", name="B"))
        rng = FakeRng([8, 100, 8, 100])
        new, _ = resolve_round(state, SAFE_MOVE, SAFE_MOVE, rng)
        assert new.b.hp == 100 - 7  # 8 - 1 hardy
        # Hardy reduction applies after hunker halving, never below 1.
        b = replace(mk("dromedary_cross", name="B"), hp=50)
        state = initial_state(mk(name="A"), b)
        rng = FakeRng([8, 100, 2])  # a spits 8; b hunkers, heals 2
        new, _ = resolve_round(state, SAFE_MOVE, PetBrawlMove.HUNKER, rng)
        assert new.b.hp == 50 - 3 + 2  # ceil(8/2)=4, -1 hardy = 3

    def test_ravenous_deals_and_takes_extra(self):
        state = initial_state(mk("pudge_cama", name="A"), mk(name="B"))
        rng = FakeRng([10, 100, 10, 100])
        new, _ = resolve_round(state, SAFE_MOVE, SAFE_MOVE, rng)
        assert new.b.hp == 100 - (10 + 1 + 1)  # +1 power(uncommon)? no: dmg+power
        # Ravenous is uncommon: damage = 10 + power 1 + ravenous 1 = 12.
        assert new.a.hp == 100 - (10 + 1)  # b's 10 + ravenous taken 1

    def test_ravenous_hunker_heals_extra(self):
        state = initial_state(mk(name="A"), mk("pudge_cama", name="B", hunger=0))
        # b starts at 80 hp (hunger 0 -> -20) and heals 2+3.
        rng = FakeRng([2, 2])
        new, _ = resolve_round(
            state,
            PetBrawlMove.HUNKER,
            PetBrawlMove.HUNKER,
            rng,
        )
        assert new.b.hp == 80 + 5

    def test_frostwool_makes_stampedes_miss_more(self):
        state = initial_state(mk(name="A"), mk("crystal_cama", name="B"))
        # Ten points above the base threshold misses only with the chill bonus.
        rng = FakeRng([STAMPEDE_MISS_PCT + 10, 8, 100])
        new, _ = resolve_round(state, PetBrawlMove.STAMPEDE, SAFE_MOVE, rng)
        assert new.b.hp == 100

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

    def test_round_cap_equal_hp_percentage_is_a_draw(self):
        a = replace(mk(name="A"), hp=60)
        b = replace(mk(name="B", adult=False), hp=48)
        state = initial_state(a, b)
        state = type(state)(
            a=state.a,
            b=state.b,
            round_no=MAX_ROUNDS - 1,
            winner=None,
        )

        new, log = resolve_round(
            state,
            SAFE_MOVE,
            SAFE_MOVE,
            FakeRng([8, 100, 10, 100, 0]),
        )

        assert (new.a.hp, new.a.max_hp) == (50, 100)
        assert (new.b.hp, new.b.max_hp) == (40, 80)
        assert new.winner == "draw"
        assert any("draw" in line.lower() for line in log)

    def test_resolve_on_finished_state_raises(self):
        state = initial_state(mk(name="A"), mk(name="B"))
        finished = type(state)(a=state.a, b=state.b, round_no=3, winner="a")
        with pytest.raises(ValueError):
            resolve_round(finished, SAFE_MOVE, SAFE_MOVE, FakeRng([]))


class TestTermination:
    def test_hunker_standoff_is_called_after_eight_rounds(self):
        rng = random.Random(0)
        state = initial_state(mk(name="A"), mk(name="B"))

        while state.winner is None:
            state, log = resolve_round(
                state,
                PetBrawlMove.HUNKER,
                PetBrawlMove.HUNKER,
                rng,
            )

        assert state.round_no == 8
        assert state.winner == "draw"
        assert any("judges" in line for line in log)

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
            assert state.winner in ("a", "b", "draw")


class TestStatisticalTuning:
    def test_repeated_hunker_cannot_defeat_an_attacking_opponent(self):
        for seed in range(100):
            rng = random.Random(seed)
            state = initial_state(mk(name="A"), mk(name="B"))
            while state.winner is None:
                state, _ = resolve_round(
                    state,
                    PetBrawlMove.HUNKER,
                    PetBrawlMove.SPIT,
                    rng,
                )
            assert state.winner == "b"

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

    def test_spit_always_lands_without_drawing_a_counter(self):
        rng = random.Random(3)
        for _ in range(200):
            state = initial_state(mk(name="A"), mk(name="B"))
            new, _ = resolve_round(state, SAFE_MOVE, PetBrawlMove.HUNKER, rng)
            assert new.b.hp <= 100  # heal capped at max
            assert new.a.hp == 100

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
