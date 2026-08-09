"""Adult cama consumption across repository and service boundaries."""

from __future__ import annotations

import json
import sqlite3
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from domain.pet_constants import ADULT_AGE_SECONDS
from repositories.bankruptcy_repository import BankruptcyRepository
from repositories.pet_brawl_repository import PetBrawlRepository
from repositories.pet_evolution_repository import PetEvolutionRepository
from repositories.pet_repository import PetRepository
from repositories.player_repository import PlayerRepository
from services import error_codes
from services.pet_evolution_service import PetEvolutionService
from services.pet_service import PetService
from tests.conftest import TEST_GUILD_ID

T0 = 1_800_000_000


@pytest.fixture
def player_repo(repo_db_path):
    repo = PlayerRepository(repo_db_path)
    repo.add(100, "Owner", TEST_GUILD_ID)
    repo.update_balance(100, TEST_GUILD_ID, 1000)
    return repo


@pytest.fixture
def pet_repo(repo_db_path, player_repo):
    return PetRepository(repo_db_path)


@pytest.fixture
def service(pet_repo, player_repo, monkeypatch):
    svc = PetService(pet_repo, player_repo, decay_per_day=20)
    monkeypatch.setattr(svc, "_now", lambda: T0)
    return svc


def make_pet(
    pet_repo,
    *,
    age: int = ADULT_AGE_SECONDS,
    owner_id: int = 100,
    name: str = "Blep",
):
    hatched_at = T0 - age
    with sqlite3.connect(pet_repo.db_path) as conn:
        cursor = conn.execute(
            "INSERT INTO pets (discord_id, guild_id, name, species, "
            "adopted_at, hatched_at, adopt_fee, last_fed_at, "
            "hunger_at_last_fed, dig_work_units, dig_work_at) "
            "VALUES (?, ?, ?, 'common_cama', ?, ?, 20, ?, 100, 0, ?)",
            (
                owner_id,
                TEST_GUILD_ID,
                name,
                hatched_at - 86400,
                hatched_at,
                T0,
                hatched_at,
            ),
        )
        pet_id = cursor.lastrowid
    return pet_repo.get_pet_by_id(pet_id, TEST_GUILD_ID)


def eat_kwargs(pet, **overrides):
    kwargs = {
        "pet_id": pet.pet_id,
        "expected_last_fed_at": pet.last_fed_at,
        "expected_hunger": pet.hunger_at_last_fed,
        "reward": 777,
        "penalty_games": 4,
        "now": T0,
    }
    kwargs.update(overrides)
    return kwargs


class TestEatRepository:
    def test_consumption_is_atomic_and_adds_existing_penalty(
        self, repo_db_path, pet_repo, player_repo
    ):
        pet = make_pet(pet_repo)
        bankruptcy_repo = BankruptcyRepository(repo_db_path)
        bankruptcy_repo.adjust_penalty_games(100, TEST_GUILD_ID, 2)

        dead, new_balance, penalty_total = pet_repo.eat_adult_pet_atomic(
            100, TEST_GUILD_ID, **eat_kwargs(pet)
        )

        assert dead.died_at == T0
        assert dead.death_cause == "eaten"
        assert dead.death_announced_at is None
        assert new_balance == 1777
        assert player_repo.get_balance(100, TEST_GUILD_ID) == 1777
        assert penalty_total == 6
        state = bankruptcy_repo.get_state(100, TEST_GUILD_ID)
        assert state["penalty_games_remaining"] == 6
        assert state["bankruptcy_count"] == 0
        assert state["last_bankruptcy_at"] is None

        with sqlite3.connect(repo_db_path) as conn:
            conn.row_factory = sqlite3.Row
            ledger = conn.execute(
                "SELECT delta, related_type, related_id, metadata "
                "FROM economy_ledger_entries "
                "WHERE source = 'pet' AND related_type = 'pet_eating'"
            ).fetchone()
        assert ledger is not None
        assert ledger["delta"] == 777
        assert ledger["related_id"] == str(pet.pet_id)
        assert json.loads(ledger["metadata"]) == {
            "pet_id": pet.pet_id,
            "species": "common_cama",
            "reward": 777,
            "penalty_games": 4,
            "penalty_games_remaining": 6,
        }
        assert pet_repo.get_eating_outcome(pet.pet_id, TEST_GUILD_ID) == {
            "reward": 777,
            "penalty_games_added": 4,
            "penalty_games_remaining": 6,
            "new_balance": 1777,
        }

    def test_bankruptcy_write_failure_rolls_back_pet_and_reward(
        self, repo_db_path, pet_repo, player_repo
    ):
        pet = make_pet(pet_repo)
        with sqlite3.connect(repo_db_path) as conn:
            conn.execute(
                "CREATE TRIGGER fail_pet_eating_penalty "
                "BEFORE INSERT ON bankruptcy_state "
                "WHEN NEW.discord_id = 100 "
                "BEGIN SELECT RAISE(ABORT, 'penalty write failed'); END"
            )

        with pytest.raises(sqlite3.IntegrityError, match="penalty write failed"):
            pet_repo.eat_adult_pet_atomic(
                100, TEST_GUILD_ID, **eat_kwargs(pet)
            )

        alive = pet_repo.get_active_pet(100, TEST_GUILD_ID)
        assert alive is not None and alive.pet_id == pet.pet_id
        assert player_repo.get_balance(100, TEST_GUILD_ID) == 1000
        with sqlite3.connect(repo_db_path) as conn:
            ledger_count = conn.execute(
                "SELECT COUNT(*) FROM economy_ledger_entries "
                "WHERE source = 'pet' AND related_type = 'pet_eating'"
            ).fetchone()[0]
        assert ledger_count == 0

    def test_rejects_baby(self, pet_repo):
        baby = make_pet(pet_repo, age=ADULT_AGE_SECONDS - 1)
        with pytest.raises(ValueError, match="not_adult"):
            pet_repo.eat_adult_pet_atomic(
                100, TEST_GUILD_ID, **eat_kwargs(baby)
            )

    def test_rejects_stale_hunger_anchor(self, pet_repo):
        pet = make_pet(pet_repo)
        with pytest.raises(ValueError, match="stale_pet"):
            pet_repo.eat_adult_pet_atomic(
                100,
                TEST_GUILD_ID,
                **eat_kwargs(pet, expected_hunger=99),
            )

    def test_rejects_active_brawl(
        self, repo_db_path, pet_repo, player_repo
    ):
        pet = make_pet(pet_repo)
        player_repo.add(200, "Opponent", TEST_GUILD_ID)
        make_pet(pet_repo, owner_id=200, name="Spit")
        PetBrawlRepository(repo_db_path).create_brawl_atomic(
            TEST_GUILD_ID,
            5,
            100,
            200,
            pet.pet_id,
            now=T0,
            expires_at=T0 + 180,
        )
        with pytest.raises(ValueError, match="in_brawl"):
            pet_repo.eat_adult_pet_atomic(
                100,
                TEST_GUILD_ID,
                **eat_kwargs(pet),
            )

    def test_brawl_cannot_be_created_after_challenger_pet_is_eaten(
        self, repo_db_path, pet_repo, player_repo
    ):
        pet = make_pet(pet_repo)
        player_repo.add(200, "Opponent", TEST_GUILD_ID)
        make_pet(pet_repo, owner_id=200, name="Spit")
        pet_repo.eat_adult_pet_atomic(
            100, TEST_GUILD_ID, **eat_kwargs(pet)
        )

        with pytest.raises(ValueError, match="challenger_pet_unavailable"):
            PetBrawlRepository(repo_db_path).create_brawl_atomic(
                TEST_GUILD_ID,
                5,
                100,
                200,
                pet.pet_id,
                now=T0,
                expires_at=T0 + 180,
            )

    def test_brawl_cannot_target_a_recipient_after_their_pet_is_eaten(
        self, repo_db_path, pet_repo, player_repo
    ):
        recipient_pet = make_pet(pet_repo)
        player_repo.add(200, "Opponent", TEST_GUILD_ID)
        challenger_pet = make_pet(pet_repo, owner_id=200, name="Spit")
        pet_repo.eat_adult_pet_atomic(
            100, TEST_GUILD_ID, **eat_kwargs(recipient_pet)
        )

        with pytest.raises(ValueError, match="recipient_pet_unavailable"):
            PetBrawlRepository(repo_db_path).create_brawl_atomic(
                TEST_GUILD_ID,
                5,
                200,
                100,
                challenger_pet.pet_id,
                now=T0,
                expires_at=T0 + 180,
            )

    def test_expired_pending_brawl_does_not_block_eating(
        self, repo_db_path, pet_repo, player_repo
    ):
        pet = make_pet(pet_repo)
        player_repo.add(200, "Opponent", TEST_GUILD_ID)
        make_pet(pet_repo, owner_id=200, name="Spit")
        PetBrawlRepository(repo_db_path).create_brawl_atomic(
            TEST_GUILD_ID,
            5,
            100,
            200,
            pet.pet_id,
            now=T0 - 181,
            expires_at=T0 - 1,
        )

        dead, _, _ = pet_repo.eat_adult_pet_atomic(
            100, TEST_GUILD_ID, **eat_kwargs(pet)
        )

        assert dead.death_cause == "eaten"

    def test_eaten_pet_does_not_advance_bad_luck_pity(self, pet_repo):
        pet = make_pet(pet_repo)
        pet_repo.eat_adult_pet_atomic(
            100, TEST_GUILD_ID, **eat_kwargs(pet)
        )

        assert pet_repo.recent_dead_species(100, TEST_GUILD_ID, 10) == []

    def test_eating_suppresses_a_queued_evolution_announcement(
        self, repo_db_path, pet_repo
    ):
        pet = make_pet(pet_repo)
        with sqlite3.connect(repo_db_path) as conn:
            conn.execute(
                "UPDATE pets SET evolution_due_at = ?, evolved_at = ?, "
                "evolution_calling = 'guardian' WHERE pet_id = ?",
                (T0 - 1, T0 - 1, pet.pet_id),
            )

        dead, _, _ = pet_repo.eat_adult_pet_atomic(
            100, TEST_GUILD_ID, **eat_kwargs(pet)
        )

        assert dead.evolution_announced_at == T0
        assert PetEvolutionRepository(repo_db_path).find_unannounced_refs() == []


class TestEatService:
    def test_rejects_petless_owner(self, service):
        result = service.eat(100, TEST_GUILD_ID)
        assert result.error_code == error_codes.NO_PET

    def test_rejects_baby(self, service, pet_repo):
        make_pet(pet_repo, age=ADULT_AGE_SECONDS - 1)
        result = service.eat(100, TEST_GUILD_ID)
        assert result.error_code == error_codes.PET_NOT_ADULT
        assert "fully grown" in result.error

    def test_preview_rejects_an_open_brawl(
        self, repo_db_path, service, pet_repo, player_repo
    ):
        pet = make_pet(pet_repo)
        player_repo.add(200, "Opponent", TEST_GUILD_ID)
        make_pet(pet_repo, owner_id=200, name="Spit")
        PetBrawlRepository(repo_db_path).create_brawl_atomic(
            TEST_GUILD_ID,
            5,
            100,
            200,
            pet.pet_id,
            now=T0,
            expires_at=T0 + 180,
        )

        result = service.eat_preview(100, TEST_GUILD_ID)

        assert result.error_code == error_codes.BRAWL_BUSY

    def test_rolls_inclusive_reward_and_penalty_ranges(self, service, pet_repo):
        make_pet(pet_repo)
        with patch(
            "services.pet_service.random.randint", side_effect=[500, 3]
        ) as randint:
            result = service.eat(100, TEST_GUILD_ID)

        assert result.success, result.error
        assert result.value["reward"] == 500
        assert result.value["penalty_games_added"] == 3
        assert result.value["penalty_games_remaining"] == 3
        assert result.value["new_balance"] == 1500
        assert result.value["pet"].death_cause == "eaten"
        assert [call.args for call in randint.call_args_list] == [
            (500, 1000),
            (3, 5),
        ]

    def test_eating_at_evolution_due_does_not_create_a_posthumous_calling(
        self, repo_db_path, pet_repo, player_repo, monkeypatch
    ):
        pet = make_pet(pet_repo)
        with sqlite3.connect(repo_db_path) as conn:
            conn.execute(
                "UPDATE pets SET evolution_started_at = ?, evolution_due_at = ? "
                "WHERE pet_id = ?",
                (T0 - ADULT_AGE_SECONDS, T0, pet.pet_id),
            )
        evolution_service = PetEvolutionService(
            PetEvolutionRepository(repo_db_path), pet_repo
        )
        svc = PetService(
            pet_repo,
            player_repo,
            decay_per_day=20,
            evolution_service=evolution_service,
        )
        monkeypatch.setattr(svc, "_now", lambda: T0)

        with patch("services.pet_service.random.randint", side_effect=[750, 4]):
            result = svc.eat(100, TEST_GUILD_ID)

        assert result.success, result.error
        dead = result.value["pet"]
        assert dead.died_at == T0
        assert dead.evolved_at is None
        assert dead.evolution_calling is None
        assert dead.evolution_primary is None
        assert dead.evolution_secondary is None
        assert evolution_service.get_unannounced() == []

    def test_sweep_reconstructs_an_undelivered_exact_outcome(
        self, service, pet_repo
    ):
        make_pet(pet_repo)
        with patch("services.pet_service.random.randint", side_effect=[888, 4]):
            result = service.eat(100, TEST_GUILD_ID)

        assert result.success, result.error
        notice = service.sweep()["deaths"][0]
        assert notice.eating_outcome == {
            "reward": 888,
            "penalty_games_added": 4,
            "penalty_games_remaining": 4,
            "new_balance": 1888,
        }


class AutoEatView:
    answer = True

    def __init__(self, owner_id, **kwargs):
        self.owner_id = owner_id
        self.value = self.answer
        self.message = None

    async def wait(self):
        return


@pytest.fixture
def eat_cog(service, monkeypatch):
    from commands.pet import PetCommands

    monkeypatch.setattr(
        "commands.pet.GLOBAL_RATE_LIMITER.check",
        lambda **kwargs: MagicMock(allowed=True, retry_after_seconds=0),
    )
    cog = PetCommands.__new__(PetCommands)
    cog.bot = MagicMock()
    cog.bot.reminder_service = MagicMock()
    cog.pet_service = service
    cog.pet_brawl_service = MagicMock()
    cog._brawl_sessions = {}
    cog._direct_death_deliveries = {}
    return cog


def make_interaction():
    interaction = MagicMock()
    interaction.user.id = 100
    interaction.guild.id = TEST_GUILD_ID
    interaction.response.defer = AsyncMock()
    interaction.response.send_message = AsyncMock()
    interaction.followup.send = AsyncMock(
        return_value=MagicMock(edit=AsyncMock())
    )
    return interaction


class TestEatCommand:
    @pytest.mark.asyncio
    async def test_validation_error_stays_private_before_public_defer(self, eat_cog):
        interaction = make_interaction()

        await eat_cog.eat.callback(eat_cog, interaction)

        interaction.response.defer.assert_not_awaited()
        interaction.response.send_message.assert_awaited_once()
        assert interaction.response.send_message.await_args.kwargs["ephemeral"] is True
        interaction.followup.send.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_warning_is_ominous_but_hides_both_ranges(
        self, eat_cog, pet_repo, monkeypatch
    ):
        make_pet(pet_repo)

        class Decline(AutoEatView):
            answer = False

        monkeypatch.setattr("commands.pet.ConfirmEatView", Decline, raising=False)
        interaction = make_interaction()
        await eat_cog.eat.callback(eat_cog, interaction)

        warning = interaction.followup.send.await_args_list[0].kwargs["embed"]
        copy = f"{warning.title}\n{warning.description}"
        assert "terrible hunger" in copy.lower()
        assert "tax your future earnings" in copy.lower()
        assert "cannot be undone" in copy.lower()
        assert "gone forever" in copy.lower()
        assert "500" not in copy
        assert "1,000" not in copy
        assert "3-5" not in copy and "3–5" not in copy
        assert pet_repo.get_active_pet(100, TEST_GUILD_ID) is not None

    @pytest.mark.asyncio
    async def test_confirmation_reveals_rolls(
        self, eat_cog, pet_repo, monkeypatch
    ):
        pet = make_pet(pet_repo)
        monkeypatch.setattr(
            "commands.pet.ConfirmEatView", AutoEatView, raising=False
        )
        interaction = make_interaction()
        with patch("services.pet_service.random.randint", side_effect=[888, 4]):
            await eat_cog.eat.callback(eat_cog, interaction)

        message = interaction.followup.send.return_value
        outcome = message.edit.await_args.kwargs["embed"]
        copy = f"{outcome.title}\n{outcome.description}"
        assert pet.name in copy
        assert "888" in copy
        assert "4" in copy
        assert "taxed" in copy.lower()
        assert message.edit.await_args.kwargs["view"] is None

    @pytest.mark.asyncio
    async def test_confirmation_cleans_up_death_delivery(
        self, eat_cog, pet_repo, monkeypatch
    ):
        pet = make_pet(pet_repo)
        monkeypatch.setattr(
            "commands.pet.ConfirmEatView", AutoEatView, raising=False
        )
        interaction = make_interaction()
        with patch("services.pet_service.random.randint", side_effect=[888, 4]):
            await eat_cog.eat.callback(eat_cog, interaction)

        dead = pet_repo.get_pet_by_id(pet.pet_id, TEST_GUILD_ID)
        assert dead.death_announced_at is not None
        eat_cog.bot.reminder_service.cancel_pet_reminder.assert_called_once_with(
            100, TEST_GUILD_ID
        )

    @pytest.mark.asyncio
    async def test_confirmation_buttons_match_the_choice(self):
        from commands.pet_helpers.views import ConfirmEatView

        labels = [item.label for item in ConfirmEatView(100).children]
        assert labels == ["Eat them", "Let them live"]


def test_eaten_death_copy_is_not_a_starvation_message(pet_repo):
    from commands.pet_helpers.embeds import build_death_embed

    pet = make_pet(pet_repo)
    dead, _, _ = pet_repo.eat_adult_pet_atomic(
        100, TEST_GUILD_ID, **eat_kwargs(pet)
    )

    embed, _ = build_death_embed(dead)
    assert "was eaten" in embed.description
    assert "starved" not in embed.description


def test_petless_memorial_uses_eaten_copy(pet_repo):
    from commands.pet_helpers.embeds import _build_petless_embed
    from domain.models.pet import PetStatus

    pet = make_pet(pet_repo)
    dead, _, _ = pet_repo.eat_adult_pet_atomic(
        100, TEST_GUILD_ID, **eat_kwargs(pet)
    )

    embed, _ = _build_petless_embed(
        PetStatus(pet=None, last_dead=dead), owner_name="Owner", next_fee=50
    )
    assert "was eaten" in embed.description
    assert "died of eaten" not in embed.description
