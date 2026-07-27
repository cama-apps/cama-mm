"""Tests for the altar: sacrifice-and-reroll across repo, service, command."""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from domain.pet_constants import (
    ADOPTION_FEES,
    EGG_HATCH_SECONDS,
    SACRIFICE_TIER_WEIGHTS,
)
from repositories.pet_brawl_repository import PetBrawlRepository
from repositories.pet_repository import PetRepository
from repositories.player_repository import PlayerRepository
from services import error_codes
from services.pet_service import PetService
from tests.conftest import TEST_GUILD_ID

T0 = 1_800_000_000
DAY = 86400
HATCH = EGG_HATCH_SECONDS


class SteerableClock:
    def __init__(self, now: int = T0):
        self.now = now

    def __call__(self) -> int:
        return self.now


@pytest.fixture
def clock():
    return SteerableClock()


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
def service(repo_db_path, pet_repo, player_repo, clock, monkeypatch):
    svc = PetService(pet_repo, player_repo, decay_per_day=20)
    monkeypatch.setattr(svc, "_now", clock)
    return svc


def balance(player_repo):
    return player_repo.get_balance(100, TEST_GUILD_ID)


def make_living_pet(pet_repo, clock, *, species="common_cama", fee=0):
    """Adopt directly and steer the clock past hatch."""
    pet = pet_repo.adopt_pet(
        100,
        TEST_GUILD_ID,
        name="Blep",
        species=species,
        fee=fee,
        now=clock.now,
        hatch_seconds=HATCH,
    )
    clock.now = pet.hatched_at + 1
    return pet_repo.get_pet_by_id(pet.pet_id, TEST_GUILD_ID)


def sacrifice_kwargs(pet, clock, **overrides):
    kwargs = {
        "old_pet_id": pet.pet_id,
        "expected_last_fed_at": pet.last_fed_at,
        "expected_hunger": pet.hunger_at_last_fed,
        "name": "Rebirth",
        "species": "rama",
        "fee": 50,
        "now": clock.now,
        "hatch_seconds": HATCH,
    }
    kwargs.update(overrides)
    return kwargs


class TestRepository:
    def test_happy_path_is_one_transaction(self, pet_repo, player_repo, clock):
        pet = make_living_pet(pet_repo, clock)
        dead, egg = pet_repo.sacrifice_and_adopt_atomic(
            100, TEST_GUILD_ID, **sacrifice_kwargs(pet, clock)
        )
        assert dead.died_at == clock.now
        assert dead.death_cause == "sacrifice"
        # At-least-once: announced only after the altar rite actually posts
        # (the command marks it); until then the sweep can retry.
        assert dead.death_announced_at is None
        assert egg.species == "rama"
        assert egg.hatched_at == clock.now + HATCH
        assert egg.hunger_at_last_fed == 100
        assert balance(player_repo) == 950
        assert pet_repo.get_active_pet(100, TEST_GUILD_ID).pet_id == egg.pet_id

    def test_insufficient_funds_rolls_back_the_death(
        self, pet_repo, player_repo, clock
    ):
        pet = make_living_pet(pet_repo, clock)
        with pytest.raises(ValueError, match="insufficient_funds"):
            pet_repo.sacrifice_and_adopt_atomic(
                100, TEST_GUILD_ID, **sacrifice_kwargs(pet, clock, fee=5000)
            )
        alive = pet_repo.get_active_pet(100, TEST_GUILD_ID)
        assert alive is not None and alive.pet_id == pet.pet_id
        assert balance(player_repo) == 1000

    def test_anchor_race_raises_stale_pet(self, pet_repo, clock):
        pet = make_living_pet(pet_repo, clock)
        with pytest.raises(ValueError, match="stale_pet"):
            pet_repo.sacrifice_and_adopt_atomic(
                100,
                TEST_GUILD_ID,
                **sacrifice_kwargs(pet, clock, expected_hunger=42),
            )

    def test_dead_and_missing_pets_rejected(self, pet_repo, clock):
        pet = make_living_pet(pet_repo, clock)
        pet_repo.claim_death(
            pet.pet_id,
            TEST_GUILD_ID,
            died_at=clock.now,
            expected_last_fed_at=pet.last_fed_at,
            expected_hunger=pet.hunger_at_last_fed,
        )
        with pytest.raises(ValueError, match="pet_dead"):
            pet_repo.sacrifice_and_adopt_atomic(
                100, TEST_GUILD_ID, **sacrifice_kwargs(pet, clock)
            )
        with pytest.raises(ValueError, match="no_pet"):
            pet_repo.sacrifice_and_adopt_atomic(
                100, TEST_GUILD_ID, **sacrifice_kwargs(pet, clock, old_pet_id=9999)
            )

    def test_mid_brawl_guard(self, repo_db_path, pet_repo, clock):
        pet = make_living_pet(pet_repo, clock)
        brawl_repo = PetBrawlRepository(repo_db_path)
        brawl_repo.create_brawl_atomic(
            TEST_GUILD_ID, 5, 100, 200, pet.pet_id,
            now=clock.now, expires_at=clock.now + 180,
        )
        with pytest.raises(ValueError, match="in_brawl"):
            pet_repo.sacrifice_and_adopt_atomic(
                100, TEST_GUILD_ID, **sacrifice_kwargs(pet, clock)
            )


class TestService:
    def test_rejects_petless_and_egg(self, service, pet_repo, clock):
        result = service.sacrifice(100, TEST_GUILD_ID, "Rebirth")
        assert result.error_code == error_codes.NO_PET
        pet_repo.adopt_pet(
            100, TEST_GUILD_ID, name="Eggy", species="common_cama",
            fee=0, now=clock.now, hatch_seconds=HATCH,
        )
        result = service.sacrifice(100, TEST_GUILD_ID, "Rebirth")
        assert result.error_code == error_codes.PET_EGG
        assert "mystery" in result.error

    def test_rejects_bad_name(self, service, pet_repo, clock):
        make_living_pet(pet_repo, clock)
        result = service.sacrifice(100, TEST_GUILD_ID, "join discord.gg/scam")
        assert result.error_code == error_codes.VALIDATION_ERROR

    def test_fee_counts_the_sacrificed_pet(self, service, pet_repo, clock):
        make_living_pet(pet_repo, clock)
        preview = service.sacrifice_preview(100, TEST_GUILD_ID)
        # 0 prior deaths + the pet on the altar -> second fee tier, not first.
        assert preview.value["fee"] == ADOPTION_FEES[1]
        result = service.sacrifice(100, TEST_GUILD_ID, "Rebirth")
        assert result.success, result.error
        assert result.value["fee"] == ADOPTION_FEES[1]

    def test_weights_follow_tier_and_stage(self, service, pet_repo, clock):
        make_living_pet(pet_repo, clock, species="invoker_cama")
        # One hour post-hatch: still a baby.
        preview = service.sacrifice_preview(100, TEST_GUILD_ID)
        assert preview.value["weights"] == SACRIFICE_TIER_WEIGHTS[("rare", False)]
        # Keep it fed through to adulthood (top-up at day 4, check at day 8).
        pet = preview.value["pet"]
        clock.now += 4 * DAY
        assert service.pet_repo.apply_hunger_top_up(
            pet.pet_id, TEST_GUILD_ID,
            expected_last_fed_at=pet.last_fed_at,
            expected_hunger=pet.hunger_at_last_fed,
            new_last_fed_at=clock.now, new_hunger=100,
        )
        clock.now += 4 * DAY  # 8 days post-hatch: adult, hunger 20, alive
        preview = service.sacrifice_preview(100, TEST_GUILD_ID)
        assert preview.value["weights"] == SACRIFICE_TIER_WEIGHTS[("rare", True)]
        with patch.object(
            service, "_roll_species_tiered", return_value="rama"
        ) as roll:
            result = service.sacrifice(100, TEST_GUILD_ID, "Rebirth")
        assert result.success, result.error
        roll.assert_called_once_with(SACRIFICE_TIER_WEIGHTS[("rare", True)])
        assert result.value["new_pet"].species == "rama"

    def test_sacrifice_excluded_from_pity(self, service, pet_repo, clock):
        # Three commons starve honestly.
        for i in range(3):
            with patch(
                "services.pet_service.random.choices",
                return_value=["common_cama"],
            ):
                adopted = service.adopt(100, TEST_GUILD_ID, f"Common {i}")
            assert adopted.success, adopted.error
            clock.now = adopted.value["pet"].hatched_at + 9 * DAY
            assert service._living_pet(100, TEST_GUILD_ID, clock.now) is None
        assert service._pity_active(100, TEST_GUILD_ID)
        # A sacrifice in between must neither count toward nor reset pity.
        # (Pity is active, so this adopt rolls a TIER first, then a species.)
        with patch(
            "services.pet_service.random.choices", return_value=["uncommon"]
        ):
            adopted = service.adopt(100, TEST_GUILD_ID, "Altar Fodder")
        clock.now = adopted.value["pet"].hatched_at + 1
        with patch.object(
            service, "_roll_species_tiered", return_value="banana_ears"
        ):
            result = service.sacrifice(100, TEST_GUILD_ID, "Rebirth")
        assert result.success, result.error
        recent = pet_repo.recent_dead_species(100, TEST_GUILD_ID, 10)
        assert len(recent) == 3  # the sacrificed pet is filtered out
        assert service._pity_active(100, TEST_GUILD_ID)
        # But it DOES escalate the fee: 4 dead total + next one on the altar.
        assert pet_repo.count_dead_pets(100, TEST_GUILD_ID) == 4

    def test_blocked_while_in_brawl(self, service, repo_db_path, pet_repo, clock):
        pet = make_living_pet(pet_repo, clock)
        PetBrawlRepository(repo_db_path).create_brawl_atomic(
            TEST_GUILD_ID, 5, 100, 200, pet.pet_id,
            now=clock.now, expires_at=clock.now + 180,
        )
        result = service.sacrifice(100, TEST_GUILD_ID, "Rebirth")
        assert result.error_code == error_codes.BRAWL_BUSY


class AutoConfirmView:
    """Stands in for ConfirmSacrificeView so command tests don't wait 60s."""

    answer = True

    def __init__(self, owner_id, **kwargs):
        self.owner_id = owner_id
        self.value = self.answer
        self.message = None

    async def wait(self):
        return


@pytest.fixture
def altar_cog(repo_db_path, service, monkeypatch):
    from commands.pet import PetCommands
    from services.pet_brawl_service import PetBrawlService

    monkeypatch.setattr(
        "commands.pet.GLOBAL_RATE_LIMITER.check",
        lambda **kwargs: MagicMock(allowed=True, retry_after_seconds=0),
    )
    cog = PetCommands.__new__(PetCommands)
    cog.bot = MagicMock()
    cog.bot.reminder_service = None
    cog.pet_service = service
    cog.pet_brawl_service = PetBrawlService(
        service, PetBrawlRepository(repo_db_path)
    )
    cog._brawl_sessions = {}
    return cog


def make_interaction(user_id=100):
    inter = MagicMock()
    inter.user.id = user_id
    inter.user.display_name = "Owner"
    inter.guild.id = TEST_GUILD_ID
    inter.response.defer = AsyncMock()
    inter.response.send_message = AsyncMock()
    inter.followup.send = AsyncMock(return_value=MagicMock(edit=AsyncMock()))
    return inter


class TestAltarCommand:
    @pytest.mark.asyncio
    async def test_confirm_performs_the_rite(
        self, altar_cog, pet_repo, clock, monkeypatch
    ):
        old = make_living_pet(pet_repo, clock)
        monkeypatch.setattr("commands.pet.ConfirmSacrificeView", AutoConfirmView)
        inter = make_interaction()
        await altar_cog.altar.callback(altar_cog, inter, "Rebirth")
        new_pet = pet_repo.get_active_pet(100, TEST_GUILD_ID)
        assert new_pet is not None and new_pet.name == "Rebirth"
        message = inter.followup.send.return_value
        edit_kwargs = message.edit.await_args.kwargs
        assert "altar" in edit_kwargs["embed"].title.lower()
        assert edit_kwargs["view"] is None
        # Delivered farewell -> death marked announced (no sweep duplicate).
        dead = pet_repo.get_pet_by_id(old.pet_id, TEST_GUILD_ID)
        assert dead.death_announced_at is not None

    @pytest.mark.asyncio
    async def test_failed_edit_falls_back_and_still_marks(
        self, altar_cog, pet_repo, clock, monkeypatch
    ):
        import discord

        old = make_living_pet(pet_repo, clock)
        monkeypatch.setattr("commands.pet.ConfirmSacrificeView", AutoConfirmView)
        inter = make_interaction()
        message = inter.followup.send.return_value
        message.edit = AsyncMock(
            side_effect=discord.HTTPException(MagicMock(status=404), "gone")
        )
        await altar_cog.altar.callback(altar_cog, inter, "Rebirth")
        # Fallback path posted (followup.send called again with the embed).
        assert inter.followup.send.await_count >= 2
        dead = pet_repo.get_pet_by_id(old.pet_id, TEST_GUILD_ID)
        assert dead.death_announced_at is not None

    def test_sweep_tombstone_names_the_altar(self, pet_repo, clock):
        from commands.pet_helpers.embeds import build_death_embed

        pet = make_living_pet(pet_repo, clock)
        dead, _ = pet_repo.sacrifice_and_adopt_atomic(
            100, TEST_GUILD_ID, **sacrifice_kwargs(pet, clock)
        )
        embed, _ = build_death_embed(dead)
        assert "altar" in embed.description
        assert "starved" not in embed.description

    @pytest.mark.asyncio
    async def test_cancel_keeps_the_pet(
        self, altar_cog, pet_repo, clock, monkeypatch
    ):
        pet = make_living_pet(pet_repo, clock)

        class Decline(AutoConfirmView):
            answer = False

        monkeypatch.setattr("commands.pet.ConfirmSacrificeView", Decline)
        inter = make_interaction()
        await altar_cog.altar.callback(altar_cog, inter, "Rebirth")
        alive = pet_repo.get_active_pet(100, TEST_GUILD_ID)
        assert alive is not None and alive.pet_id == pet.pet_id
        message = inter.followup.send.return_value
        assert "cold" in message.edit.await_args.kwargs["embed"].title.lower()
