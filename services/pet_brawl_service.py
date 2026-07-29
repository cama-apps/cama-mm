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
import math
import random

from config import TIP_FEE_RATE
from domain.models.pet import Pet, PetMood
from domain.pet_brawl import (
    Duelist,
    PetBrawlState,
    brawl_traits,
    build_duelist,
    initial_state,
)
from domain.pet_constants import (
    BRAWL_INSURANCE_REDUCTION,
    BRAWL_LOSS_FLOOR,
    BRAWL_LOSS_HUNGER,
    BRAWL_LOSS_XP,
    BRAWL_TRAINING_LEVEL_CAP,
    BRAWL_TRAINING_STAT_CAP,
    BRAWL_TRAINING_XP_CAP,
    BRAWL_WIN_HUNGER,
    BRAWL_WIN_HUNGER_DAILY_CAP,
    BRAWL_WIN_XP,
    BRAWL_XP_PER_LEVEL,
    PET_BRAWL_ACCEPT_SECONDS,
    PET_BRAWL_ACTIVE_TTL_SECONDS,
    SOLO_TRAINING_XP_PER_SESSION,
    get_species,
)
from domain.pet_evolution import PetActivity
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
    "not_challenger": (
        "Only the challenger can withdraw.",
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
    "challenger_funds": (
        "You cannot cover that wager and fee.",
        error_codes.INSUFFICIENT_FUNDS,
    ),
    "recipient_funds": (
        "Your opponent cannot cover that wager.",
        error_codes.INSUFFICIENT_FUNDS,
    ),
    "invalid_wager": (
        "Pet-brawl wagers must be whole numbers from 0 to 100 JC.",
        error_codes.VALIDATION_ERROR,
    ),
    "invalid_fee": (
        "That pet-brawl fee is invalid.",
        error_codes.VALIDATION_ERROR,
    ),
    "participant_mismatch": (
        "Those pets do not match the accepted brawl.",
        error_codes.VALIDATION_ERROR,
    ),
    "no_pet": ("You have no living pet to train.", error_codes.NO_PET),
    "pet_dead": ("Your pet has passed away.", error_codes.PET_DEAD),
    "pet_egg": ("Eggs need to hatch before they can train.", error_codes.PET_EGG),
    "in_brawl": (
        "Your cama has an open brawl—settle that first.",
        error_codes.BRAWL_BUSY,
    ),
    "no_training_sessions": (
        "No solo sessions are ready yet. Another arrives next game day.",
        error_codes.COOLDOWN_ACTIVE,
    ),
    "training_complete": (
        "Your cama has completed its permanent training.",
        error_codes.VALIDATION_ERROR,
    ),
}

_BUILD_TITLES = {"str": "Bruiser", "int": "Tactician", "dex": "Trickster"}
_PERSONALITY_EVENT_COPY = {
    "milestone.rookie": "🌟 A first real brawl leaves a new Rookie in the ring.",
    "milestone.veteran": "🎖️ Another hard-fought bout earns a Veteran's composure.",
    "milestone.champion": "🏆 Ten wins. The pet-brawl circuit has a new Champion.",
    "milestone.build": "🌠 Fully trained—this cama's battle style has taken shape.",
    "str.victory": "💪 **{winner}** paws the ground, already asking for heavier opposition.",
    "int.victory": "🧠 **{winner}** studies the replay like the result was calculated.",
    "dex.victory": "💨 **{winner}** celebrates by dodging an imaginary rematch.",
    "balanced.victory": "✨ **{winner}** looks pleased with a thoroughly well-rounded showing.",
    "defiant.loss": "🔥 **{loser}** refuses to call that a loss—only research.",
}
_SOLO_TRAINING_COPY = (
    "🏃 **{pet}** tears through a dusty obstacle course.",
    "🎯 **{pet}** practices until the training dummy looks nervous.",
    "🧩 **{pet}** studies an increasingly complicated pile of snacks.",
)


class PetBrawlService:
    def __init__(
        self,
        pet_service,
        pet_brawl_repo,
        *,
        rng: random.Random | None = None,
        pet_evolution_service=None,
    ):
        self.pet_service = pet_service
        self.pet_brawl_repo = pet_brawl_repo
        self._rng = rng or random.Random()
        self.pet_evolution_service = pet_evolution_service

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
        pet = self.pet_service.living_pet(discord_id, guild_id, now)
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
            training_str=pet.training_str,
            training_int=pet.training_int,
            training_dex=pet.training_dex,
        )

    @staticmethod
    def career_summary(pet: Pet, *, wins: int, losses: int) -> dict:
        total = wins + losses
        if wins >= 10:
            rank = "Champion"
        elif total >= 10:
            rank = "Veteran"
        elif total >= 1:
            rank = "Rookie"
        else:
            rank = None
        build = None
        if pet.training_xp >= BRAWL_TRAINING_XP_CAP:
            values = {
                "str": pet.training_str,
                "int": pet.training_int,
                "dex": pet.training_dex,
            }
            highest = max(values.values())
            leaders = [stat for stat, value in values.items() if value == highest]
            build = (
                _BUILD_TITLES[leaders[0]]
                if len(leaders) == 1
                else "All-Rounder"
            )
        return {
            "xp": pet.training_xp,
            "str": pet.training_str,
            "int": pet.training_int,
            "dex": pet.training_dex,
            "rank": rank,
            "build": build,
        }

    def _choose_stat_gain(self, pet: Pet, award: int) -> str | None:
        if (
            pet.training_xp >= BRAWL_TRAINING_XP_CAP
            or pet.training_xp // BRAWL_XP_PER_LEVEL
            == min(
                BRAWL_TRAINING_XP_CAP,
                pet.training_xp + award,
            )
            // BRAWL_XP_PER_LEVEL
        ):
            return None
        stats = {
            "str": pet.training_str,
            "int": pet.training_int,
            "dex": pet.training_dex,
        }
        if sum(stats.values()) >= BRAWL_TRAINING_LEVEL_CAP:
            return None
        eligible = [
            stat
            for stat, value in stats.items()
            if value < BRAWL_TRAINING_STAT_CAP
        ]
        return self._rng.choice(eligible) if eligible else None

    @staticmethod
    def _record_milestone(
        winner_record: tuple[int, int],
        loser_record: tuple[int, int],
    ) -> str | None:
        if winner_record[0] == 9:
            return "milestone.champion"
        if sum(winner_record) == 9 or sum(loser_record) == 9:
            return "milestone.veteran"
        if sum(winner_record) == 0 or sum(loser_record) == 0:
            return "milestone.rookie"
        return None

    def _personality_event_key(
        self,
        winner: Duelist,
        loser: Duelist,
        milestone_key: str | None,
    ) -> str | None:
        if milestone_key is not None:
            return milestone_key
        if self._rng.random() >= 0.20:
            return None
        stats = {
            "str": winner.training_str,
            "int": winner.training_int,
            "dex": winner.training_dex,
        }
        highest = max(stats.values())
        leaders = [stat for stat, value in stats.items() if value == highest]
        winner_key = (
            f"{leaders[0]}.victory"
            if len(leaders) == 1 and highest > 0
            else "balanced.victory"
        )
        return self._rng.choice((winner_key, "defiant.loss"))

    @staticmethod
    def personality_event_text(
        event_key: str | None,
        winner: Duelist,
        loser: Duelist,
    ) -> str | None:
        template = _PERSONALITY_EVENT_COPY.get(event_key)
        return (
            template.format(winner=winner.name, loser=loser.name)
            if template
            else None
        )

    def train_solo(
        self, discord_id: int, guild_id: int | None
    ) -> Result[dict]:
        now = self._now()
        pet = self.pet_service.living_pet(discord_id, guild_id, now)
        if pet is None:
            return Result.fail(
                "You have no living pet to train.",
                code=error_codes.NO_PET,
            )
        if now < pet.hatched_at:
            return Result.fail(
                "Eggs need to hatch before they can train.",
                code=error_codes.PET_EGG,
            )
        available = self.pet_service.pet_repo.available_solo_training_sessions(
            pet, now
        )
        award = min(
            available * SOLO_TRAINING_XP_PER_SESSION,
            BRAWL_TRAINING_XP_CAP - pet.training_xp,
        )
        proposed_stat = self._choose_stat_gain(pet, award)
        try:
            outcome = self.pet_service.pet_repo.train_solo_atomic(
                pet.pet_id,
                guild_id,
                now=now,
                proposed_stat=proposed_stat,
            )
        except ValueError as exc:
            return self._map_repo_error(exc)
        updated = self.pet_service.pet_repo.get_pet_by_id(pet.pet_id, guild_id)
        wins, losses = self.pet_brawl_repo.get_pet_record(pet.pet_id, guild_id)
        return Result.ok(
            {
                **outcome,
                "pet": updated,
                "career": self.career_summary(
                    updated,
                    wins=wins,
                    losses=losses,
                ),
                "flavor": self._rng.choice(_SOLO_TRAINING_COPY).format(
                    pet=updated.name
                ),
            }
        )

    # --- lifecycle ---

    def challenge(
        self,
        challenger_id: int,
        recipient_id: int,
        guild_id: int | None,
        channel_id: int,
        wager: int = 0,
    ) -> Result[dict]:
        if type(wager) is not int or not 0 <= wager <= 100:
            return Result.fail(
                "Pet-brawl wagers must be whole numbers from 0 to 100 JC.",
                code=error_codes.VALIDATION_ERROR,
            )
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
            fee = max(1, math.ceil(wager * TIP_FEE_RATE)) if wager else 0
            brawl = self.pet_brawl_repo.create_brawl_atomic(
                guild_id,
                channel_id,
                challenger_id,
                recipient_id,
                challenger_pet.value.pet_id,
                now=now,
                expires_at=now + PET_BRAWL_ACCEPT_SECONDS,
                wager=wager,
                fee=fee,
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
        challenger_pet = self.pet_service.living_pet_by_id(
            brawl.challenger_pet_id, guild_id, now
        )
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
        challenger_pet = self.pet_service.living_pet_by_id(
            brawl.challenger_pet_id, guild_id, now
        )
        recipient_pet = self.pet_service.living_pet_by_id(
            brawl.recipient_pet_id, guild_id, now
        )
        if challenger_pet is None or recipient_pet is None:
            try:
                self.pet_brawl_repo.void_atomic(brawl_id, guild_id, now)
            except ValueError:
                pass
            return Result.fail(
                "One of those pets is no longer ready. Brawl voided.",
                code=error_codes.PET_DEAD,
            )
        state = initial_state(
            self._to_duelist(challenger_pet, now),
            self._to_duelist(recipient_pet, now),
        )
        return Result.ok(
            {
                "brawl": brawl,
                "state": state,
                "rng": random.Random(self._rng.getrandbits(64)),
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
        try:
            self.pet_brawl_repo.withdraw_atomic(
                brawl_id, guild_id, challenger_id, self._now()
            )
        except ValueError as exc:
            return self._map_repo_error(exc)
        return Result.ok()

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
        if final_state.winner not in ("a", "b", "draw"):
            return Result.fail(
                "That brawl has no winner yet.", code=error_codes.VALIDATION_ERROR
            )
        if final_state.winner == "draw":
            duelists = (final_state.a, final_state.b)
            now = self._now()
            try:
                settlement = self.pet_brawl_repo.settle_draw_atomic(
                    brawl_id,
                    guild_id,
                    participant_pet_ids=(
                        final_state.a.pet_id,
                        final_state.b.pet_id,
                    ),
                    rounds=rounds,
                    now=now,
                )
            except ValueError as exc:
                return self._map_repo_error(exc)
            records = self.pet_brawl_repo.get_records_for(
                [d.pet_id for d in duelists], guild_id
            )
            return Result.ok(
                {
                    "draw": True,
                    "duelists": duelists,
                    "records": records,
                    "wager": settlement["wager"],
                    "fee": settlement["fee"],
                }
            )
        winner = final_state.a if final_state.winner == "a" else final_state.b
        loser = final_state.b if final_state.winner == "a" else final_state.a
        winner_pet = self.pet_service.pet_repo.get_pet_by_id(
            winner.pet_id, guild_id
        )
        loser_pet = self.pet_service.pet_repo.get_pet_by_id(loser.pet_id, guild_id)
        if winner_pet is None or loser_pet is None:
            return Result.fail(
                "One of those pets no longer exists.",
                code=error_codes.NOT_FOUND,
            )
        before_records = self.pet_brawl_repo.get_records_for(
            [winner.pet_id, loser.pet_id], guild_id
        )
        winner_stat_gain = self._choose_stat_gain(winner_pet, BRAWL_WIN_XP)
        loser_stat_gain = self._choose_stat_gain(loser_pet, BRAWL_LOSS_XP)
        event_key = self._personality_event_key(
            winner,
            loser,
            self._record_milestone(
                before_records[winner.pet_id],
                before_records[loser.pet_id],
            ),
        )
        # Winner gain mirrors match wins, Pack Cama feed bonus included.
        winner_gain = BRAWL_WIN_HUNGER + get_species(winner.species_id).match_feed_bonus
        loser_loss = BRAWL_LOSS_HUNGER - (
            BRAWL_INSURANCE_REDUCTION
            if brawl_traits(loser.species_id).loss_insurance
            else 0
        )
        now = self._now()
        try:
            settlement = self.pet_brawl_repo.settle_brawl_atomic(
                brawl_id,
                guild_id,
                winner_id=winner.owner_id,
                winner_pet_id=winner.pet_id,
                loser_pet_id=loser.pet_id,
                rounds=rounds,
                now=now,
                decay_per_day=self.pet_service.decay_per_day,
                winner_gain=winner_gain,
                loser_loss=loser_loss,
                loss_floor=BRAWL_LOSS_FLOOR,
                daily_win_cap=BRAWL_WIN_HUNGER_DAILY_CAP,
                winner_stat_gain=winner_stat_gain,
                loser_stat_gain=loser_stat_gain,
                personality_event_key=event_key,
            )
        except ValueError as exc:
            return self._map_repo_error(exc)
        if self.pet_evolution_service is not None:
            for duelist in (winner, loser):
                self.pet_evolution_service.record_activity(
                    duelist.owner_id,
                    guild_id,
                    PetActivity.PET_BRAWL_COMPLETED,
                    f"pet-brawl:{brawl_id}",
                    occurred_at=now,
                )
        records = self.pet_brawl_repo.get_records_for(
            [winner.pet_id, loser.pet_id], guild_id
        )
        updated_winner = self.pet_service.pet_repo.get_pet_by_id(
            winner.pet_id, guild_id
        )
        updated_loser = self.pet_service.pet_repo.get_pet_by_id(
            loser.pet_id, guild_id
        )
        career = {
            winner.pet_id: self.career_summary(
                updated_winner,
                wins=records[winner.pet_id][0],
                losses=records[winner.pet_id][1],
            ),
            loser.pet_id: self.career_summary(
                updated_loser,
                wins=records[loser.pet_id][0],
                losses=records[loser.pet_id][1],
            ),
        }
        event_key = settlement["personality_event_key"]
        return Result.ok(
            {
                "winner": winner,
                "loser": loser,
                "winner_delta": settlement["winner_delta"],
                "loser_delta": settlement["loser_delta"],
                "winner_xp_delta": settlement["winner_xp_delta"],
                "loser_xp_delta": settlement["loser_xp_delta"],
                "winner_stat_gain": settlement["winner_stat_gain"],
                "loser_stat_gain": settlement["loser_stat_gain"],
                "payout": settlement["payout"],
                "wager": settlement["wager"],
                "fee": settlement["fee"],
                "records": records,
                "career": career,
                "personality_event_key": event_key,
                "personality_event": self.personality_event_text(
                    event_key, winner, loser
                ),
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
