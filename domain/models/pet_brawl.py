"""Domain model for pet brawls.

A row is the persisted challenge lifecycle and final result of one brawl.
The battle itself (round-by-round HP) is deliberately NOT stored here: the
stakes are hunger-only and applied atomically at settlement, so an
interrupted battle can be voided harmlessly.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
from enum import StrEnum
from typing import Any


class PetBrawlStatus(StrEnum):
    PENDING = "pending"
    ACTIVE = "active"
    DONE = "done"
    DECLINED = "declined"
    EXPIRED = "expired"
    VOID = "void"


@dataclass(frozen=True, slots=True)
class PetBrawl:
    brawl_id: int
    guild_id: int
    channel_id: int
    challenger_id: int
    recipient_id: int
    challenger_pet_id: int
    recipient_pet_id: int | None
    status: str
    created_at: int
    expires_at: int
    resolved_at: int | None
    winner_id: int | None
    winner_pet_id: int | None
    loser_pet_id: int | None
    rounds: int | None
    winner_hunger_delta: int
    loser_hunger_delta: int

    @classmethod
    def from_row(cls, row: Mapping[str, Any]) -> PetBrawl:
        return cls(**dict(row))

    @property
    def is_open(self) -> bool:
        return self.status in (PetBrawlStatus.PENDING, PetBrawlStatus.ACTIVE)
