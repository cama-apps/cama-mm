"""PetBrawlService tests: real repositories, steerable clock, seeded rng."""

from __future__ import annotations

import random
import sqlite3

import pytest

from domain.pet_brawl import resolve_round
from repositories.pet_brawl_repository import PetBrawlRepository
from repositories.pet_repository import PetRepository
from repositories.player_repository import PlayerRepository
from services import error_codes
from services.pet_brawl_service import PetBrawlService
from services.pet_service import PetService
from tests.conftest import TEST_GUILD_ID

T0 = 1_800_000_000
DAY = 86400

CHALLENGER = 100
RECIPIENT = 200


class SteerableClock:
    def __init__(self, now: int = T0):
        self.now = now

    def __call__(self) -> int:
        return self.now


@pytest.fixture
def clock():
    return SteerableClock()


@pytest.fixture
def pet_service(repo_db_path, clock, monkeypatch):
    pet_repo = PetRepository(repo_db_path)
    player_repo = PlayerRepository(repo_db_path)
    svc = PetService(pet_repo, player_repo, decay_per_day=20)
    monkeypatch.setattr(svc, "_now", clock)
    return svc


@pytest.fixture
def service(repo_db_path, pet_service):
    return PetBrawlService(
        pet_service,
        PetBrawlRepository(repo_db_path),
        rng=random.Random(42),
    )


@pytest.fixture
def insert_pet(repo_db_path):
    def _insert(
        discord_id,
        *,
        species="common_cama",
        hunger=100,
        last_fed_at=T0,
        hatched_at=T0 - 9 * DAY,
        died_at=None,
    ):
        with sqlite3.connect(repo_db_path) as conn:
            cursor = conn.execute(
                "INSERT INTO pets (discord_id, guild_id, name, species, "
                "adopted_at, hatched_at, adopt_fee, last_fed_at, "
                "hunger_at_last_fed, died_at) "
                "VALUES (?, ?, 'Brawler', ?, ?, ?, 20, ?, ?, ?)",
                (
                    discord_id,
                    TEST_GUILD_ID,
                    species,
                    hatched_at - DAY,
                    hatched_at,
                    last_fed_at,
                    hunger,
                    died_at,
                ),
            )
            return cursor.lastrowid

    return _insert


def start_battle(service, insert_pet, **challenger_pet_kw):
    pet_a = insert_pet(CHALLENGER, **challenger_pet_kw)
    pet_b = insert_pet(RECIPIENT)
    challenge = service.challenge(CHALLENGER, RECIPIENT, TEST_GUILD_ID, 555)
    assert challenge.success, challenge.error
    accept = service.accept(
        challenge.value["brawl"].brawl_id, TEST_GUILD_ID, RECIPIENT
    )
    assert accept.success, accept.error
    return accept.value, pet_a, pet_b


class TestChallenge:
    def test_rejects_self(self, service):
        result = service.challenge(CHALLENGER, CHALLENGER, TEST_GUILD_ID, 555)
        assert not result.success
        assert result.error_code == error_codes.VALIDATION_ERROR

    def test_rejects_petless_challenger(self, service, insert_pet):
        insert_pet(RECIPIENT)
        result = service.challenge(CHALLENGER, RECIPIENT, TEST_GUILD_ID, 555)
        assert result.error_code == error_codes.NO_PET

    def test_rejects_petless_recipient(self, service, insert_pet):
        insert_pet(CHALLENGER)
        result = service.challenge(CHALLENGER, RECIPIENT, TEST_GUILD_ID, 555)
        assert result.error_code == error_codes.NO_PET
        assert "opponent" in result.error.lower()

    def test_rejects_egg(self, service, insert_pet, clock):
        insert_pet(CHALLENGER, hatched_at=clock.now + DAY, last_fed_at=clock.now + DAY)
        insert_pet(RECIPIENT)
        result = service.challenge(CHALLENGER, RECIPIENT, TEST_GUILD_ID, 555)
        assert result.error_code == error_codes.PET_EGG

    def test_rejects_busy_player(self, service, insert_pet):
        insert_pet(CHALLENGER)
        insert_pet(RECIPIENT)
        insert_pet(300)
        assert service.challenge(CHALLENGER, RECIPIENT, TEST_GUILD_ID, 555).success
        result = service.challenge(300, CHALLENGER, TEST_GUILD_ID, 555)
        assert result.error_code == error_codes.BRAWL_BUSY

    def test_starved_challenger_pet_resolves_to_no_pet(
        self, service, insert_pet, clock
    ):
        # Anchored long ago: derived-starved, lazily claimed on the read path.
        insert_pet(CHALLENGER, last_fed_at=T0 - 30 * DAY, hatched_at=T0 - 40 * DAY)
        insert_pet(RECIPIENT)
        result = service.challenge(CHALLENGER, RECIPIENT, TEST_GUILD_ID, 555)
        assert result.error_code == error_codes.NO_PET


class TestAccept:
    def test_accept_builds_initial_state(self, service, insert_pet):
        battle, pet_a, pet_b = start_battle(service, insert_pet)
        state = battle["state"]
        assert state.a.pet_id == pet_a
        assert state.b.pet_id == pet_b
        assert state.a.owner_id == CHALLENGER
        assert state.round_no == 0
        assert battle["brawl"].status == "active"
        assert isinstance(battle["rng"], random.Random)

    def test_snapshot_reflects_hunger_and_stage(self, service, insert_pet, clock):
        # Challenger's pet hatched 2 days ago (baby), hunger 60 at accept.
        insert_pet(
            CHALLENGER, hunger=60, last_fed_at=clock.now, hatched_at=T0 - 2 * DAY
        )
        insert_pet(RECIPIENT)
        challenge = service.challenge(CHALLENGER, RECIPIENT, TEST_GUILD_ID, 555)
        accept = service.accept(
            challenge.value["brawl"].brawl_id, TEST_GUILD_ID, RECIPIENT
        )
        duelist = accept.value["state"].a
        assert not duelist.is_adult
        assert duelist.max_hp == 100
        assert duelist.hp == 100 - (100 - 60) // 5

    def test_accept_voids_when_challenger_pet_starved(
        self, service, insert_pet, clock
    ):
        insert_pet(CHALLENGER)
        insert_pet(RECIPIENT)
        challenge = service.challenge(CHALLENGER, RECIPIENT, TEST_GUILD_ID, 555)
        brawl_id = challenge.value["brawl"].brawl_id
        # Challenger's pet starves while the challenge sits open (100 hunger
        # at 20/day = dead in 5 days; the 3-minute window is steered past it).
        clock.now = T0 + 6 * DAY
        result = service.accept(brawl_id, TEST_GUILD_ID, RECIPIENT)
        assert result.error_code == error_codes.PET_DEAD
        assert (
            service.pet_brawl_repo.get_brawl(brawl_id, TEST_GUILD_ID).status == "void"
        )

    def test_wrong_recipient_rejected(self, service, insert_pet):
        insert_pet(CHALLENGER)
        insert_pet(RECIPIENT)
        challenge = service.challenge(CHALLENGER, RECIPIENT, TEST_GUILD_ID, 555)
        result = service.accept(
            challenge.value["brawl"].brawl_id, TEST_GUILD_ID, 999
        )
        assert result.error_code == error_codes.NO_PET  # 999 has no pet either
        insert_pet(999)
        result = service.accept(
            challenge.value["brawl"].brawl_id, TEST_GUILD_ID, 999
        )
        assert result.error_code == error_codes.VALIDATION_ERROR


class TestDeclineWithdraw:
    def test_decline(self, service, insert_pet):
        insert_pet(CHALLENGER)
        insert_pet(RECIPIENT)
        challenge = service.challenge(CHALLENGER, RECIPIENT, TEST_GUILD_ID, 555)
        brawl_id = challenge.value["brawl"].brawl_id
        assert service.decline(brawl_id, TEST_GUILD_ID, RECIPIENT).success
        assert (
            service.pet_brawl_repo.get_brawl(brawl_id, TEST_GUILD_ID).status
            == "declined"
        )

    def test_withdraw_challenger_only(self, service, insert_pet):
        insert_pet(CHALLENGER)
        insert_pet(RECIPIENT)
        challenge = service.challenge(CHALLENGER, RECIPIENT, TEST_GUILD_ID, 555)
        brawl_id = challenge.value["brawl"].brawl_id
        assert not service.withdraw(brawl_id, TEST_GUILD_ID, RECIPIENT).success
        assert service.withdraw(brawl_id, TEST_GUILD_ID, CHALLENGER).success
        assert (
            service.pet_brawl_repo.get_brawl(brawl_id, TEST_GUILD_ID).status == "void"
        )


class TestSettle:
    def play_out(self, battle):
        state, rng = battle["state"], battle["rng"]
        rounds = 0
        from domain.pet_brawl import SAFE_MOVE

        while state.winner is None:
            state, _ = resolve_round(state, SAFE_MOVE, SAFE_MOVE, rng)
            rounds += 1
        return state, rounds

    def test_settle_applies_hunger_and_records(
        self, service, pet_service, insert_pet
    ):
        battle, pet_a, pet_b = start_battle(
            service, insert_pet, hunger=50
        )
        state, rounds = self.play_out(battle)
        result = service.settle(
            battle["brawl"].brawl_id, TEST_GUILD_ID, state, rounds
        )
        assert result.success, result.error
        winner, loser = result.value["winner"], result.value["loser"]
        assert {winner.pet_id, loser.pet_id} == {pet_a, pet_b}
        assert result.value["records"][winner.pet_id] == (1, 0)
        assert result.value["records"][loser.pet_id] == (0, 1)
        if winner.pet_id == pet_a:  # started at hunger 50 -> gains
            assert result.value["winner_delta"] == 10
        else:
            assert result.value["loser_delta"] == -15

    def test_pack_cama_winner_gains_bonus(self, service, insert_pet, clock):
        pet_a = insert_pet(CHALLENGER, species="courier_cama", hunger=50)
        insert_pet(RECIPIENT)
        challenge = service.challenge(CHALLENGER, RECIPIENT, TEST_GUILD_ID, 555)
        battle = service.accept(
            challenge.value["brawl"].brawl_id, TEST_GUILD_ID, RECIPIENT
        ).value
        # Force the challenger as winner regardless of the fight.
        from dataclasses import replace

        state = battle["state"]
        forced = type(state)(
            a=state.a, b=replace(state.b, hp=0), round_no=3, winner="a"
        )
        result = service.settle(battle["brawl"].brawl_id, TEST_GUILD_ID, forced, 3)
        assert result.value["winner"].pet_id == pet_a
        assert result.value["winner_delta"] == 15  # 10 + Pack Cama 5

    def test_gilded_loser_insured(self, service, insert_pet):
        insert_pet(CHALLENGER)
        insert_pet(RECIPIENT, species="jopacama")
        challenge = service.challenge(CHALLENGER, RECIPIENT, TEST_GUILD_ID, 555)
        battle = service.accept(
            challenge.value["brawl"].brawl_id, TEST_GUILD_ID, RECIPIENT
        ).value
        from dataclasses import replace

        state = battle["state"]
        forced = type(state)(
            a=state.a, b=replace(state.b, hp=0), round_no=3, winner="a"
        )
        result = service.settle(battle["brawl"].brawl_id, TEST_GUILD_ID, forced, 3)
        assert result.value["loser_delta"] == -10  # 15 - 5 insurance

    def test_settle_unfinished_state_rejected(self, service, insert_pet):
        battle, _, _ = start_battle(service, insert_pet)
        result = service.settle(
            battle["brawl"].brawl_id, TEST_GUILD_ID, battle["state"], 0
        )
        assert not result.success


class TestSweep:
    def test_sweep_stale_expires_and_voids(self, service, insert_pet, clock):
        insert_pet(CHALLENGER)
        insert_pet(RECIPIENT)
        challenge = service.challenge(CHALLENGER, RECIPIENT, TEST_GUILD_ID, 555)
        brawl_id = challenge.value["brawl"].brawl_id
        clock.now = T0 + 4 * DAY  # past accept window, pets still alive
        assert service.sweep_stale() == {"expired": 1, "voided": 0}
        assert (
            service.pet_brawl_repo.get_brawl(brawl_id, TEST_GUILD_ID).status
            == "expired"
        )
