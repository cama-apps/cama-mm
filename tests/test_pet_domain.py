"""Pure-math tests for the pet domain model (no DB, no mocks)."""

from __future__ import annotations

from domain.models.pet import Pet, PetMood, PetStage
from domain.pet_constants import (
    ADULT_AGE_SECONDS,
    DEFAULT_HUNGER_DECAY_PER_DAY,
    EGG_HATCH_SECONDS,
    adoption_fee,
    food_cost,
    get_species,
)

T0 = 1_800_000_000  # arbitrary fixed epoch anchor
RATE = DEFAULT_HUNGER_DECAY_PER_DAY  # 20 points/day
DAY = 86400


def make_pet(**overrides) -> Pet:
    defaults = {
        "pet_id": 1,
        "discord_id": 111,
        "guild_id": 999,
        "name": "Testcama",
        "species": "common_cama",
        "adopted_at": T0,
        "hatched_at": T0 + EGG_HATCH_SECONDS,
        "adopt_fee": 20,
        "last_fed_at": T0 + EGG_HATCH_SECONDS,
        "hunger_at_last_fed": 100,
        "times_fed": 0,
        "feeds_today": 0,
        "feed_date": None,
        "week_consumed_jc": 0,
        "week_key": None,
        "prev_week_consumed_jc": 0,
        "prev_week_key": None,
        "pampered_until": None,
        "accessory": None,
        "dig_work_units": 0,
        "dig_work_at": T0 + EGG_HATCH_SECONDS,
        "aegis_used": 0,
        "hatch_announced_at": None,
        "died_at": None,
        "death_cause": None,
        "death_announced_at": None,
    }
    defaults.update(overrides)
    return Pet(**defaults)


class TestHungerMath:
    def test_full_pet_loses_20_points_per_day(self):
        pet = make_pet()
        hatch = pet.hatched_at
        assert pet.current_hunger(hatch, RATE) == 100
        assert pet.current_hunger(hatch + DAY, RATE) == 80
        assert pet.current_hunger(hatch + 2 * DAY + DAY // 2, RATE) == 50

    def test_hunger_floors_at_zero(self):
        pet = make_pet()
        assert pet.current_hunger(pet.hatched_at + 30 * DAY, RATE) == 0

    def test_egg_does_not_decay_before_hatch(self):
        pet = make_pet()
        assert pet.current_hunger(T0, RATE) == 100
        assert pet.current_hunger(pet.hatched_at - 1, RATE) == 100

    def test_starvation_time_is_five_days_after_last_feed(self):
        pet = make_pet()
        assert pet.starvation_time(RATE) == pet.last_fed_at + 5 * DAY

    def test_starvation_boundary_exact(self):
        pet = make_pet()
        death = pet.starvation_time(RATE)
        assert not pet.is_starved(death - 1, RATE)
        assert pet.is_starved(death, RATE)

    def test_starvation_moment_independent_of_discovery_time(self):
        pet = make_pet()
        death = pet.starvation_time(RATE)
        # Whether checked one hour or three weeks late, the computed
        # moment of death never moves.
        assert pet.is_starved(death + 3600, RATE)
        assert pet.is_starved(death + 21 * DAY, RATE)
        assert pet.starvation_time(RATE) == death

    def test_egg_cannot_starve_invariant(self):
        # Anchor is 100 @ hatched_at, so starvation always lands a full
        # decay period after hatch — an egg can never be starved.
        pet = make_pet()
        assert pet.starvation_time(RATE) > pet.hatched_at
        assert not pet.is_starved(pet.hatched_at, RATE)

    def test_dead_pet_reports_zero_hunger_and_never_starves_again(self):
        pet = make_pet(died_at=T0 + 10 * DAY)
        assert pet.current_hunger(T0 + 20 * DAY, RATE) == 0
        assert not pet.is_starved(T0 + 20 * DAY, RATE)

    def test_hunger_crossing_time_for_warning(self):
        pet = make_pet()
        # 100 -> 30 is 70 points at 20/day = 3.5 days.
        crossing = pet.hunger_crossing_time(30, RATE)
        assert crossing == pet.last_fed_at + 3 * DAY + DAY // 2
        assert pet.current_hunger(crossing, RATE) == 30


class TestQuirks:
    def test_dune_cama_decays_ten_percent_slower(self):
        pet = make_pet(species="dromedary_cross")
        # 20/day * 90% = 18/day.
        assert pet.current_hunger(pet.last_fed_at + DAY, RATE) == 82

    def test_ravenous_cama_decays_faster_but_restores_more(self):
        pet = make_pet(species="pudge_cama")
        assert pet.current_hunger(pet.last_fed_at + DAY, RATE) == 78  # 22/day
        now = pet.last_fed_at + 2 * DAY  # hunger 56
        assert pet.restored_hunger(now, RATE, 25) == 56 + 27  # 25 * 110%

    def test_restore_caps_at_max_hunger(self):
        pet = make_pet()
        now = pet.last_fed_at + DAY  # hunger 80
        assert pet.restored_hunger(now, RATE, 100) == 100

    def test_shellback_has_aegis_and_rama_spits(self):
        assert get_species("aegis_cama").has_aegis
        assert get_species("rama").spit_one_in == 20
        assert not get_species("common_cama").has_aegis

    def test_frostwool_food_discount(self):
        assert food_cost("crystal_cama", "cheese") == 27  # 30 * 90%
        assert food_cost("common_cama", "cheese") == 30

    def test_unknown_species_falls_back_to_quirkless(self):
        pet = make_pet(species="retired_species")
        assert pet.current_hunger(pet.last_fed_at + DAY, RATE) == 80


class TestStageAndAge:
    def test_stage_boundaries(self):
        pet = make_pet()
        assert pet.stage(T0) == PetStage.EGG
        assert pet.stage(pet.hatched_at - 1) == PetStage.EGG
        assert pet.stage(pet.hatched_at) == PetStage.BABY
        assert pet.stage(pet.hatched_at + ADULT_AGE_SECONDS - 1) == PetStage.BABY
        assert pet.stage(pet.hatched_at + ADULT_AGE_SECONDS) == PetStage.ADULT

    def test_dead_pet_stage_frozen_at_death(self):
        died = T0 + EGG_HATCH_SECONDS + 2 * DAY  # died a baby
        pet = make_pet(died_at=died)
        assert pet.stage(died + 365 * DAY) == PetStage.BABY

    def test_age_zero_for_egg_and_frozen_at_death(self):
        pet = make_pet()
        assert pet.age_seconds(T0 + 100) == 0
        died = pet.hatched_at + 3 * DAY
        dead = make_pet(died_at=died)
        assert dead.age_seconds(died + 50 * DAY) == 3 * DAY


class TestMood:
    def test_mood_bands(self):
        pet = make_pet()
        feed = pet.last_fed_at
        assert pet.mood(feed, RATE) == PetMood.HAPPY  # 100
        assert pet.mood(feed + 2 * DAY, RATE) == PetMood.CONTENT  # 60
        assert pet.mood(feed + 4 * DAY, RATE) == PetMood.HUNGRY  # 20
        assert pet.mood(feed + 4 * DAY + DAY // 2, RATE) == PetMood.STARVING  # 10

    def test_pampered_overrides_unless_starving(self):
        pet = make_pet(pampered_until=T0 + 30 * DAY)
        feed = pet.last_fed_at
        assert pet.mood(feed + 4 * DAY, RATE) == PetMood.HAPPY  # hungry but pampered
        # Below the hungry floor the override stops working.
        assert pet.mood(feed + 4 * DAY + DAY // 2, RATE) == PetMood.STARVING

    def test_art_mood_collapses_to_three_keys(self):
        pet = make_pet()
        feed = pet.last_fed_at
        assert pet.art_mood(feed, RATE) == "happy"
        assert pet.art_mood(feed + 2 * DAY, RATE) == "neutral"
        assert pet.art_mood(feed + 4 * DAY, RATE) == "hungry"


class TestDigWork:
    def test_accrues_across_historical_mood_bands(self):
        pet = make_pet()
        hatch = pet.hatched_at

        units = pet.dig_work_units_between(hatch, hatch + 2 * DAY, RATE)

        # Hunger 70 is still Happy: 1.55 happy days at 12 blocks/day,
        # followed by 0.45 content days at 8/day.
        assert units == (22 * DAY) + (DAY // 5)

    def test_pampering_uses_the_happy_rate_for_that_interval(self):
        pet = make_pet(
            hunger_at_last_fed=60,
            pampered_until=T0 + EGG_HATCH_SECONDS + DAY,
        )
        hatch = pet.hatched_at

        assert pet.dig_work_units_between(hatch, hatch + DAY, RATE) == 12 * DAY

    def test_eggs_and_time_after_starvation_do_not_produce(self):
        pet = make_pet()

        units = pet.dig_work_units_between(
            pet.adopted_at,
            pet.hatched_at + 10 * DAY,
            RATE,
        )

        # Inclusive 70/40/15 mood floors yield 1.55 happy days,
        # 1.5 content days, and 1.25 hungry days.
        assert units == (34 * DAY) + (7 * DAY // 20)


class TestConstantsAndRows:
    def test_adoption_fee_escalates_and_clamps(self):
        assert adoption_fee(0) == 20
        assert adoption_fee(1) == 50
        assert adoption_fee(2) == 75
        assert adoption_fee(3) == 100
        assert adoption_fee(50) == 100
        assert adoption_fee(-1) == 20

    def test_species_weights_sum_to_total(self):
        from domain.pet_constants import SPECIES, SPECIES_WEIGHT_TOTAL

        assert sum(s.weight for s in SPECIES.values()) == SPECIES_WEIGHT_TOTAL

    def test_feeds_used_on_resets_across_game_days(self):
        pet = make_pet(feeds_today=4, feed_date="2026-07-25")
        assert pet.feeds_used_on("2026-07-25") == 4
        assert pet.feeds_used_on("2026-07-26") == 0  # fresh day, cap reset

    def test_from_row_round_trip(self):
        pet = make_pet()
        row = {
            "pet_id": 1, "discord_id": 111, "guild_id": 999,
            "name": "Testcama", "species": "common_cama",
            "adopted_at": T0, "hatched_at": T0 + EGG_HATCH_SECONDS,
            "adopt_fee": 20,
            "last_fed_at": T0 + EGG_HATCH_SECONDS, "hunger_at_last_fed": 100,
            "times_fed": 0, "feeds_today": 0, "feed_date": None,
            "week_consumed_jc": 0, "week_key": None,
            "prev_week_consumed_jc": 0, "prev_week_key": None,
            "pampered_until": None, "accessory": None,
            "dig_work_units": 0, "dig_work_at": T0 + EGG_HATCH_SECONDS,
            "aegis_used": 0,
            "hatch_announced_at": None,
            "died_at": None, "death_cause": None, "death_announced_at": None,
        }
        assert Pet.from_row(row) == pet
