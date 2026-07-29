"""Narrow orchestration boundary for Camagotchi upbringing and evolution."""

from __future__ import annotations

import logging
import time

from domain.models.pet import EvolutionNotice, Pet
from domain.pet_evolution import (
    PetActivity,
    PetCalling,
    hint_key_for_scores,
    resolve_evolution,
)

logger = logging.getLogger("cama_bot.services.pet_evolution")


class PetEvolutionService:
    def __init__(self, evolution_repo, pet_repo):
        self.evolution_repo = evolution_repo
        self.pet_repo = pet_repo

    def _now(self) -> int:
        return int(time.time())

    def record_activity(
        self,
        discord_id: int,
        guild_id: int | None,
        activity: PetActivity,
        source_key: str,
        *,
        occurred_at: int | None = None,
    ) -> int:
        """Best-effort activity capture; pet scoring never breaks gameplay."""
        occurred_at = self._now() if occurred_at is None else int(occurred_at)
        try:
            return self.evolution_repo.record_activity(
                discord_id,
                guild_id,
                activity=PetActivity(activity),
                source_key=source_key,
                occurred_at=occurred_at,
            )
        except Exception:
            logger.exception(
                "Pet evolution activity capture failed for %s",
                PetActivity(activity).value,
            )
            return 0

    def hint_for(self, pet: Pet) -> str | None:
        if pet.evolved_at is not None or pet.evolution_due_at is None:
            return None
        scores = self.evolution_repo.get_scores(pet.pet_id, pet.guild_id)
        return hint_key_for_scores(scores)

    def resolve_if_due(self, pet: Pet, *, alive_through: int) -> Pet:
        """Resolve once if the pet was alive strictly through its due moment."""
        due_at = pet.evolution_due_at
        if pet.evolved_at is not None or due_at is None or due_at > alive_through:
            return pet

        self.evolution_repo.claim_evolution(
            pet.pet_id,
            pet.guild_id,
            expected_due_at=due_at,
            decide=lambda scores: resolve_evolution(scores, pet_id=pet.pet_id),
        )
        return self.pet_repo.get_pet_by_id(pet.pet_id, pet.guild_id) or pet

    def find_due(self, now: int, *, limit: int = 100) -> list[Pet]:
        pets: list[Pet] = []
        for pet_id, guild_id in self.evolution_repo.find_due_refs(now, limit=limit):
            pet = self.pet_repo.get_pet_by_id(pet_id, guild_id)
            if pet is not None:
                pets.append(pet)
        return pets

    def get_unannounced(self, *, limit: int = 100) -> list[EvolutionNotice]:
        notices: list[EvolutionNotice] = []
        for pet_id, guild_id in self.evolution_repo.find_unannounced_refs(
            limit=limit
        ):
            pet = self.pet_repo.get_pet_by_id(pet_id, guild_id)
            if pet is not None:
                notices.append(EvolutionNotice(pet=pet))
        return notices

    def callingdex(
        self,
        discord_id: int,
        guild_id: int | None,
    ) -> tuple[list[PetCalling], int]:
        raised = [
            PetCalling(value)
            for value in self.evolution_repo.callings_raised(
                discord_id,
                guild_id,
            )
        ]
        return raised, len(PetCalling)

    def mark_announced(self, pet: Pet) -> None:
        self.evolution_repo.mark_announced(
            pet.pet_id,
            pet.guild_id,
            self._now(),
        )
