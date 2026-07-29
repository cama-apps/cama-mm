"""PetService tests against a real repository with a frozen, steerable clock."""

from __future__ import annotations

from unittest.mock import patch

import pytest

from domain.models.pet import PetStage
from domain.pet_constants import EGG_HATCH_SECONDS
from repositories.pet_repository import PetRepository
from repositories.player_repository import PlayerRepository
from services import error_codes
from services.pet_service import (
    PetService,
    _prev_week_key_for_timestamp,
    _week_key_for_timestamp,
)
from tests.conftest import TEST_GUILD_ID

T0 = 1_800_000_000
DAY = 86400


class SteerableClock:
    def __init__(self, now: int = T0):
        self.now = now

    def __call__(self) -> int:
        return self.now


@pytest.fixture
def clock():
    return SteerableClock()


@pytest.fixture
def service(repo_db_path, clock, monkeypatch):
    pet_repo = PetRepository(repo_db_path)
    player_repo = PlayerRepository(repo_db_path)
    svc = PetService(pet_repo, player_repo, decay_per_day=20)
    monkeypatch.setattr(svc, "_now", clock)
    player_repo.add(100, "Owner", TEST_GUILD_ID)
    player_repo.update_balance(100, TEST_GUILD_ID, 1000)
    return svc


def adopt_species(service, name, species, egg_tier="standard"):
    """Adopt an egg and pin a species for tests not concerned with its roll."""
    result = service.adopt(100, TEST_GUILD_ID, name, egg_tier)
    assert result.success, result.error
    egg = result.value["pet"]
    return service.pet_repo.resolve_hatch_species(
        egg.pet_id,
        TEST_GUILD_ID,
        species=species,
        now=egg.hatched_at,
    )


def adopt_common(service, clock, name="Blep"):
    return adopt_species(service, name, "common_cama")


def hatch(service, clock, pet):
    clock.now = pet.hatched_at
    return service.pet_repo.get_pet_by_id(pet.pet_id, TEST_GUILD_ID)


class TestAdopt:
    def test_adopt_creates_egg_and_charges_first_fee(self, service, clock):
        pet = adopt_common(service, clock)
        assert pet.adopt_fee == 20
        assert pet.hatched_at == T0 + EGG_HATCH_SECONDS
        status = service.get_status(100, TEST_GUILD_ID).value
        assert status.stage == PetStage.EGG
        assert status.hunger == 100

    def test_adopt_over_a_starved_pet_claims_death_and_escalates_fee(
        self, service, clock
    ):
        pet = adopt_common(service, clock)
        clock.now = pet.hatched_at + 9 * DAY  # long past starvation, unswept
        second = adopt_common(service, clock, name="Blep II")
        assert second.adopt_fee == 50  # the death was claimed before pricing
        dead = service.pet_repo.get_pet_by_id(pet.pet_id, TEST_GUILD_ID)
        assert dead.died_at == pet.hatched_at + 5 * DAY

    def test_adoption_fee_escalates_with_deaths(self, service, clock):
        pet = adopt_common(service, clock)
        service.pet_repo.claim_death(
            pet.pet_id, TEST_GUILD_ID, died_at=T0 + 100,
            expected_last_fed_at=pet.last_fed_at,
            expected_hunger=pet.hunger_at_last_fed,
        )
        second = adopt_common(service, clock, name="Blep II")
        assert second.adopt_fee == 50

    def test_species_roll_respects_tier_weights(self, service, clock):
        egg = service.adopt(100, TEST_GUILD_ID, "Weighted").value["pet"]
        clock.now = egg.hatched_at
        with patch("services.pet_service.random.choices") as choices:
            choices.return_value = ["rama"]
            status = service.get_status(100, TEST_GUILD_ID).value
            (species_ids,), kwargs = choices.call_args
            weights = kwargs["weights"]
        assert status.pet.species == "rama"
        by_id = dict(zip(species_ids, weights, strict=True))
        assert by_id["common_cama"] == 1500
        assert by_id["embergear_cama"] == 1500
        assert by_id["riverglow_cama"] == 750
        assert by_id["prismwool_cama"] == 225
        assert by_id["rama"] == 100
        assert sum(weights) == 10_000

    def test_unregistered_player_rejected(self, service, clock):
        result = service.adopt(999, TEST_GUILD_ID, "Ghost")
        assert result.error_code == error_codes.PLAYER_NOT_FOUND

    def test_name_validation(self, service, clock):
        assert (
            service.adopt(100, TEST_GUILD_ID, "").error_code
            == error_codes.VALIDATION_ERROR
        )
        assert (
            service.adopt(100, TEST_GUILD_ID, "x" * 33).error_code
            == error_codes.VALIDATION_ERROR
        )
        assert (
            service.adopt(100, TEST_GUILD_ID, "hi @everyone").error_code
            == error_codes.VALIDATION_ERROR
        )
        assert (
            service.adopt(100, TEST_GUILD_ID, "https://spam.example").error_code
            == error_codes.VALIDATION_ERROR
        )


class TestEggHatchAndUpgrade:
    def test_standard_species_is_selected_at_hatch_not_purchase(
        self, service, clock
    ):
        adopted = service.adopt(100, TEST_GUILD_ID, "Later")
        assert adopted.success, adopted.error
        egg = adopted.value["pet"]
        assert egg.species == "unhatched"
        assert egg.egg_tier == "standard"

        clock.now = egg.hatched_at
        with patch(
            "services.pet_service.random.choices",
            return_value=["rama"],
        ):
            status = service.get_status(100, TEST_GUILD_ID).value
        assert status.pet.species == "rama"

    def test_second_gilded_adoption_upgrades_and_renames_existing_egg(
        self, service, clock
    ):
        egg = service.adopt(100, TEST_GUILD_ID, "Blep").value["pet"]
        result = service.adopt(100, TEST_GUILD_ID, "Goldie", "gilded")
        assert result.success, result.error
        assert result.value["upgraded"]
        upgraded = result.value["pet"]
        assert upgraded.pet_id == egg.pet_id
        assert upgraded.name == "Goldie"
        assert upgraded.hatched_at == egg.hatched_at
        assert upgraded.species == "unhatched"
        assert upgraded.egg_tier == "gilded"
        assert upgraded.adopt_fee == 250
        assert service.player_repo.get_balance(100, TEST_GUILD_ID) == 750

    def test_second_gilded_adoption_rejects_repeat_hatched_and_unaffordable_requests(
        self, service, clock
    ):
        egg = service.adopt(100, TEST_GUILD_ID, "Blep").value["pet"]
        assert service.adopt(100, TEST_GUILD_ID, "Goldie", "gilded").success
        assert (
            service.adopt(100, TEST_GUILD_ID, "Again", "gilded").error_code
            == error_codes.ALREADY_HAS_PET
        )

        clock.now = egg.hatched_at
        with patch.object(service, "_roll_species_tiered", return_value="rama"):
            assert (
                service.adopt(100, TEST_GUILD_ID, "Too Late", "gilded").error_code
                == error_codes.ALREADY_HAS_PET
            )

        service.pet_repo.claim_death(
            egg.pet_id,
            TEST_GUILD_ID,
            died_at=clock.now,
            expected_last_fed_at=egg.last_fed_at,
            expected_hunger=egg.hunger_at_last_fed,
        )
        clock.now += 1
        service.player_repo.update_balance(100, TEST_GUILD_ID, 100)
        assert service.adopt(100, TEST_GUILD_ID, "Second").success
        failed = service.adopt(100, TEST_GUILD_ID, "Pricier", "gilded")
        assert failed.error_code == error_codes.INSUFFICIENT_FUNDS
        unchanged = service.pet_repo.get_active_pet(100, TEST_GUILD_ID)
        assert unchanged.egg_tier == "standard"
        assert unchanged.name == "Second"

    def test_second_standard_adoption_remains_rejected(
        self, service, clock
    ):
        assert service.adopt(100, TEST_GUILD_ID, "Blep").success
        result = service.adopt(100, TEST_GUILD_ID, "Replacement", "standard")
        assert result.error_code == error_codes.ALREADY_HAS_PET

    def test_hatch_resolution_is_stable_across_sweep_retries(
        self, service, clock
    ):
        egg = service.adopt(100, TEST_GUILD_ID, "Stable").value["pet"]
        clock.now = egg.hatched_at
        with patch("services.pet_service.random.choices", return_value=["rama"]):
            first = service.sweep()["hatches"][0].pet
        with patch(
            "services.pet_service.random.choices",
            return_value=["common_cama"],
        ):
            retried = service.sweep()["hatches"][0].pet
        assert first.species == "rama"
        assert retried.species == "rama"

    def test_warning_crossing_is_deferred_until_species_resolves(
        self, service, clock
    ):
        egg = service.adopt(100, TEST_GUILD_ID, "Later").value["pet"]
        assert service.get_warning_crossing(100, TEST_GUILD_ID) is None
        clock.now = egg.hatched_at
        with patch(
            "services.pet_service.random.choices",
            return_value=["dromedary_cross"],
        ):
            crossing = service.get_warning_crossing(100, TEST_GUILD_ID)
        assert crossing == (
            egg.hatched_at + 70 * DAY * 100 // (20 * 90),
            "Later",
        )


class TestFeeding:
    def test_feed_flow(self, service, clock):
        pet = adopt_common(service, clock)
        service.buy(100, TEST_GUILD_ID, "tango", 5)
        clock.now = pet.hatched_at + 2 * DAY  # hunger 60
        result = service.feed(100, TEST_GUILD_ID, "tango")
        assert result.success
        outcome = result.value
        assert outcome.old_hunger == 60
        assert outcome.new_hunger == 85
        assert outcome.remaining_qty == 4
        assert outcome.feeds_left_today == 3

    def test_feed_egg_rejected(self, service, clock):
        adopt_common(service, clock)
        service.buy(100, TEST_GUILD_ID, "tango", 1)
        result = service.feed(100, TEST_GUILD_ID, "tango")
        assert result.error_code == error_codes.PET_EGG

    def test_feed_full_pet_rejected(self, service, clock):
        pet = adopt_common(service, clock)
        service.buy(100, TEST_GUILD_ID, "tango", 1)
        clock.now = pet.hatched_at  # hunger exactly 100
        result = service.feed(100, TEST_GUILD_ID, "tango")
        assert result.error_code == error_codes.PET_FULL

    def test_feed_without_supplies(self, service, clock):
        pet = adopt_common(service, clock)
        clock.now = pet.hatched_at + DAY
        result = service.feed(100, TEST_GUILD_ID, "tango")
        assert result.error_code == error_codes.NO_SUPPLIES

    def test_starved_pet_feeds_as_dead(self, service, clock):
        pet = adopt_common(service, clock)
        service.buy(100, TEST_GUILD_ID, "cheese", 2)
        clock.now = pet.hatched_at + 6 * DAY  # starved at +5 days
        result = service.feed(100, TEST_GUILD_ID, "cheese")
        assert result.error_code == error_codes.NO_PET
        dead = service.pet_repo.get_pet_by_id(pet.pet_id, TEST_GUILD_ID)
        # Death stamped at the computed starvation moment, not discovery.
        assert dead.died_at == pet.hatched_at + 5 * DAY

    def test_rama_spits_at_seeded_roll(self, service, clock):
        pet = adopt_species(service, "Rama", "rama")
        service.buy(100, TEST_GUILD_ID, "tango", 2)
        clock.now = pet.hatched_at + 2 * DAY
        with patch("services.pet_service.random.randint", return_value=1):
            result = service.feed(100, TEST_GUILD_ID, "tango")
        assert result.success
        assert result.value.spat
        assert result.value.new_hunger == result.value.old_hunger
        assert result.value.remaining_qty == 1  # item still consumed
        with patch("services.pet_service.random.randint", return_value=2):
            result = service.feed(100, TEST_GUILD_ID, "tango")
        assert result.success
        assert not result.value.spat
        assert result.value.new_hunger > result.value.old_hunger


class TestPassiveDigWork:
    def test_status_previews_historical_work_without_persisting_it(
        self, service, clock
    ):
        pet = adopt_common(service, clock)
        clock.now = pet.hatched_at + 2 * DAY

        status = service.get_status(100, TEST_GUILD_ID).value
        stored = service.pet_repo.get_pet_by_id(pet.pet_id, TEST_GUILD_ID)

        assert status.dig_work_units == (22 * DAY) + (DAY // 5)
        assert status.dig_work_rate == 8
        assert stored.dig_work_units == 0
        assert stored.dig_work_at == pet.hatched_at

    def test_feed_settles_old_moods_before_moving_the_hunger_anchor(
        self, service, clock
    ):
        pet = adopt_common(service, clock)
        service.buy(100, TEST_GUILD_ID, "cheese", 1)
        clock.now = pet.hatched_at + 2 * DAY

        assert service.feed(100, TEST_GUILD_ID, "cheese").success

        settled = service.pet_repo.get_pet_by_id(pet.pet_id, TEST_GUILD_ID)
        assert settled.dig_work_units == (22 * DAY) + (DAY // 5)
        assert settled.dig_work_at == clock.now
        clock.now += DAY
        status = service.get_status(100, TEST_GUILD_ID).value
        assert status.dig_work_units == (34 * DAY) + (DAY // 5)

    def test_pamper_settles_before_the_happy_override_starts(
        self, service, clock
    ):
        pet = adopt_common(service, clock)
        clock.now = pet.hatched_at + 2 * DAY

        assert service.buy(100, TEST_GUILD_ID, "salt_lick", 1).success

        clock.now += DAY // 2
        status = service.get_status(100, TEST_GUILD_ID).value
        assert status.dig_work_units == (28 * DAY) + (DAY // 5)

    def test_match_top_up_settles_before_moving_the_hunger_anchor(
        self, service, clock
    ):
        pet = adopt_common(service, clock)
        clock.now = pet.hatched_at + 2 * DAY

        service.on_match_win([100], TEST_GUILD_ID)

        settled = service.pet_repo.get_pet_by_id(pet.pet_id, TEST_GUILD_ID)
        assert settled.dig_work_units == (22 * DAY) + (DAY // 5)
        assert settled.dig_work_at == clock.now

    def test_starvation_settles_only_the_productive_lifetime(
        self, service, clock
    ):
        pet = adopt_common(service, clock)
        clock.now = pet.hatched_at + 9 * DAY

        assert service.get_status(100, TEST_GUILD_ID).value.pet is None

        dead = service.pet_repo.get_pet_by_id(pet.pet_id, TEST_GUILD_ID)
        assert dead.dig_work_units == (34 * DAY) + (7 * DAY // 20)
        assert dead.dig_work_at == pet.hatched_at + 5 * DAY

    def test_aegis_revive_preserves_work_earned_before_starvation(
        self, service, clock
    ):
        pet = adopt_species(service, "Shelly", "aegis_cama")
        starved_at = pet.starvation_time(service.decay_per_day)
        clock.now = starved_at + 60

        status = service.get_status(100, TEST_GUILD_ID).value

        assert status.pet is not None
        assert status.pet.dig_work_units == (34 * DAY) + (7 * DAY // 20)
        assert status.pet.dig_work_at == starved_at


class TestQuirks:
    def test_aegis_revive_fires_once_then_death(self, service, clock):
        pet = adopt_species(service, "Shelly", "aegis_cama")
        starve_1 = pet.hatched_at + 5 * DAY
        # 10 hunger at 20/day = 12h more life after the revive.
        starve_2 = starve_1 + DAY // 2
        clock.now = starve_1 + 3600  # discovered an hour after first starvation
        status = service.get_status(100, TEST_GUILD_ID).value
        assert status.pet is not None  # revived, not dead
        assert status.pet.aegis_used == 1
        clock.now = starve_2 + 3600
        status = service.get_status(100, TEST_GUILD_ID).value
        assert status.pet is None
        assert status.last_dead.died_at == starve_2

    def test_frostwool_discount_applies_to_buy(self, service, clock):
        pet = adopt_species(service, "Frosty", "crystal_cama")
        clock.now = pet.hatched_at
        result = service.buy(100, TEST_GUILD_ID, "cheese", 2)
        assert result.success
        assert result.value["total_cost"] == 54  # 27 each, not 30

    def test_unhatched_frostwool_uses_species_neutral_prices(self, service, clock):
        pet = service.adopt(100, TEST_GUILD_ID, "Secret").value["pet"]
        assert clock.now < pet.hatched_at
        assert pet.species == "unhatched"
        result = service.buy(100, TEST_GUILD_ID, "cheese", 2)
        assert result.success
        assert result.value["total_cost"] == 60

    def test_invoker_salt_lick_lasts_longer(self, service, clock):
        pet = adopt_species(service, "Orbs", "invoker_cama")
        clock.now = pet.hatched_at
        result = service.buy(100, TEST_GUILD_ID, "salt_lick", 1)
        assert result.success
        assert result.value["pampered_until"] == clock.now + 18 * 3600

    def test_gilded_refund_bonus(self, service, clock):
        pet = adopt_species(service, "Goldie", "jopacama")
        service.buy(100, TEST_GUILD_ID, "cheese", 20)
        clock.now = pet.hatched_at + 2 * DAY
        service.feed(100, TEST_GUILD_ID, "cheese")
        _seed_fund(service, 500)
        week_of_feed = _week_key_for_timestamp(clock.now)
        # Advance day by day (feeding to stay alive) until the fed week is
        # the previous week, then sweep.
        while _prev_week_key_for_timestamp(clock.now) != week_of_feed:
            clock.now += DAY
            service.feed(100, TEST_GUILD_ID, "cheese")
        with patch("services.pet_service.random.randint", return_value=100):
            notices = service.sweep()["refunds"]
        payout = notices[0].payouts[0]
        assert payout.multiplier_pct == 110  # 100 roll + 10pp Gilded bonus


def _seed_fund(service, amount, guild_id=TEST_GUILD_ID):
    with service.pet_repo.connection() as conn:
        conn.execute(
            "INSERT INTO nonprofit_fund (guild_id, total_collected) VALUES (?, ?) "
            "ON CONFLICT(guild_id) DO UPDATE SET total_collected = excluded.total_collected",
            (guild_id, amount),
        )


class TestGacha:
    def test_gilded_egg_charges_premium_and_skips_commons(self, service, clock):
        service.player_repo.update_balance(100, TEST_GUILD_ID, 1000)
        result = service.adopt(100, TEST_GUILD_ID, "Fancy", "gilded")
        assert result.success, result.error
        pet = result.value["pet"]
        assert result.value["egg_tier"] == "gilded"
        assert pet.adopt_fee == 20 + 230  # base fee + premium
        assert pet.species == "unhatched"
        clock.now = pet.hatched_at
        with patch.object(service, "_roll_species_tiered", return_value="rama"):
            pet = service.get_status(100, TEST_GUILD_ID).value.pet
        from domain.pet_constants import get_species

        assert get_species(pet.species).tier != "common"

    def test_gilded_odds_over_many_rolls_never_common(self, service, clock):
        from domain.pet_constants import GILDED_TIER_WEIGHTS, get_species

        tiers = {
            get_species(service._roll_species_tiered(GILDED_TIER_WEIGHTS)).tier
            for _ in range(200)
        }
        assert "common" not in tiers
        assert "uncommon" in tiers

    def test_pity_activates_after_three_common_deaths(self, service, clock):
        service.player_repo.update_balance(100, TEST_GUILD_ID, 5000)
        # Three commons adopted and starved out in sequence.
        for i in range(3):
            pet = adopt_common(service, clock, name=f"Common {i}")
            clock.now = pet.hatched_at + 9 * DAY  # starves; resolved lazily
            assert service.living_pet(100, TEST_GUILD_ID, clock.now) is None
        assert service._pity_active(100, TEST_GUILD_ID)
        result = service.adopt(100, TEST_GUILD_ID, "Pity Roll")
        assert result.success
        assert result.value["pity_active"]
        clock.now = result.value["pet"].hatched_at
        with patch.object(service, "_roll_species_tiered", return_value="jopacama"):
            pet = service.get_status(100, TEST_GUILD_ID).value.pet
        from domain.pet_constants import get_species

        assert get_species(pet.species).tier != "common"

    def test_pity_broken_by_a_non_common(self, service, clock):
        service.player_repo.update_balance(100, TEST_GUILD_ID, 5000)
        pet = adopt_common(service, clock, name="C1")
        clock.now = pet.hatched_at + 9 * DAY
        service.living_pet(100, TEST_GUILD_ID, clock.now)
        pet = adopt_species(service, "Fancy", "jopacama")
        clock.now = pet.hatched_at + 9 * DAY
        service.living_pet(100, TEST_GUILD_ID, clock.now)
        pet = adopt_common(service, clock, name="C2")
        clock.now = pet.hatched_at + 9 * DAY
        service.living_pet(100, TEST_GUILD_ID, clock.now)
        assert not service._pity_active(100, TEST_GUILD_ID)

    def test_trinket_roll_equips_and_dupes_refund(self, service, clock):
        pet = adopt_common(service, clock)
        clock.now = pet.hatched_at + DAY
        with patch("services.pet_service.random.choices", return_value=["top_hat"]):
            first = service.roll_trinket(100, TEST_GUILD_ID)
        assert first.success, first.error
        assert not first.value.duplicate
        assert first.value.net_cost == 25
        assert first.value.owned_count == 1
        fresh = service.pet_repo.get_active_pet(100, TEST_GUILD_ID)
        assert fresh.accessory == "top_hat"
        with patch("services.pet_service.random.choices", return_value=["top_hat"]):
            dupe = service.roll_trinket(100, TEST_GUILD_ID)
        assert dupe.success
        assert dupe.value.duplicate
        assert dupe.value.net_cost == 15  # 25 - 10 refund
        assert dupe.value.owned_count == 1

    def test_trinket_requires_hatched_living_pet(self, service, clock):
        result = service.roll_trinket(100, TEST_GUILD_ID)
        assert result.error_code == error_codes.NO_PET
        adopt_common(service, clock)
        result = service.roll_trinket(100, TEST_GUILD_ID)  # still an egg
        assert result.error_code == error_codes.PET_EGG

    def test_wear_trinket_owned_only(self, service, clock):
        pet = adopt_common(service, clock)
        clock.now = pet.hatched_at + DAY
        assert (
            service.wear_trinket(100, TEST_GUILD_ID, "red_bow").error_code
            == error_codes.VALIDATION_ERROR
        )
        with patch("services.pet_service.random.choices", return_value=["red_bow"]):
            service.roll_trinket(100, TEST_GUILD_ID)
        with patch("services.pet_service.random.choices", return_value=["top_hat"]):
            service.roll_trinket(100, TEST_GUILD_ID)  # equips top_hat
        result = service.wear_trinket(100, TEST_GUILD_ID, "red_bow")
        assert result.success
        assert service.pet_repo.get_active_pet(100, TEST_GUILD_ID).accessory == "red_bow"

    def test_camadex_counts_hatched_species_only(self, service, clock):
        assert service.camadex(100, TEST_GUILD_ID) == ([], 13)
        pet = adopt_common(service, clock)
        # Still an egg: species not yet revealed.
        assert service.camadex(100, TEST_GUILD_ID)[0] == []
        clock.now = pet.hatched_at + DAY
        raised, total = service.camadex(100, TEST_GUILD_ID)
        assert raised == ["common_cama"]
        assert total == 13


class TestMatchHook:
    def test_win_feeds_living_pets_and_skips_eggs(self, service, clock):
        pet = adopt_common(service, clock)
        clock.now = pet.hatched_at + 2 * DAY  # hunger 60
        fed = service.on_match_win([100, 555], TEST_GUILD_ID)
        assert len(fed) == 1
        fed_pet, amount = fed[0]
        assert fed_pet.pet_id == pet.pet_id
        assert amount == 10
        status = service.get_status(100, TEST_GUILD_ID).value
        assert status.hunger == 70

    def test_win_caps_at_max(self, service, clock):
        pet = adopt_common(service, clock)
        clock.now = pet.hatched_at  # full
        service.on_match_win([100], TEST_GUILD_ID)
        assert service.get_status(100, TEST_GUILD_ID).value.hunger == 100

    def test_win_returns_post_top_up_anchors(self, service, clock):
        pet = adopt_common(service, clock)
        clock.now = pet.hatched_at + 2 * DAY  # hunger 60
        fed = service.on_match_win([100], TEST_GUILD_ID)
        fed_pet, _amount = fed[0]
        assert fed_pet.last_fed_at == clock.now
        assert fed_pet.hunger_at_last_fed == 70

    def test_pack_cama_gets_bonus(self, service, clock):
        pet = adopt_species(service, "Donkey", "courier_cama")
        clock.now = pet.hatched_at + 2 * DAY
        fed = service.on_match_win([100], TEST_GUILD_ID)
        assert fed[0][1] == 15

    def test_hook_never_raises(self, service, clock, monkeypatch):
        monkeypatch.setattr(
            service.pet_repo, "get_active_pets_for",
            lambda *a, **k: (_ for _ in ()).throw(RuntimeError("db down")),
        )
        assert service.on_match_win([100], TEST_GUILD_ID) == []


class TestSweep:
    def test_sweep_hatches_once(self, service, clock):
        pet = adopt_common(service, clock)
        clock.now = pet.hatched_at + 60
        result = service.sweep()
        assert [n.pet.pet_id for n in result["hatches"]] == [pet.pet_id]
        service.mark_hatch_announced(result["hatches"][0].pet)
        assert service.sweep()["hatches"] == []

    def test_sweep_claims_death_at_starvation_moment(self, service, clock):
        pet = adopt_common(service, clock)
        clock.now = pet.hatched_at + 9 * DAY
        result = service.sweep()
        assert len(result["deaths"]) == 1
        assert result["deaths"][0].pet.died_at == pet.hatched_at + 5 * DAY
        # Unmarked deaths are re-offered next tick (retry semantics).
        assert len(service.sweep()["deaths"]) == 1
        service.mark_death_announced(result["deaths"][0].pet)
        assert service.sweep()["deaths"] == []

    def test_refund_requires_alive_at_payout(self, service, clock):
        pet = adopt_common(service, clock)
        service.buy(100, TEST_GUILD_ID, "tango", 4)
        clock.now = pet.hatched_at + DAY
        service.feed(100, TEST_GUILD_ID, "tango")
        _seed_fund(service, 500)
        balance_before = _balance(service, 100)
        # Starve it out, then cross into the next week: no refund for the dead.
        clock.now = pet.hatched_at + 12 * DAY
        result = service.sweep()
        assert result["refunds"] == []
        assert _balance(service, 100) == balance_before

    def test_refund_pays_and_claims_window_once(self, service, clock):
        pet = adopt_common(service, clock)
        service.buy(100, TEST_GUILD_ID, "cheese", 3)  # 48 JC
        clock.now = pet.hatched_at + 2 * DAY
        service.feed(100, TEST_GUILD_ID, "cheese")  # consumed 16
        _seed_fund(service, 500)
        week_of_feed = _week_key_for_timestamp(clock.now)
        # Advance until the fed week is the previous week, keeping pet alive.
        while _prev_week_key_for_timestamp(clock.now) != week_of_feed:
            clock.now += DAY
            service.feed(100, TEST_GUILD_ID, "cheese")
        balance_before = _balance(service, 100)
        with patch("services.pet_service.random.randint", return_value=200):
            result = service.sweep()
        assert len(result["refunds"]) == 1
        notice = result["refunds"][0]
        assert notice.total_paid == 60  # 30 consumed * 200%
        assert not notice.scaled_down
        assert _balance(service, 100) == balance_before + 60
        # The payment is claimed once, but its summary is re-offered until
        # Discord delivery is acknowledged.
        with patch("services.pet_service.random.randint", return_value=200):
            retry = service.sweep()["refunds"]
        assert retry == [notice]
        assert _balance(service, 100) == balance_before + 60
        service.mark_refund_announced(notice)
        assert service.sweep()["refunds"] == []

    def test_stale_snapshot_never_kills_a_fed_pet(self, service, clock):
        """Regression for the critical sweep-vs-feed race: resolving from a
        stale snapshot must re-read and keep the (fed) pet alive."""
        pet = adopt_common(service, clock)
        service.buy(100, TEST_GUILD_ID, "cheese", 2)
        clock.now = pet.hatched_at + 4 * DAY
        assert service.feed(100, TEST_GUILD_ID, "cheese").success
        # Simulate a sweep holding the PRE-feed snapshot past starvation time.
        clock.now = pet.hatched_at + 6 * DAY
        resolved = service.resolve_starvation(pet, clock.now)
        assert resolved is not None
        assert resolved.died_at is None

    def test_empty_fund_leaves_week_unclaimed_until_topped_up(self, service, clock):
        pet = adopt_common(service, clock)
        service.buy(100, TEST_GUILD_ID, "cheese", 20)
        clock.now = pet.hatched_at + 2 * DAY
        service.feed(100, TEST_GUILD_ID, "cheese")
        week_of_feed = _week_key_for_timestamp(clock.now)
        while _prev_week_key_for_timestamp(clock.now) != week_of_feed:
            clock.now += DAY
            service.feed(100, TEST_GUILD_ID, "cheese")
        _seed_fund(service, 0)
        with patch("services.pet_service.random.randint", return_value=200):
            assert service.sweep()["refunds"] == []
        assert not service.pet_repo.is_refund_window_paid(
            TEST_GUILD_ID, week_of_feed
        )
        _seed_fund(service, 500)
        service.feed(100, TEST_GUILD_ID, "cheese")  # keep alive for the tick
        with patch("services.pet_service.random.randint", return_value=200):
            notices = service.sweep()["refunds"]
        assert notices and notices[0].total_paid == 60

    def test_outage_spanning_week_still_pays_both_weeks_via_lookback(
        self, service, clock
    ):
        """Care in weeks W and W+1, sweeps down until W+2: the 2-week
        lookback pays BOTH windows instead of silently forfeiting W."""
        pet = adopt_common(service, clock)
        service.buy(100, TEST_GUILD_ID, "cheese", 25)
        clock.now = pet.hatched_at + 2 * DAY
        service.feed(100, TEST_GUILD_ID, "cheese")
        week_w = _week_key_for_timestamp(clock.now)
        _seed_fund(service, 500)
        # Feed daily until the LAST day of week W+1 (no sweeps run — outage).
        while _week_key_for_timestamp(clock.now) == week_w:
            clock.now += DAY
            service.feed(100, TEST_GUILD_ID, "cheese")
        week_w1 = _week_key_for_timestamp(clock.now)
        while _week_key_for_timestamp(clock.now + DAY) == week_w1:
            clock.now += DAY
            service.feed(100, TEST_GUILD_ID, "cheese")
        # Bot returns on the first day of W+2; pet alive (fed yesterday).
        clock.now += DAY
        with patch("services.pet_service.random.randint", return_value=100):
            notices = service.sweep()["refunds"]
        paid_weeks = {n.week_key for n in notices}
        assert week_w in paid_weeks  # would forfeit without the lookback
        assert week_w1 in paid_weeks

    def test_refund_scales_down_to_fund(self, service, clock):
        pet = adopt_common(service, clock)
        service.buy(100, TEST_GUILD_ID, "cheese", 3)
        clock.now = pet.hatched_at + 2 * DAY
        service.feed(100, TEST_GUILD_ID, "cheese")
        _seed_fund(service, 5)  # can't cover a 32 JC payout
        week_of_feed = _week_key_for_timestamp(clock.now)
        while _prev_week_key_for_timestamp(clock.now) != week_of_feed:
            clock.now += DAY
            service.feed(100, TEST_GUILD_ID, "cheese")
        with patch("services.pet_service.random.randint", return_value=200):
            result = service.sweep()
        notice = result["refunds"][0]
        assert notice.scaled_down
        assert notice.total_paid == 5  # pro-rata down to the whole fund
        assert service.pet_repo.get_nonprofit_balance(TEST_GUILD_ID) == 0


def _balance(service, discord_id):
    return PlayerRepository(service.pet_repo.db_path).get_balance(
        discord_id, TEST_GUILD_ID
    )
