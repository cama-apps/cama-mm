"""Domain types for guild-scoped lobby moderation."""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum


class SuspensionScope(str, Enum):
    """Lobby kinds affected by a suspension."""

    ALL = "all"
    OPEN = "open"
    LOWSKILL = "lowskill"


class SuspensionCompletion(str, Enum):
    """Conditions that release a lobby suspension."""

    TIME = "time"
    MATCHES = "matches"
    EITHER = "either"
    BOTH = "both"


class ModerationSource(str, Enum):
    """Authority or workflow that produced a moderation event."""

    ADMIN = "admin"
    VOTE = "vote"
    CREATOR = "creator"
    SYSTEM = "system"


class ModerationEventType(str, Enum):
    """Immutable moderation history event kinds."""

    SUSPEND = "suspend"
    REPLACE = "replace"
    LIFT = "lift"
    COMPLETE = "complete"
    KICK = "kick"
    LOWPRIO_ASSIGN = "lowprio_assign"
    LOWPRIO_REPLACE = "lowprio_replace"
    LOWPRIO_CLEAR = "lowprio_clear"
    LOWPRIO_COMPLETE = "lowprio_complete"


class ActiveSuspensionExistsError(ValueError):
    """Raised when a caller tries to create over an active suspension."""


@dataclass(frozen=True)
class LobbySuspension:
    """Current lobby-suspension state enriched with recorded-match progress."""

    discord_id: int
    guild_id: int
    scope: SuspensionScope
    completion: SuspensionCompletion
    reason: str
    source: ModerationSource
    actor_id: int
    expires_at: int | None
    matches_required: int | None
    pending_match_watermark: int
    matches_completed: int
    matches_remaining: int | None
    revision: int
    active: bool
    created_at: int
    updated_at: int

    def applies_to(self, lobby_kind: str | Enum | None) -> bool:
        """Return whether this suspension covers ``lobby_kind``.

        ``None`` intentionally means "do not filter by lobby kind", which is
        useful for admin status and history views.
        """
        if lobby_kind is None or self.scope is SuspensionScope.ALL:
            return True
        value = lobby_kind.value if isinstance(lobby_kind, Enum) else str(lobby_kind)
        return self.scope.value == value.lower()

    def is_active_at(self, now: int) -> bool:
        """Evaluate the configured time/match completion semantics."""
        if not self.active:
            return False

        time_pending = self.expires_at is not None and now < self.expires_at
        matches_pending = (
            self.matches_remaining is not None and self.matches_remaining > 0
        )

        if self.completion is SuspensionCompletion.TIME:
            return time_pending
        if self.completion is SuspensionCompletion.MATCHES:
            return matches_pending
        if self.completion is SuspensionCompletion.EITHER:
            return time_pending and matches_pending
        return time_pending or matches_pending


@dataclass(frozen=True)
class ModerationEvent:
    """One immutable moderation or low-priority audit event."""

    event_id: int
    guild_id: int
    discord_id: int
    event_type: ModerationEventType
    source: ModerationSource
    actor_id: int | None
    reason: str | None
    scope: SuspensionScope | None
    completion: SuspensionCompletion | None
    expires_at: int | None
    matches_required: int | None
    matches_remaining: int | None
    pending_match_watermark: int | None
    wins_required: int | None
    wins_remaining: int | None
    match_id: int | None
    created_at: int
