"""Domain model for cama pets.

All time-dependent state (hunger, stage, mood, death) is DERIVED from stored
anchors rather than ticked by a scheduler, so bot downtime never corrupts
state — it only delays announcements. The one rule that follows from this:
`starvation_time` (the moment hunger hit 0) is the pet's time of death,
never the moment anyone noticed.

Hunger math is exact integer arithmetic: decay rates and species quirk
multipliers are integer points-per-day and integer percentages.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass, replace
from enum import StrEnum
from typing import Any

from domain.pet_constants import (
    ADULT_AGE_SECONDS,
    DIG_WORK_PER_DIG_CAP_BLOCKS,
    DIG_WORK_RATE_CONTENT,
    DIG_WORK_RATE_HAPPY,
    DIG_WORK_RATE_HUNGRY,
    DIG_WORK_UNITS_PER_BLOCK,
    MAX_HUNGER,
    MOOD_CONTENT_MIN,
    MOOD_HAPPY_MIN,
    MOOD_HUNGRY_MIN,
    get_species,
)

_SECONDS_PER_DAY = 86400


class PetStage(StrEnum):
    EGG = "egg"
    BABY = "baby"
    ADULT = "adult"


class PetMood(StrEnum):
    HAPPY = "happy"
    CONTENT = "content"
    HUNGRY = "hungry"
    STARVING = "starving"


# Mood -> art asset key (only three art moods exist per species/stage).
ART_MOOD = {
    PetMood.HAPPY: "happy",
    PetMood.CONTENT: "neutral",
    PetMood.HUNGRY: "hungry",
    PetMood.STARVING: "hungry",
}


@dataclass(frozen=True, slots=True)
class Pet:
    pet_id: int
    discord_id: int
    guild_id: int
    name: str
    species: str
    adopted_at: int
    hatched_at: int
    adopt_fee: int
    last_fed_at: int
    hunger_at_last_fed: int
    times_fed: int
    feeds_today: int
    feed_date: str | None
    week_consumed_jc: int
    week_key: str | None
    prev_week_consumed_jc: int
    prev_week_key: str | None
    pampered_until: int | None
    accessory: str | None
    dig_work_units: int
    dig_work_at: int
    aegis_used: int
    hatch_announced_at: int | None
    died_at: int | None
    death_cause: str | None
    death_announced_at: int | None
    egg_tier: str = "standard"
    training_xp: int = 0
    training_str: int = 0
    training_int: int = 0
    training_dex: int = 0
    solo_training_sessions: int = 3
    solo_training_recharged_at: int | None = None
    evolution_started_at: int | None = None
    evolution_due_at: int | None = None
    evolved_at: int | None = None
    evolution_calling: str | None = None
    evolution_primary: str | None = None
    evolution_secondary: str | None = None
    evolution_announced_at: int | None = None

    @classmethod
    def from_row(cls, row: Mapping[str, Any]) -> Pet:
        return cls(**dict(row))

    def _decay_pct(self) -> int:
        return get_species(self.species).decay_pct

    def current_hunger(self, now: int, decay_per_day: int) -> int:
        """Derived hunger at `now`.

        An egg's anchor is (100 @ hatched_at), so elapsed time is negative
        before hatch and clamps to zero decay — eggs cannot starve without
        any special-casing.
        """
        if self.died_at is not None:
            return 0
        elapsed = max(0, now - self.last_fed_at)
        decayed = elapsed * decay_per_day * self._decay_pct() // (_SECONDS_PER_DAY * 100)
        return max(0, self.hunger_at_last_fed - decayed)

    def starvation_time(self, decay_per_day: int) -> int:
        """The exact moment derived hunger reaches 0 — the time of death."""
        rate = decay_per_day * self._decay_pct()
        if rate <= 0:
            raise ValueError("decay rate must be positive")
        points = self.hunger_at_last_fed * _SECONDS_PER_DAY * 100
        return self.last_fed_at + -(-points // rate)  # ceil division

    def is_starved(self, now: int, decay_per_day: int) -> bool:
        return self.died_at is None and now >= self.starvation_time(decay_per_day)

    def hunger_crossing_time(self, threshold: int, decay_per_day: int) -> int:
        """When derived hunger first drops to `threshold` (for warning DMs)."""
        rate = decay_per_day * self._decay_pct()
        if rate <= 0:
            raise ValueError("decay rate must be positive")
        surplus = max(0, self.hunger_at_last_fed - threshold)
        return self.last_fed_at + -(-(surplus * _SECONDS_PER_DAY * 100) // rate)

    def stage(self, now: int) -> PetStage:
        """Life stage, frozen at the moment of death for dead pets."""
        ref = now if self.died_at is None else min(now, self.died_at)
        if ref < self.hatched_at:
            return PetStage.EGG
        if ref < self.hatched_at + ADULT_AGE_SECONDS:
            return PetStage.BABY
        return PetStage.ADULT

    def mood(self, now: int, decay_per_day: int) -> PetMood:
        hunger = self.current_hunger(now, decay_per_day)
        if (
            self.pampered_until is not None
            and self.pampered_until > now
            and hunger >= MOOD_HUNGRY_MIN
        ):
            return PetMood.HAPPY
        if hunger >= MOOD_HAPPY_MIN:
            return PetMood.HAPPY
        if hunger >= MOOD_CONTENT_MIN:
            return PetMood.CONTENT
        if hunger >= MOOD_HUNGRY_MIN:
            return PetMood.HUNGRY
        return PetMood.STARVING

    def art_mood(self, now: int, decay_per_day: int) -> str:
        return ART_MOOD[self.mood(now, decay_per_day)]

    def dig_work_units_between(
        self, start: int, end: int, decay_per_day: int
    ) -> int:
        """Work accrued over ``[start, end)`` at the historical mood rates."""
        start = max(int(start), self.hatched_at)
        stop = self.died_at if self.died_at is not None else self.starvation_time(
            decay_per_day
        )
        end = min(int(end), stop)
        if end <= start:
            return 0

        boundaries = {start, end}
        for threshold in (
            MOOD_HAPPY_MIN - 1,
            MOOD_CONTENT_MIN - 1,
            MOOD_HUNGRY_MIN - 1,
        ):
            crossing = self.hunger_crossing_time(threshold, decay_per_day)
            if start < crossing < end:
                boundaries.add(crossing)
        if self.pampered_until is not None and start < self.pampered_until < end:
            boundaries.add(self.pampered_until)

        rates = {
            PetMood.HAPPY: DIG_WORK_RATE_HAPPY,
            PetMood.CONTENT: DIG_WORK_RATE_CONTENT,
            PetMood.HUNGRY: DIG_WORK_RATE_HUNGRY,
            PetMood.STARVING: 0,
        }
        living = self if self.died_at is None else replace(self, died_at=None)
        points = sorted(boundaries)
        return sum(
            (right - left) * rates[living.mood(left, decay_per_day)]
            for left, right in zip(points, points[1:])
        )

    def age_seconds(self, now: int) -> int:
        """Age since hatch; eggs are age 0, dead pets freeze at death."""
        ref = now if self.died_at is None else min(now, self.died_at)
        return max(0, ref - self.hatched_at)

    def restored_hunger(self, now: int, decay_per_day: int, restore: int) -> int:
        """New anchor hunger after feeding `restore` points (quirk-adjusted)."""
        boosted = restore * get_species(self.species).restore_pct // 100
        return min(MAX_HUNGER, self.current_hunger(now, decay_per_day) + boosted)

    def feeds_used_on(self, game_date: str) -> int:
        """Feed-cap usage for a game date; a stale feed_date means a fresh day."""
        return self.feeds_today if self.feed_date == game_date else 0

    def consumed_in_week(self, week_key: str) -> int:
        """Care JC consumed during the given week (current or previous slot)."""
        if self.week_key == week_key:
            return self.week_consumed_jc
        if self.prev_week_key == week_key:
            return self.prev_week_consumed_jc
        return 0


@dataclass(frozen=True, slots=True)
class PetDigWork:
    """A point-in-time pet-work offer that can be claimed by one dig."""

    pet_id: int
    pet_name: str
    expected_units: int
    expected_at: int
    accrued_units: int
    as_of: int

    @property
    def available_blocks(self) -> int:
        return min(
            DIG_WORK_PER_DIG_CAP_BLOCKS,
            self.accrued_units // DIG_WORK_UNITS_PER_BLOCK,
        )

    def claim(self, applied_blocks: int) -> dict[str, int]:
        applied_blocks = int(applied_blocks)
        if not 0 < applied_blocks <= self.available_blocks:
            raise ValueError("applied pet-work blocks exceed the available offer")
        return {
            "pet_id": self.pet_id,
            "expected_units": self.expected_units,
            "expected_at": self.expected_at,
            "new_units": (
                self.accrued_units
                - applied_blocks * DIG_WORK_UNITS_PER_BLOCK
            ),
            "new_at": self.as_of,
        }


@dataclass(frozen=True, slots=True)
class PetStatus:
    """Composed view for status embeds: the living pet (if any) with its
    derived state, the owner's supplies, and the most recent memorial."""

    pet: Pet | None
    hunger: int = 0
    stage: PetStage | None = None
    mood: PetMood | None = None
    age_seconds: int = 0
    supplies: dict[str, int] | None = None
    last_dead: Pet | None = None
    dig_work_units: int = 0
    dig_work_rate: int = 0
    evolution_hint: str | None = None


@dataclass(frozen=True, slots=True)
class FeedOutcome:
    pet: Pet
    item_id: str
    old_hunger: int
    new_hunger: int
    remaining_qty: int
    feeds_left_today: int
    spat: bool = False


@dataclass(frozen=True, slots=True)
class TrinketOutcome:
    accessory_id: str
    duplicate: bool
    net_cost: int
    owned_count: int


@dataclass(frozen=True, slots=True)
class DeathNotice:
    pet: Pet
    eating_outcome: Mapping[str, int] | None = None


@dataclass(frozen=True, slots=True)
class HatchNotice:
    pet: Pet


@dataclass(frozen=True, slots=True)
class EvolutionNotice:
    pet: Pet


@dataclass(frozen=True, slots=True)
class RefundPayout:
    discord_id: int
    consumed_jc: int
    multiplier_pct: int
    amount: int


@dataclass(frozen=True, slots=True)
class RefundNotice:
    guild_id: int
    week_key: str
    payouts: tuple[RefundPayout, ...]
    total_paid: int
    scaled_down: bool
