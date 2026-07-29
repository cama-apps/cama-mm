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


class ScriptedServiceRng:
    def getrandbits(self, _bits):
        return 42

    def choice(self, values):
        return values[0]

    def random(self):
        return 0.0


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
        training_xp=0,
        training_str=0,
        training_int=0,
        training_dex=0,
        solo_training_sessions=3,
        solo_training_recharged_at=None,
    ):
        with sqlite3.connect(repo_db_path) as conn:
            cursor = conn.execute(
                "INSERT INTO pets (discord_id, guild_id, name, species, "
                "adopted_at, hatched_at, adopt_fee, last_fed_at, "
                "hunger_at_last_fed, died_at, training_xp, training_str, "
                "training_int, training_dex, solo_training_sessions, "
                "solo_training_recharged_at) "
                "VALUES (?, ?, 'Brawler', ?, ?, ?, 20, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                (
                    discord_id,
                    TEST_GUILD_ID,
                    species,
                    hatched_at - DAY,
                    hatched_at,
                    last_fed_at,
                    hunger,
                    died_at,
                    training_xp,
                    training_str,
                    training_int,
                    training_dex,
                    solo_training_sessions,
                    solo_training_recharged_at,
                ),
            )
            return cursor.lastrowid

    return _insert


def seed_player(repo_db_path, discord_id, balance):
    repo = PlayerRepository(repo_db_path)
    repo.add(
        discord_id=discord_id,
        discord_username=f"Player {discord_id}",
        guild_id=TEST_GUILD_ID,
    )
    repo.update_balance(discord_id, TEST_GUILD_ID, balance)
    return repo


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


class TestSoloTraining:
    def test_first_training_spends_three_banked_sessions(self, service, insert_pet):
        pet_id = insert_pet(CHALLENGER)

        result = service.train_solo(CHALLENGER, TEST_GUILD_ID)

        assert result.success, result.error
        assert result.value["sessions_used"] == 3
        assert result.value["xp_delta"] == 3
        assert result.value["stat_gain"] is None
        assert result.value["sessions_available"] == 0
        pet = service.pet_service.pet_repo.get_pet_by_id(pet_id, TEST_GUILD_ID)
        assert pet.training_xp == 3
        assert pet.solo_training_sessions == 0
        assert pet.solo_training_recharged_at == T0
        assert service.record(pet_id, TEST_GUILD_ID) == (0, 0)

    def test_training_recharges_one_session_per_game_day(
        self, service, insert_pet, clock
    ):
        pet_id = insert_pet(
            CHALLENGER,
            solo_training_sessions=0,
            solo_training_recharged_at=T0,
        )
        clock.now = T0 + 2 * DAY

        result = service.train_solo(CHALLENGER, TEST_GUILD_ID)

        assert result.success, result.error
        assert result.value["sessions_used"] == 2
        assert result.value["xp_delta"] == 2
        pet = service.pet_service.pet_repo.get_pet_by_id(pet_id, TEST_GUILD_ID)
        assert pet.solo_training_sessions == 0
        assert pet.solo_training_recharged_at == clock.now

    def test_training_threshold_awards_a_random_eligible_stat(
        self, service, insert_pet
    ):
        service._rng = ScriptedServiceRng()
        pet_id = insert_pet(CHALLENGER, training_xp=4)

        result = service.train_solo(CHALLENGER, TEST_GUILD_ID)

        assert result.success, result.error
        assert result.value["xp_delta"] == 3
        assert result.value["stat_gain"] == "str"
        pet = service.pet_service.pet_repo.get_pet_by_id(pet_id, TEST_GUILD_ID)
        assert (pet.training_xp, pet.training_str) == (7, 1)

    def test_training_near_cap_preserves_unused_sessions(
        self, service, insert_pet
    ):
        service._rng = ScriptedServiceRng()
        pet_id = insert_pet(
            CHALLENGER,
            training_xp=19,
            training_str=1,
            training_int=1,
            training_dex=1,
        )

        result = service.train_solo(CHALLENGER, TEST_GUILD_ID)

        assert result.success, result.error
        assert result.value["sessions_used"] == 1
        assert result.value["sessions_available"] == 2
        assert result.value["stat_gain"] == "str"
        pet = service.pet_service.pet_repo.get_pet_by_id(pet_id, TEST_GUILD_ID)
        assert pet.training_xp == 20
        assert pet.solo_training_sessions == 2

    def test_training_without_a_session_is_a_cooldown(
        self, service, insert_pet
    ):
        insert_pet(
            CHALLENGER,
            solo_training_sessions=0,
            solo_training_recharged_at=T0,
        )

        result = service.train_solo(CHALLENGER, TEST_GUILD_ID)

        assert not result.success
        assert result.error_code == error_codes.COOLDOWN_ACTIVE

    def test_training_rejects_eggs(self, service, insert_pet, clock):
        insert_pet(
            CHALLENGER,
            hatched_at=clock.now + DAY,
            last_fed_at=clock.now + DAY,
        )

        result = service.train_solo(CHALLENGER, TEST_GUILD_ID)

        assert not result.success
        assert result.error_code == error_codes.PET_EGG

    def test_training_rejects_an_active_brawl(self, service, insert_pet):
        start_battle(service, insert_pet)

        result = service.train_solo(CHALLENGER, TEST_GUILD_ID)

        assert not result.success
        assert result.error_code == error_codes.BRAWL_BUSY

    def test_training_is_available_during_a_pending_challenge(
        self, service, insert_pet
    ):
        insert_pet(CHALLENGER)
        insert_pet(RECIPIENT)
        challenge = service.challenge(
            CHALLENGER, RECIPIENT, TEST_GUILD_ID, 555
        )
        assert challenge.success, challenge.error

        result = service.train_solo(RECIPIENT, TEST_GUILD_ID)

        assert result.success, result.error
        assert result.value["xp_delta"] == 3


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

    def test_optional_wager_uses_tip_fee_and_escrows_challenger(
        self, repo_db_path, service, insert_pet
    ):
        players = seed_player(repo_db_path, CHALLENGER, 100)
        seed_player(repo_db_path, RECIPIENT, 100)
        insert_pet(CHALLENGER)
        insert_pet(RECIPIENT)

        result = service.challenge(
            CHALLENGER, RECIPIENT, TEST_GUILD_ID, 555, wager=20
        )

        assert result.success, result.error
        assert (result.value["brawl"].wager, result.value["brawl"].fee) == (20, 1)
        assert players.get_balance(CHALLENGER, TEST_GUILD_ID) == 79

    @pytest.mark.parametrize("wager", [-1, 101, 1.5, True])
    def test_invalid_wager_is_rejected(self, service, wager):
        result = service.challenge(
            CHALLENGER, RECIPIENT, TEST_GUILD_ID, 555, wager=wager
        )

        assert not result.success
        assert result.error_code == error_codes.VALIDATION_ERROR


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

    def test_snapshot_includes_permanent_training_stats(self, service, insert_pet):
        insert_pet(
            CHALLENGER,
            training_xp=15,
            training_str=2,
            training_int=1,
            training_dex=0,
        )
        insert_pet(RECIPIENT)
        challenge = service.challenge(CHALLENGER, RECIPIENT, TEST_GUILD_ID, 555)

        accepted = service.accept(
            challenge.value["brawl"].brawl_id, TEST_GUILD_ID, RECIPIENT
        )

        duelist = accepted.value["state"].a
        assert (duelist.training_str, duelist.training_int, duelist.training_dex) == (
            2,
            1,
            0,
        )

    def test_accept_refreshes_training_changed_after_initial_validation(
        self, repo_db_path, service, insert_pet, monkeypatch
    ):
        insert_pet(CHALLENGER)
        recipient_pet_id = insert_pet(RECIPIENT, training_xp=4)
        challenge = service.challenge(
            CHALLENGER, RECIPIENT, TEST_GUILD_ID, 555
        )
        original_accept = service.pet_brawl_repo.accept_atomic

        def train_then_accept(*args, **kwargs):
            with sqlite3.connect(repo_db_path) as conn:
                conn.execute(
                    "UPDATE pets SET training_xp = 5, training_int = 1 "
                    "WHERE pet_id = ?",
                    (recipient_pet_id,),
                )
            return original_accept(*args, **kwargs)

        monkeypatch.setattr(
            service.pet_brawl_repo,
            "accept_atomic",
            train_then_accept,
        )

        result = service.accept(
            challenge.value["brawl"].brawl_id,
            TEST_GUILD_ID,
            RECIPIENT,
        )

        assert result.success, result.error
        assert result.value["state"].b.training_int == 1

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

    def test_withdraw_racing_accept_cannot_void_active_brawl(
        self, service, insert_pet, monkeypatch
    ):
        """A withdraw whose pre-read saw 'pending' must lose to a committed accept."""
        insert_pet(CHALLENGER)
        insert_pet(RECIPIENT)
        challenge = service.challenge(CHALLENGER, RECIPIENT, TEST_GUILD_ID, 555)
        brawl_id = challenge.value["brawl"].brawl_id
        stale = service.pet_brawl_repo.get_brawl(brawl_id, TEST_GUILD_ID)
        assert service.accept(brawl_id, TEST_GUILD_ID, RECIPIENT).success
        monkeypatch.setattr(
            service.pet_brawl_repo, "get_brawl", lambda *a, **k: stale
        )
        assert not service.withdraw(brawl_id, TEST_GUILD_ID, CHALLENGER).success
        monkeypatch.undo()
        assert (
            service.pet_brawl_repo.get_brawl(brawl_id, TEST_GUILD_ID).status
            == "active"
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

    def test_settle_returns_training_titles_and_persisted_personality_event(
        self, service, pet_service, insert_pet
    ):
        service._rng = ScriptedServiceRng()
        insert_pet(CHALLENGER, training_xp=4)
        insert_pet(RECIPIENT, training_xp=4)
        challenge = service.challenge(CHALLENGER, RECIPIENT, TEST_GUILD_ID, 555)
        battle = service.accept(
            challenge.value["brawl"].brawl_id, TEST_GUILD_ID, RECIPIENT
        ).value
        from dataclasses import replace

        state = battle["state"]
        forced = type(state)(
            a=state.a, b=replace(state.b, hp=0), round_no=3, winner="a"
        )
        result = service.settle(
            battle["brawl"].brawl_id, TEST_GUILD_ID, forced, 3
        )

        assert result.success, result.error
        assert result.value["winner_xp_delta"] == 2
        assert result.value["loser_xp_delta"] == 1
        assert result.value["winner_stat_gain"] in {"str", "int", "dex"}
        assert result.value["loser_stat_gain"] in {"str", "int", "dex"}
        assert result.value["career"][state.a.pet_id]["rank"] == "Rookie"
        assert result.value["career"][state.b.pet_id]["rank"] == "Rookie"
        done = service.pet_brawl_repo.get_brawl(
            battle["brawl"].brawl_id, TEST_GUILD_ID
        )
        assert done.challenger_stat_gain == result.value["winner_stat_gain"]
        assert done.personality_event_key == result.value["personality_event_key"]
        assert result.value["personality_event"]

    def test_reaching_max_training_uses_build_milestone_event(
        self, service, insert_pet
    ):
        service._rng = ScriptedServiceRng()
        insert_pet(
            CHALLENGER,
            training_xp=19,
            training_str=1,
            training_int=1,
            training_dex=1,
        )
        insert_pet(RECIPIENT)
        challenge = service.challenge(CHALLENGER, RECIPIENT, TEST_GUILD_ID, 555)
        battle = service.accept(
            challenge.value["brawl"].brawl_id, TEST_GUILD_ID, RECIPIENT
        ).value
        from dataclasses import replace

        state = battle["state"]
        forced = type(state)(
            a=state.a, b=replace(state.b, hp=0), round_no=3, winner="a"
        )

        result = service.settle(
            battle["brawl"].brawl_id, TEST_GUILD_ID, forced, 3
        )

        assert result.success, result.error
        assert result.value["personality_event_key"] == "milestone.build"
        assert "fully trained" in result.value["personality_event"].lower()
        done = service.pet_brawl_repo.get_brawl(
            battle["brawl"].brawl_id, TEST_GUILD_ID
        )
        assert done.personality_event_key == "milestone.build"

    def test_career_summary_separates_rank_from_max_training_build(
        self, service, insert_pet
    ):
        pet_id = insert_pet(
            CHALLENGER,
            training_xp=20,
            training_str=2,
            training_int=1,
            training_dex=1,
        )
        pet = service.pet_service.pet_repo.get_pet_by_id(pet_id, TEST_GUILD_ID)

        summary = service.career_summary(pet, wins=10, losses=2)

        assert summary == {
            "xp": 20,
            "str": 2,
            "int": 1,
            "dex": 1,
            "rank": "Champion",
            "build": "Bruiser",
        }


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
