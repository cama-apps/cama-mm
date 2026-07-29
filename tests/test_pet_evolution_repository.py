"""Real-SQLite coverage for bounded Camagotchi evolution persistence."""

from __future__ import annotations

import sqlite3
import threading
import time

import pytest

from domain.pet_constants import ADULT_AGE_SECONDS
from domain.pet_evolution import PetActivity, PetInstinct
from repositories.pet_evolution_repository import PetEvolutionRepository
from repositories.pet_repository import PetRepository
from tests.conftest import TEST_GUILD_ID

T0 = 1_800_000_000
DAY = 86400


@pytest.fixture
def evolution_repo(repo_db_path):
    return PetEvolutionRepository(repo_db_path)


def insert_pet(
    db_path: str,
    *,
    discord_id: int = 100,
    guild_id: int = TEST_GUILD_ID,
    start: int = T0,
    due: int = T0 + 7 * DAY,
    died_at: int | None = None,
    evolved_at: int | None = None,
) -> int:
    with sqlite3.connect(db_path) as conn:
        cursor = conn.execute(
            """
            INSERT INTO pets (
                discord_id, guild_id, name, species, adopted_at, hatched_at,
                adopt_fee, last_fed_at, hunger_at_last_fed,
                died_at, death_cause,
                evolution_started_at, evolution_due_at, evolved_at,
                evolution_calling
            )
            VALUES (
                ?, ?, 'Blep', 'common_cama', ?, ?, 20, ?, 100,
                ?, ?, ?, ?, ?, ?
            )
            """,
            (
                discord_id,
                guild_id,
                start - DAY,
                start,
                start,
                died_at,
                "test death" if died_at is not None else None,
                start,
                due,
                evolved_at,
                "delver" if evolved_at is not None else None,
            ),
        )
        return int(cursor.lastrowid)


def score(
    repo: PetEvolutionRepository,
    activity: PetActivity,
    source_key: str,
    *,
    discord_id: int = 100,
    guild_id: int = TEST_GUILD_ID,
    occurred_at: int = T0 + 100,
) -> int:
    return repo.record_activity(
        discord_id,
        guild_id,
        activity=activity,
        source_key=source_key,
        occurred_at=occurred_at,
    )


class TestBoundedDailyScores:
    def test_first_action_awards_two_second_fills_cap_and_duplicates_are_noops(
        self, evolution_repo
    ):
        pet_id = insert_pet(evolution_repo.db_path)

        assert score(evolution_repo, PetActivity.DIG_COMPLETED, "dig:1") == 2
        assert score(evolution_repo, PetActivity.DIG_COMPLETED, "dig:1") == 0
        assert score(evolution_repo, PetActivity.DIG_COMPLETED, "dig:2") == 1
        assert score(evolution_repo, PetActivity.DIG_COMPLETED, "dig:3") == 0

        assert evolution_repo.get_scores(pet_id, TEST_GUILD_ID) == {
            PetInstinct.DELVING: 3,
            PetInstinct.COMPETITION: 0,
            PetInstinct.FORTUNE: 0,
            PetInstinct.FELLOWSHIP: 0,
            PetInstinct.WISDOM: 0,
        }

    def test_routine_care_scores_once_but_a_tip_can_fill_the_daily_path_cap(
        self, evolution_repo
    ):
        pet_id = insert_pet(evolution_repo.db_path)

        assert score(evolution_repo, PetActivity.PET_CARED, "feed:1") == 1
        assert score(evolution_repo, PetActivity.PET_CARED, "feed:2") == 0
        assert score(evolution_repo, PetActivity.TIP_SENT, "tip:1") == 2

        assert (
            evolution_repo.get_scores(pet_id, TEST_GUILD_ID)[
                PetInstinct.FELLOWSHIP
            ]
            == 3
        )

    def test_rows_stay_bounded_when_the_window_intersects_eight_calendar_dates(
        self, evolution_repo
    ):
        start = T0 + DAY - 60
        pet_id = insert_pet(
            evolution_repo.db_path,
            start=start,
            due=start + 7 * DAY,
        )
        activities = {
            PetInstinct.DELVING: PetActivity.DIG_COMPLETED,
            PetInstinct.COMPETITION: PetActivity.MATCH_RECORDED,
            PetInstinct.FORTUNE: PetActivity.WHEEL_SPUN,
            PetInstinct.FELLOWSHIP: PetActivity.TIP_SENT,
            PetInstinct.WISDOM: PetActivity.TRIVIA_COMPLETED,
        }

        instants = [start + 1, start + 61]
        instants.extend(start + day * DAY + 100 for day in range(1, 7))
        assert len(instants) == 8
        for instant_index, occurred_at in enumerate(instants):
            for instinct, activity in activities.items():
                for attempt in range(20):
                    score(
                        evolution_repo,
                        activity,
                        f"{instinct.value}:{instant_index}:{attempt}",
                        occurred_at=occurred_at,
                    )

        with sqlite3.connect(evolution_repo.db_path) as conn:
            row_count = conn.execute(
                "SELECT COUNT(*) FROM pet_evolution_daily WHERE pet_id = ?",
                (pet_id,),
            ).fetchone()[0]
        assert row_count == 35


class TestEvolutionWindows:
    def test_new_adoption_scores_from_hatch_until_normal_adulthood(
        self, repo_db_path, player_repository
    ):
        player_repository.add(100, "Owner", TEST_GUILD_ID)
        player_repository.update_balance(100, TEST_GUILD_ID, 100)
        pet = PetRepository(repo_db_path).adopt_pet(
            100,
            TEST_GUILD_ID,
            name="Blep",
            egg_tier="standard",
            fee=20,
            now=T0,
            hatch_seconds=DAY,
        )

        assert pet.evolution_started_at == pet.hatched_at
        assert pet.evolution_due_at == pet.hatched_at + ADULT_AGE_SECONDS
        assert pet.evolved_at is None


class TestEligibilityAndIsolation:
    def test_users_without_an_upbringing_pet_do_not_take_a_write_lock(
        self, evolution_repo, monkeypatch
    ):
        monkeypatch.setattr(
            evolution_repo,
            "atomic_transaction",
            lambda: pytest.fail("ineligible activity acquired a write lock"),
        )

        assert score(evolution_repo, PetActivity.DIG_COMPLETED, "dig:no-pet") == 0

    @pytest.mark.parametrize(
        ("setup_activity", "setup_source", "activity", "source"),
        [
            (
                PetActivity.DIG_COMPLETED,
                "dig:duplicate",
                PetActivity.DIG_COMPLETED,
                "dig:duplicate",
            ),
            (
                PetActivity.PET_CARED,
                "care:first",
                PetActivity.PET_CARED,
                "care:second",
            ),
        ],
    )
    def test_read_only_preflight_rejects_duplicate_and_daily_care(
        self,
        evolution_repo,
        monkeypatch,
        setup_activity,
        setup_source,
        activity,
        source,
    ):
        insert_pet(evolution_repo.db_path)
        assert score(evolution_repo, setup_activity, setup_source) > 0
        monkeypatch.setattr(
            evolution_repo,
            "atomic_transaction",
            lambda: pytest.fail("score-ineligible activity acquired a write lock"),
        )

        assert score(evolution_repo, activity, source) == 0

    def test_read_only_preflight_rejects_an_already_capped_path(
        self, evolution_repo, monkeypatch
    ):
        insert_pet(evolution_repo.db_path)
        assert score(evolution_repo, PetActivity.DIG_COMPLETED, "dig:1") == 2
        assert score(evolution_repo, PetActivity.DIG_COMPLETED, "dig:2") == 1
        monkeypatch.setattr(
            evolution_repo,
            "atomic_transaction",
            lambda: pytest.fail("capped path acquired a write lock"),
        )

        assert score(evolution_repo, PetActivity.DIG_COMPLETED, "dig:3") == 0

    def test_guild_scoping_selects_only_the_pet_in_that_guild(self, evolution_repo):
        pet_a = insert_pet(evolution_repo.db_path, guild_id=111)
        pet_b = insert_pet(evolution_repo.db_path, guild_id=222)

        assert score(
            evolution_repo,
            PetActivity.DIG_COMPLETED,
            "dig:1",
            guild_id=111,
        ) == 2

        assert evolution_repo.get_scores(pet_a, 111)[PetInstinct.DELVING] == 2
        assert evolution_repo.get_scores(pet_b, 222)[PetInstinct.DELVING] == 0

    @pytest.mark.parametrize(
        ("occurred_at", "died_at", "evolved_at"),
        [
            (T0 - 1, None, None),
            (T0 + 7 * DAY, None, None),
            (T0 + 100, T0 + 50, None),
            (T0 + 100, None, T0 + 50),
        ],
    )
    def test_events_outside_a_living_unresolved_upbringing_window_are_ignored(
        self, evolution_repo, occurred_at, died_at, evolved_at
    ):
        pet_id = insert_pet(
            evolution_repo.db_path,
            died_at=died_at,
            evolved_at=evolved_at,
        )

        assert score(
            evolution_repo,
            PetActivity.DIG_COMPLETED,
            "dig:outside",
            occurred_at=occurred_at,
        ) == 0
        assert evolution_repo.get_scores(pet_id, TEST_GUILD_ID)[PetInstinct.DELVING] == 0


class TestAtomicResolution:
    def test_activity_cannot_commit_between_score_snapshot_and_calling_claim(
        self, evolution_repo
    ):
        due = T0 + 7 * DAY
        pet_id = insert_pet(evolution_repo.db_path, due=due)
        assert score(evolution_repo, PetActivity.DIG_COMPLETED, "dig:1") == 2
        assert score(evolution_repo, PetActivity.DIG_COMPLETED, "dig:2") == 1

        decision_started = threading.Event()
        finish_decision = threading.Event()
        writer_entered = threading.Event()
        writer_repo = PetEvolutionRepository(evolution_repo.db_path)
        original_atomic = writer_repo.atomic_transaction

        def writer_atomic():
            writer_entered.set()
            return original_atomic()

        writer_repo.atomic_transaction = writer_atomic
        results = {}

        def decide(scores):
            decision_started.set()
            assert finish_decision.wait(timeout=2)
            from domain.pet_evolution import resolve_evolution

            return resolve_evolution(scores, pet_id=pet_id)

        def resolve():
            results["claimed"] = evolution_repo.claim_evolution(
                pet_id,
                TEST_GUILD_ID,
                expected_due_at=due,
                decide=decide,
            )

        def write():
            results["awarded"] = score(
                writer_repo,
                PetActivity.WHEEL_SPUN,
                "wheel:late",
                occurred_at=due - 1,
            )

        resolver = threading.Thread(target=resolve)
        writer = threading.Thread(target=write)
        resolver.start()
        assert decision_started.wait(timeout=2)
        writer.start()
        assert writer_entered.wait(timeout=2)
        time.sleep(0.05)
        finish_decision.set()
        resolver.join(timeout=2)
        writer.join(timeout=2)

        assert not resolver.is_alive()
        assert not writer.is_alive()
        assert results == {"claimed": True, "awarded": 0}
        with evolution_repo.connection() as conn:
            pet = conn.execute(
                "SELECT evolution_calling FROM pets WHERE pet_id = ?",
                (pet_id,),
            ).fetchone()
        assert pet["evolution_calling"] == "delver"


def test_unannounced_evolutions_are_returned_in_bounded_order(evolution_repo):
    pet_ids = [
        insert_pet(
            evolution_repo.db_path,
            discord_id=100 + index,
            evolved_at=T0 + index,
        )
        for index in range(3)
    ]

    refs = evolution_repo.find_unannounced_refs(limit=2)

    assert refs == [
        (pet_ids[0], TEST_GUILD_ID),
        (pet_ids[1], TEST_GUILD_ID),
    ]
