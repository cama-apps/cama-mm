"""Service layer for pet brawls.

Synchronous Result-returning methods (commands wrap them in
asyncio.to_thread). The battle itself is round-resolved in the command
layer straight through domain.pet_brawl; this service owns the persisted
lifecycle (challenge/accept/decline/void), the pet snapshots that become
duelists, and the hunger settlement.

Randomness is constructor-injected so tests can pin every roll.
"""

from __future__ import annotations

import logging
import random

from domain.models.pet import Pet, PetMood
from domain.pet_brawl import Duelist, PetBrawlState, build_duelist, initial_state
from domain.pet_constants import (
    BRAWL_INSURANCE_REDUCTION,
    BRAWL_LOSS_FLOOR,
    BRAWL_LOSS_HUNGER,
    BRAWL_WIN_HUNGER,
    BRAWL_WIN_HUNGER_DAILY_CAP,
    PET_BRAWL_ACCEPT_SECONDS,
    PET_BRAWL_ACTIVE_TTL_SECONDS,
    get_species,
)
from services import error_codes
from services.result import Result

logger = logging.getLogger("cama_bot.services.pet_brawl")

_REPO_ERRORS = {
    "brawl_busy": (
        "One of you is already in a pet brawl. Finish that first.",
        error_codes.BRAWL_BUSY,
    ),
    "no_brawl": ("That brawl no longer exists.", error_codes.NOT_FOUND),
    "not_recipient": (
        "This challenge isn't yours to answer.",
        error_codes.VALIDATION_ERROR,
    ),
    "expired": ("That challenge has expired.", error_codes.NOT_FOUND),
    "not_pending": (
        "That challenge has already been answered.",
        error_codes.VALIDATION_ERROR,
    ),
    "not_open": ("That brawl is already over.", error_codes.VALIDATION_ERROR),
    "not_active": ("That brawl isn't underway.", error_codes.VALIDATION_ERROR),
    "already_done": (
        "That brawl was already settled.",
        error_codes.VALIDATION_ERROR,
    ),
}


class PetBrawlService:
    def __init__(self, pet_service, pet_brawl_repo, *, rng: random.Random | None = None):
        self.pet_service = pet_service
        self.pet_brawl_repo = pet_brawl_repo
        self._rng = rng or random.Random()

    # Patchable seam, mirroring PetService._now.
    def _now(self) -> int:
        return self.pet_service._now()

    def _map_repo_error(self, exc: ValueError) -> Result:
        message, code = _REPO_ERRORS.get(
            str(exc), ("Something went wrong with that brawl.", error_codes.VALIDATION_ERROR)
        )
        return Result.fail(message, code=code)

    # --- pet snapshots ---

    def _battle_ready_pet(
        self, discord_id: int, guild_id: int | None, now: int, *, whose: str
    ) -> Result[Pet]:
        pet = self.pet_service._living_pet(discord_id, guild_id, now)
        if pet is None:
            return Result.fail(
                f"{whose} pet to brawl with — adopt one with /pet adopt."
                if whose == "You have no"
                else f"{whose} has no living pet to brawl with.",
                code=error_codes.NO_PET,
            )
        if now < pet.hatched_at:
            return Result.fail(
                f"**{pet.name}** is still an egg. Eggs don't brawl.",
                code=error_codes.PET_EGG,
            )
        return Result.ok(pet)

    def _to_duelist(self, pet: Pet, now: int) -> Duelist:
        return build_duelist(
            pet_id=pet.pet_id,
            owner_id=pet.discord_id,
            name=pet.name,
            species_id=pet.species,
            stage=pet.stage(now),
            hunger=pet.current_hunger(now, self.pet_service.decay_per_day),
            happy=pet.mood(now, self.pet_service.decay_per_day) is PetMood.HAPPY,
            aegis_used=pet.aegis_used,
        )

    # --- lifecycle ---

    def challenge(
        self,
        challenger_id: int,
        recipient_id: int,
        guild_id: int | None,
        channel_id: int,
    ) -> Result[dict]:
        if challenger_id == recipient_id:
            return Result.fail(
                "Your cama refuses to fight itself.", code=error_codes.VALIDATION_ERROR
            )
        now = self._now()
        challenger_pet = self._battle_ready_pet(
            challenger_id, guild_id, now, whose="You have no"
        )
        if not challenger_pet:
            return challenger_pet
        recipient_pet = self._battle_ready_pet(
            recipient_id, guild_id, now, whose="Your opponent"
        )
        if not recipient_pet:
            return recipient_pet
        try:
            brawl = self.pet_brawl_repo.create_brawl_atomic(
                guild_id,
                channel_id,
                challenger_id,
                recipient_id,
                challenger_pet.value.pet_id,
                now=now,
                expires_at=now + PET_BRAWL_ACCEPT_SECONDS,
            )
        except ValueError as exc:
            return self._map_repo_error(exc)
        return Result.ok(
            {
                "brawl": brawl,
                "challenger_pet": challenger_pet.value,
                "recipient_pet": recipient_pet.value,
            }
        )

    def accept(self, brawl_id: int, guild_id: int | None, recipient_id: int) -> Result[dict]:
        """Validate both pets, activate the row, and build the opening state."""
        now = self._now()
        brawl = self.pet_brawl_repo.get_brawl(brawl_id, guild_id)
        if brawl is None:
            return Result.fail("That brawl no longer exists.", code=error_codes.NOT_FOUND)
        challenger_pet = self.pet_service.pet_repo.get_pet_by_id(
            brawl.challenger_pet_id, guild_id
        )
        if challenger_pet is not None:
            challenger_pet = self.pet_service._resolve_starvation(challenger_pet, now)
        if challenger_pet is None:
            # The challenger's pet died while the challenge sat open.
            try:
                self.pet_brawl_repo.void_atomic(brawl_id, guild_id, now)
            except ValueError:
                pass
            return Result.fail(
                "The challenger's pet is no longer with us. Challenge voided.",
                code=error_codes.PET_DEAD,
            )
        recipient_pet = self._battle_ready_pet(
            recipient_id, guild_id, now, whose="You have no"
        )
        if not recipient_pet:
            return recipient_pet
        try:
            brawl = self.pet_brawl_repo.accept_atomic(
                brawl_id, guild_id, recipient_id, recipient_pet.value.pet_id, now
            )
        except ValueError as exc:
            return self._map_repo_error(exc)
        seed = self._rng.getrandbits(64)
        state = initial_state(
            self._to_duelist(challenger_pet, now),
            self._to_duelist(recipient_pet.value, now),
        )
        return Result.ok(
            {
                "brawl": brawl,
                "state": state,
                "rng": random.Random(seed),
                "seed": seed,
            }
        )

    def decline(
        self, brawl_id: int, guild_id: int | None, recipient_id: int
    ) -> Result[None]:
        try:
            self.pet_brawl_repo.decline_atomic(
                brawl_id, guild_id, recipient_id, self._now()
            )
        except ValueError as exc:
            return self._map_repo_error(exc)
        return Result.ok()

    def withdraw(
        self, brawl_id: int, guild_id: int | None, challenger_id: int
    ) -> Result[None]:
        brawl = self.pet_brawl_repo.get_brawl(brawl_id, guild_id)
        if brawl is None or brawl.challenger_id != challenger_id:
            return Result.fail(
                "Only the challenger can withdraw.", code=error_codes.VALIDATION_ERROR
            )
        if brawl.status != "pending":
            return Result.fail(
                "That challenge has already been answered.",
                code=error_codes.VALIDATION_ERROR,
            )
        return self.void(brawl_id, guild_id)

    def void(self, brawl_id: int, guild_id: int | None) -> Result[None]:
        try:
            self.pet_brawl_repo.void_atomic(brawl_id, guild_id, self._now())
        except ValueError as exc:
            return self._map_repo_error(exc)
        return Result.ok()

    # --- settlement ---

    def settle(
        self,
        brawl_id: int,
        guild_id: int | None,
        final_state: PetBrawlState,
        rounds: int,
    ) -> Result[dict]:
        if final_state.winner not in ("a", "b"):
            return Result.fail(
                "That brawl has no winner yet.", code=error_codes.VALIDATION_ERROR
            )
        winner = final_state.a if final_state.winner == "a" else final_state.b
        loser = final_state.b if final_state.winner == "a" else final_state.a
        winner_species = get_species(winner.species_id)
        loser_species = get_species(loser.species_id)
        winner_gain = BRAWL_WIN_HUNGER + winner_species.match_feed_bonus
        loser_loss = BRAWL_LOSS_HUNGER - (
            BRAWL_INSURANCE_REDUCTION if loser_species.refund_bonus_pp > 0 else 0
        )
        try:
            settlement = self.pet_brawl_repo.settle_brawl_atomic(
                brawl_id,
                guild_id,
                winner_id=winner.owner_id,
                winner_pet_id=winner.pet_id,
                loser_pet_id=loser.pet_id,
                rounds=rounds,
                now=self._now(),
                decay_per_day=self.pet_service.decay_per_day,
                winner_gain=winner_gain,
                loser_loss=loser_loss,
                loss_floor=BRAWL_LOSS_FLOOR,
                daily_win_cap=BRAWL_WIN_HUNGER_DAILY_CAP,
            )
        except ValueError as exc:
            return self._map_repo_error(exc)
        records = self.pet_brawl_repo.get_records_for(
            [winner.pet_id, loser.pet_id], guild_id
        )
        return Result.ok(
            {
                "winner": winner,
                "loser": loser,
                "winner_delta": settlement["winner_delta"],
                "loser_delta": settlement["loser_delta"],
                "records": records,
            }
        )

    # --- maintenance / reads ---

    def sweep_stale(self) -> dict[str, int]:
        return self.pet_brawl_repo.sweep_stale(
            self._now(), active_ttl_seconds=PET_BRAWL_ACTIVE_TTL_SECONDS
        )

    def record(self, pet_id: int, guild_id: int | None) -> tuple[int, int]:
        return self.pet_brawl_repo.get_pet_record(pet_id, guild_id)

    def records_for(
        self, pet_ids: list[int], guild_id: int | None
    ) -> dict[int, tuple[int, int]]:
        return self.pet_brawl_repo.get_records_for(pet_ids, guild_id)
