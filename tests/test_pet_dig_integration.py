"""Passive pet-work integration with the normal Dig commit path."""

from __future__ import annotations

import random
from unittest.mock import patch

import pytest

from repositories.dig_repository import DigRepository, TunnelStateConflictError
from repositories.pet_repository import PetRepository
from repositories.player_repository import PlayerRepository
from services.dig_service import DigService
from services.pet_service import PetService
from tests.conftest import TEST_GUILD_ID

T0 = 1_800_000_000
DAY = 86400
USER_ID = 10001


class Clock:
    def __init__(self):
        self.now = T0


@pytest.fixture
def pet_dig(repo_db_path, monkeypatch):
    clock = Clock()
    player_repo = PlayerRepository(repo_db_path)
    pet_repo = PetRepository(repo_db_path)
    dig_repo = DigRepository(repo_db_path)
    player_repo.add(USER_ID, "Miner", TEST_GUILD_ID)
    player_repo.update_balance(USER_ID, TEST_GUILD_ID, 1000)
    pet_service = PetService(pet_repo, player_repo, decay_per_day=20)
    monkeypatch.setattr(pet_service, "_now", lambda: clock.now)
    dig_service = DigService(dig_repo, player_repo)
    dig_service.pet_service = pet_service
    monkeypatch.setattr(
        dig_service, "_get_weather_effects", lambda guild_id, layer_name: {}
    )
    monkeypatch.setattr("services.dig_service.time.time", lambda: clock.now)
    egg = pet_service.adopt(USER_ID, TEST_GUILD_ID, "Blep").value["pet"]
    pet = pet_repo.resolve_hatch_species(
        egg.pet_id,
        TEST_GUILD_ID,
        species="common_cama",
        now=egg.hatched_at,
    )
    return clock, pet_service, dig_service, pet_repo, dig_repo, pet


def _seed_started_tunnel(dig_repo, *, depth: int) -> None:
    dig_repo.create_tunnel(USER_ID, TEST_GUILD_ID, "Pet Mine")
    dig_repo.update_tunnel(
        USER_ID,
        TEST_GUILD_ID,
        depth=depth,
        max_depth=depth,
        total_digs=1,
        last_dig_at=0,
    )


def _seed_work(pet_repo, pet, *, blocks: int, at: int) -> None:
    with pet_repo.connection() as conn:
        conn.execute(
            "UPDATE pets SET dig_work_units = ?, dig_work_at = ? WHERE pet_id = ?",
            (blocks * DAY, at, pet.pet_id),
        )


def _make_normal_dig_deterministic(monkeypatch) -> None:
    monkeypatch.setattr(random, "randint", lambda low, high: low)
    monkeypatch.setattr(random, "random", lambda: 0.99)


def test_first_dig_spends_at_most_twelve_pet_blocks(pet_dig):
    clock, _pet_service, dig_service, pet_repo, _dig_repo, pet = pet_dig
    clock.now = pet.hatched_at + 2 * DAY

    with patch("services.dig.dig_core_mixin.random.randint", side_effect=[5, 1]):
        result = dig_service.dig(USER_ID, TEST_GUILD_ID)

    fresh = pet_repo.get_pet_by_id(pet.pet_id, TEST_GUILD_ID)
    assert result["advance"] == 17
    assert result["pet_dig_bonus"] == 12
    assert result["pet_name"] == "Blep"
    assert fresh.dig_work_units == (10 * DAY) + (DAY // 5)
    assert fresh.dig_work_at == clock.now


def test_pet_bonus_stops_at_boss_boundary_and_retains_the_rest(
    pet_dig, monkeypatch
):
    clock, _pet_service, dig_service, pet_repo, dig_repo, pet = pet_dig
    clock.now = pet.hatched_at
    _seed_started_tunnel(dig_repo, depth=20)
    _seed_work(pet_repo, pet, blocks=12, at=clock.now)
    _make_normal_dig_deterministic(monkeypatch)

    result = dig_service.dig(USER_ID, TEST_GUILD_ID)

    fresh = pet_repo.get_pet_by_id(pet.pet_id, TEST_GUILD_ID)
    assert result["depth_after"] == 24
    assert result["pet_dig_bonus"] == 3
    assert result["boss_encounter"]
    assert fresh.dig_work_units == 9 * DAY


def test_cave_in_does_not_consume_pet_work(pet_dig, monkeypatch):
    clock, _pet_service, dig_service, pet_repo, dig_repo, pet = pet_dig
    clock.now = pet.hatched_at
    _seed_started_tunnel(dig_repo, depth=10)
    _seed_work(pet_repo, pet, blocks=12, at=clock.now)
    monkeypatch.setattr(random, "random", lambda: 0.0)

    result = dig_service.dig(USER_ID, TEST_GUILD_ID)

    fresh = pet_repo.get_pet_by_id(pet.pet_id, TEST_GUILD_ID)
    assert result["cave_in"]
    assert result["pet_dig_bonus"] == 0
    assert fresh.dig_work_units == 12 * DAY


def test_paid_normal_dig_uses_pet_work(pet_dig, monkeypatch):
    clock, _pet_service, dig_service, pet_repo, dig_repo, pet = pet_dig
    clock.now = pet.hatched_at
    _seed_started_tunnel(dig_repo, depth=10)
    dig_repo.update_tunnel(
        USER_ID,
        TEST_GUILD_ID,
        last_dig_at=clock.now,
    )
    _seed_work(pet_repo, pet, blocks=12, at=clock.now)
    _make_normal_dig_deterministic(monkeypatch)

    result = dig_service.dig(USER_ID, TEST_GUILD_ID, paid=True)

    assert result["success"]
    assert result["pet_dig_bonus"] == 12
    assert pet_repo.get_pet_by_id(pet.pet_id, TEST_GUILD_ID).dig_work_units == 0


def test_stale_pet_work_claim_rolls_back_tunnel_advance(pet_dig):
    clock, _pet_service, _dig_service, pet_repo, dig_repo, pet = pet_dig
    clock.now = pet.hatched_at
    _seed_started_tunnel(dig_repo, depth=10)
    _seed_work(pet_repo, pet, blocks=12, at=clock.now)

    with pytest.raises(RuntimeError, match="pet work changed"):
        dig_repo.atomic_tunnel_balance_update(
            USER_ID,
            TEST_GUILD_ID,
            tunnel_updates={"depth": 20},
            pet_work_claim={
                "pet_id": pet.pet_id,
                "expected_units": 13 * DAY,
                "expected_at": clock.now,
                "new_units": 0,
                "new_at": clock.now,
            },
        )

    assert dig_repo.get_tunnel(USER_ID, TEST_GUILD_ID)["depth"] == 10
    assert pet_repo.get_pet_by_id(pet.pet_id, TEST_GUILD_ID).dig_work_units == 12 * DAY


def test_deterministic_dig_applies_pet_work(pet_dig, monkeypatch):
    clock, _pet_service, dig_service, pet_repo, dig_repo, pet = pet_dig
    clock.now = pet.hatched_at
    _seed_started_tunnel(dig_repo, depth=10)
    _seed_work(pet_repo, pet, blocks=12, at=clock.now)
    _make_normal_dig_deterministic(monkeypatch)

    terminal, preconditions = dig_service.dig_with_preconditions(
        USER_ID,
        TEST_GUILD_ID,
    )
    assert terminal is None
    preconditions["cave_in_chance"] = 0

    result = dig_service._execute_deterministic_outcome(preconditions)

    assert result["pet_dig_bonus"] == 12
    assert result["pet_name"] == "Blep"
    assert pet_repo.get_pet_by_id(
        pet.pet_id,
        TEST_GUILD_ID,
    ).dig_work_units == 0


def test_deterministic_cave_in_does_not_consume_pet_work(
    pet_dig, monkeypatch
):
    clock, _pet_service, dig_service, pet_repo, dig_repo, pet = pet_dig
    clock.now = pet.hatched_at
    _seed_started_tunnel(dig_repo, depth=10)
    _seed_work(pet_repo, pet, blocks=12, at=clock.now)
    _make_normal_dig_deterministic(monkeypatch)

    terminal, preconditions = dig_service.dig_with_preconditions(
        USER_ID,
        TEST_GUILD_ID,
    )
    assert terminal is None
    preconditions["cave_in_chance"] = 1

    result = dig_service._execute_deterministic_outcome(preconditions)

    assert result["cave_in"]
    assert result["pet_dig_bonus"] == 0
    assert pet_repo.get_pet_by_id(
        pet.pet_id,
        TEST_GUILD_ID,
    ).dig_work_units == 12 * DAY


def test_applied_outcome_stops_pet_work_at_boss_boundary(pet_dig, monkeypatch):
    clock, _pet_service, dig_service, pet_repo, dig_repo, pet = pet_dig
    clock.now = pet.hatched_at
    _seed_started_tunnel(dig_repo, depth=20)
    _seed_work(pet_repo, pet, blocks=12, at=clock.now)
    _make_normal_dig_deterministic(monkeypatch)

    terminal, preconditions = dig_service.dig_with_preconditions(
        USER_ID,
        TEST_GUILD_ID,
    )
    assert terminal is None

    result = dig_service.apply_dig_outcome(
        preconditions,
        {
            "advance": 1,
            "jc_earned": 0,
            "cave_in": False,
            "event_id": "",
        },
    )

    assert result["depth_after"] == 24
    assert result["pet_dig_bonus"] == 3
    assert result["boss_encounter"]
    assert pet_repo.get_pet_by_id(
        pet.pet_id,
        TEST_GUILD_ID,
    ).dig_work_units == 9 * DAY


def test_applied_cave_in_does_not_consume_pet_work(pet_dig, monkeypatch):
    clock, _pet_service, dig_service, pet_repo, dig_repo, pet = pet_dig
    clock.now = pet.hatched_at
    _seed_started_tunnel(dig_repo, depth=10)
    _seed_work(pet_repo, pet, blocks=12, at=clock.now)
    _make_normal_dig_deterministic(monkeypatch)

    terminal, preconditions = dig_service.dig_with_preconditions(
        USER_ID,
        TEST_GUILD_ID,
    )
    assert terminal is None

    result = dig_service.apply_dig_outcome(
        preconditions,
        {
            "advance": 0,
            "jc_earned": 0,
            "cave_in": True,
            "event_id": "",
        },
    )

    assert result["cave_in"]
    assert result["pet_dig_bonus"] == 0
    assert pet_repo.get_pet_by_id(
        pet.pet_id,
        TEST_GUILD_ID,
    ).dig_work_units == 12 * DAY


def test_pet_work_race_does_not_charge_a_paid_dig(pet_dig, monkeypatch):
    clock, _pet_service, dig_service, pet_repo, dig_repo, pet = pet_dig
    clock.now = pet.hatched_at
    _seed_started_tunnel(dig_repo, depth=10)
    dig_repo.update_tunnel(
        USER_ID,
        TEST_GUILD_ID,
        last_dig_at=clock.now,
    )
    _seed_work(pet_repo, pet, blocks=12, at=clock.now)
    _make_normal_dig_deterministic(monkeypatch)
    balance_before = dig_service.player_repo.get_balance(
        USER_ID,
        TEST_GUILD_ID,
    )
    original_profit_policies = dig_service._apply_jc_profit_policies
    raced = False

    def settle_pet_during_dig(discord_id, guild_id, amount):
        nonlocal raced
        if not raced:
            raced = True
            with pet_repo.connection() as conn:
                conn.execute(
                    "UPDATE pets SET dig_work_units = ? WHERE pet_id = ?",
                    (13 * DAY, pet.pet_id),
                )
        return original_profit_policies(discord_id, guild_id, amount)

    monkeypatch.setattr(
        dig_service,
        "_apply_jc_profit_policies",
        settle_pet_during_dig,
    )

    with pytest.raises(RuntimeError, match="pet work changed"):
        dig_service.dig(USER_ID, TEST_GUILD_ID, paid=True)

    tunnel = dig_repo.get_tunnel(USER_ID, TEST_GUILD_ID)
    fresh_pet = pet_repo.get_pet_by_id(pet.pet_id, TEST_GUILD_ID)
    assert dig_service.player_repo.get_balance(
        USER_ID,
        TEST_GUILD_ID,
    ) == balance_before
    assert tunnel["depth"] == 10
    assert tunnel["paid_digs_today"] == 0
    assert fresh_pet.dig_work_units == 13 * DAY


def test_paid_dig_counter_race_rolls_back_the_dig(pet_dig, monkeypatch):
    clock, _pet_service, dig_service, _pet_repo, dig_repo, _pet = pet_dig
    clock.now = _pet.hatched_at
    _seed_started_tunnel(dig_repo, depth=10)
    dig_repo.update_tunnel(
        USER_ID,
        TEST_GUILD_ID,
        last_dig_at=clock.now,
    )
    _make_normal_dig_deterministic(monkeypatch)
    balance_before = dig_service.player_repo.get_balance(
        USER_ID,
        TEST_GUILD_ID,
    )
    today = dig_service._get_game_date()
    original_profit_policies = dig_service._apply_jc_profit_policies
    raced = False

    def count_another_paid_dig(discord_id, guild_id, amount):
        nonlocal raced
        if not raced:
            raced = True
            dig_repo.update_tunnel(
                discord_id,
                guild_id,
                paid_dig_date=today,
                paid_digs_today=1,
            )
        return original_profit_policies(discord_id, guild_id, amount)

    monkeypatch.setattr(
        dig_service,
        "_apply_jc_profit_policies",
        count_another_paid_dig,
    )

    with pytest.raises(TunnelStateConflictError, match="tunnel state changed"):
        dig_service.dig(USER_ID, TEST_GUILD_ID, paid=True)

    tunnel = dig_repo.get_tunnel(USER_ID, TEST_GUILD_ID)
    assert dig_service.player_repo.get_balance(
        USER_ID,
        TEST_GUILD_ID,
    ) == balance_before
    assert tunnel["depth"] == 10
    assert tunnel["paid_dig_date"] == today
    assert tunnel["paid_digs_today"] == 1
