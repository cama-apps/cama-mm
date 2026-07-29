"""Evolution orchestration and lazy-lifecycle ordering tests."""

from __future__ import annotations

import sqlite3
from unittest.mock import MagicMock

import pytest

from domain.models.pet import EvolutionNotice
from domain.pet_evolution import PetActivity, PetCalling, PetInstinct
from repositories.pet_evolution_repository import PetEvolutionRepository
from repositories.pet_repository import PetRepository
from repositories.player_repository import PlayerRepository
from services.pet_evolution_service import PetEvolutionService
from services.pet_service import PetService
from tests.conftest import TEST_GUILD_ID

T0 = 1_800_000_000
DAY = 86400


def insert_upbringing_pet(
    db_path: str,
    *,
    starvation_at: int,
    due_at: int = T0 + 7 * DAY,
) -> int:
    with sqlite3.connect(db_path) as conn:
        cursor = conn.execute(
            """
            INSERT INTO pets (
                discord_id, guild_id, name, species, adopted_at, hatched_at,
                adopt_fee, last_fed_at, hunger_at_last_fed,
                evolution_started_at, evolution_due_at
            )
            VALUES (?, ?, 'Blep', 'common_cama', ?, ?, 20, ?, 100, ?, ?)
            """,
            (
                100,
                TEST_GUILD_ID,
                T0 - DAY,
                T0,
                starvation_at - 5 * DAY,
                T0,
                due_at,
            ),
        )
        return int(cursor.lastrowid)


@pytest.fixture
def evolution_stack(repo_db_path):
    pet_repo = PetRepository(repo_db_path)
    evolution_repo = PetEvolutionRepository(repo_db_path)
    evolution_service = PetEvolutionService(evolution_repo, pet_repo)
    pet_service = PetService(
        pet_repo,
        PlayerRepository(repo_db_path),
        decay_per_day=20,
        evolution_service=evolution_service,
    )
    return pet_repo, evolution_repo, evolution_service, pet_service


class TestActivityBoundary:
    def test_record_activity_derives_clock_once(self):
        repo = MagicMock()
        repo.record_activity.return_value = 2
        service = PetEvolutionService(repo, MagicMock())
        service._now = lambda: T0

        assert (
            service.record_activity(
                100,
                TEST_GUILD_ID,
                PetActivity.DIG_COMPLETED,
                "dig:123",
            )
            == 2
        )

        repo.record_activity.assert_called_once_with(
            100,
            TEST_GUILD_ID,
            activity=PetActivity.DIG_COMPLETED,
            source_key="dig:123",
            occurred_at=T0,
        )

    def test_record_activity_is_fail_open_for_primary_gameplay(self):
        repo = MagicMock()
        repo.record_activity.side_effect = sqlite3.OperationalError("locked")
        service = PetEvolutionService(repo, MagicMock())

        assert (
            service.record_activity(
                100,
                TEST_GUILD_ID,
                PetActivity.DIG_COMPLETED,
                "dig:123",
                occurred_at=T0,
            )
            == 0
        )


class TestLazyLifecycleOrdering:
    def test_pet_evolves_at_due_time_before_a_later_starvation(
        self, evolution_stack
    ):
        pet_repo, evolution_repo, evolution_service, pet_service = evolution_stack
        due = T0 + 7 * DAY
        pet_id = insert_upbringing_pet(
            pet_repo.db_path,
            starvation_at=due + 100,
            due_at=due,
        )
        evolution_repo.record_activity(
            100,
            TEST_GUILD_ID,
            activity=PetActivity.DIG_COMPLETED,
            source_key="dig:1",
            occurred_at=T0 + 10,
        )
        evolution_repo.record_activity(
            100,
            TEST_GUILD_ID,
            activity=PetActivity.DIG_COMPLETED,
            source_key="dig:2",
            occurred_at=T0 + 20,
        )

        assert (
            pet_service.living_pet_by_id(
                pet_id,
                TEST_GUILD_ID,
                due + 200,
            )
            is None
        )

        pet = pet_repo.get_pet_by_id(pet_id, TEST_GUILD_ID)
        assert pet.evolved_at == due
        assert pet.evolution_calling == PetCalling.DELVER
        assert pet.evolution_primary == PetInstinct.DELVING
        assert pet.evolution_secondary is None
        assert pet.died_at == due + 100

        notices = evolution_service.get_unannounced()
        assert notices == [EvolutionNotice(pet=pet)]
        assert evolution_service.callingdex(100, TEST_GUILD_ID) == (
            [PetCalling.DELVER],
            len(PetCalling),
        )
        evolution_service.mark_announced(pet)
        assert evolution_service.get_unannounced() == []

    def test_status_exposes_only_the_narrative_hint_key(self, evolution_stack):
        pet_repo, evolution_repo, _evolution_service, pet_service = evolution_stack
        pet_id = insert_upbringing_pet(
            pet_repo.db_path,
            starvation_at=T0 + 10 * DAY,
        )
        evolution_repo.record_activity(
            100,
            TEST_GUILD_ID,
            activity=PetActivity.DIG_COMPLETED,
            source_key="dig:1",
            occurred_at=T0 + 10,
        )
        evolution_repo.record_activity(
            100,
            TEST_GUILD_ID,
            activity=PetActivity.DIG_COMPLETED,
            source_key="dig:2",
            occurred_at=T0 + 20,
        )
        pet_service._now = lambda: T0 + 100

        status = pet_service.get_status(100, TEST_GUILD_ID).value

        assert status.pet.pet_id == pet_id
        assert status.evolution_hint == "delving"

    def test_pet_that_starves_at_the_due_moment_does_not_evolve(
        self, evolution_stack
    ):
        pet_repo, _evolution_repo, _evolution_service, pet_service = evolution_stack
        due = T0 + 7 * DAY
        pet_id = insert_upbringing_pet(
            pet_repo.db_path,
            starvation_at=due,
            due_at=due,
        )

        assert (
            pet_service.living_pet_by_id(
                pet_id,
                TEST_GUILD_ID,
                due,
            )
            is None
        )

        pet = pet_repo.get_pet_by_id(pet_id, TEST_GUILD_ID)
        assert pet.died_at == due
        assert pet.evolved_at is None
        assert pet.evolution_calling is None

    def test_sweep_resolves_healthy_due_pets_without_scanning_all_pets(
        self, evolution_stack
    ):
        pet_repo, _evolution_repo, _evolution_service, pet_service = evolution_stack
        due = T0 + 7 * DAY
        pet_id = insert_upbringing_pet(
            pet_repo.db_path,
            starvation_at=due + 5 * DAY,
            due_at=due,
        )
        pet_service._now = lambda: due

        result = pet_service.sweep()

        pet = pet_repo.get_pet_by_id(pet_id, TEST_GUILD_ID)
        assert pet.evolved_at == due
        assert pet.evolution_calling == PetCalling.WAYFARER
        assert result["evolutions"] == [EvolutionNotice(pet=pet)]
