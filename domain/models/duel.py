from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
from enum import StrEnum
from typing import Any

DUEL_ISSUANCE_FEE = 50


class DuelRecipientFundingError(ValueError):
    """Raised when a recipient cannot fund a newly issued duel."""

    def __init__(self, recipient_id: int, wager: int) -> None:
        self.recipient_id = recipient_id
        self.wager = wager
        super().__init__("The challenged player cannot cover the duel wager.")


class DuelStatus(StrEnum):
    PENDING = "pending"
    ACCEPTED = "accepted"
    DECLINED = "declined"
    EXPIRED = "expired"
    RESOLVED = "resolved"
    VOIDED = "voided"
    DELIVERY_FAILED = "delivery_failed"


class DuelTrial(StrEnum):
    TRIAL_BY_COMBAT = "trial_by_combat"
    TRIAL_OF_FIVE = "trial_of_five"


class DuelResolution(StrEnum):
    CHALLENGER_VICTORY = "challenger_victory"
    RECIPIENT_VICTORY = "recipient_victory"
    VOID = "void"


class DuelDueKind(StrEnum):
    REMINDER = "reminder"
    EXPIRED = "expired"
    UNRESOLVED = "unresolved"


@dataclass(frozen=True, slots=True)
class DuelChallenge:
    challenge_id: int
    guild_id: int
    channel_id: int
    message_id: int | None
    challenger_id: int
    recipient_id: int
    wager: int
    issuance_fee: int
    status: DuelStatus
    trial_type: DuelTrial | None
    challenger_glicko: float
    challenger_rd: float
    recipient_glicko: float
    recipient_rd: float
    created_at: int
    expires_at: int
    next_reminder_at: int | None
    responded_at: int | None
    resolved_at: int | None
    winner_id: int | None
    resolution_actor_id: int | None

    @property
    def decline_penalty(self) -> int:
        return (self.wager + 1) // 2

    @classmethod
    def from_row(cls, row: Mapping[str, Any]) -> DuelChallenge:
        values = dict(row)
        values["status"] = DuelStatus(values["status"])
        if values["trial_type"] is not None:
            values["trial_type"] = DuelTrial(values["trial_type"])
        return cls(**values)


@dataclass(frozen=True, slots=True)
class DuelDueResult:
    kind: DuelDueKind
    challenge: DuelChallenge
    remaining_seconds: int = 0
    ping_recipient: bool = False
