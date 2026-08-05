"""Policy layer for lobby suspensions and moderation audit history."""

from __future__ import annotations

import re
import time
from enum import Enum

from domain.models.moderation import (
    ActiveSuspensionExistsError,
    LobbySuspension,
    ModerationEvent,
    ModerationEventType,
    ModerationSource,
    SuspensionCompletion,
    SuspensionScope,
)
from repositories.interfaces import IModerationRepository

_DURATION_TOKEN = re.compile(r"(\d+)\s*([smhdw])", re.IGNORECASE)
_DURATION_MULTIPLIERS = {
    "s": 1,
    "m": 60,
    "h": 60 * 60,
    "d": 24 * 60 * 60,
    "w": 7 * 24 * 60 * 60,
}
MIN_DURATION_SECONDS = 5 * 60
MAX_DURATION_SECONDS = 365 * 24 * 60 * 60
MIN_MATCHES = 1
MAX_MATCHES = 20
_DURATION_BOUNDS_ERROR = "Duration must be between 5 minutes and 365 days"
_LOW_PRIORITY_EVENTS = {
    ModerationEventType.LOWPRIO_ASSIGN,
    ModerationEventType.LOWPRIO_REPLACE,
    ModerationEventType.LOWPRIO_CLEAR,
    ModerationEventType.LOWPRIO_COMPLETE,
}


def parse_duration_seconds(value: str) -> int:
    """Parse compact duration text such as ``2h`` or ``1d 12h``."""
    if not isinstance(value, str) or not value.strip():
        raise ValueError("Duration is required (for example: 30m, 2h, 3d, or 1w)")

    raw = value.strip().lower()
    total = 0
    position = 0
    matched = False
    for token in _DURATION_TOKEN.finditer(raw):
        if raw[position : token.start()].strip():
            raise ValueError("Invalid duration; use units s, m, h, d, or w")
        amount = int(token.group(1))
        total += amount * _DURATION_MULTIPLIERS[token.group(2).lower()]
        position = token.end()
        matched = True

    if not matched or raw[position:].strip():
        raise ValueError("Invalid duration; use units s, m, h, d, or w")
    if not MIN_DURATION_SECONDS <= total <= MAX_DURATION_SECONDS:
        raise ValueError(_DURATION_BOUNDS_ERROR)
    return total


def parse_expiry(value: str, now: int | None = None) -> int:
    """Return an epoch-second expiry for compact duration text."""
    current = int(time.time()) if now is None else int(now)
    return current + parse_duration_seconds(value)


def _enum_value(enum_type, value):
    raw = value.value if isinstance(value, Enum) else str(value).lower()
    try:
        return enum_type(raw)
    except ValueError as exc:
        choices = ", ".join(item.value for item in enum_type)
        raise ValueError(f"Invalid {enum_type.__name__}: expected one of {choices}") from exc


def _clean_required_reason(reason: str) -> str:
    if not isinstance(reason, str) or not reason.strip():
        raise ValueError("A moderation reason is required")
    return reason.strip()


class ModerationService:
    """Validate, evaluate, and audit guild-scoped lobby moderation state."""

    parse_duration_seconds = staticmethod(parse_duration_seconds)
    parse_expiry = staticmethod(parse_expiry)

    def __init__(self, moderation_repo: IModerationRepository):
        self.repo = moderation_repo

    @staticmethod
    def _now(now: int | None) -> int:
        return int(time.time()) if now is None else int(now)

    @staticmethod
    def _validate_requirements(
        completion: SuspensionCompletion,
        *,
        expires_at: int | None,
        matches: int | None,
        now: int,
    ) -> None:
        needs_time = completion in {
            SuspensionCompletion.TIME,
            SuspensionCompletion.EITHER,
            SuspensionCompletion.BOTH,
        }
        needs_matches = completion in {
            SuspensionCompletion.MATCHES,
            SuspensionCompletion.EITHER,
            SuspensionCompletion.BOTH,
        }
        if needs_time:
            if expires_at is None:
                raise ValueError(f"{completion.value} completion requires expires_at")
            if (
                isinstance(expires_at, bool)
                or not isinstance(expires_at, int)
                or not (
                    MIN_DURATION_SECONDS
                    <= expires_at - now
                    <= MAX_DURATION_SECONDS
                )
            ):
                raise ValueError(_DURATION_BOUNDS_ERROR)
        elif expires_at is not None:
            raise ValueError("matches completion does not accept expires_at")

        if needs_matches:
            if matches is None:
                raise ValueError(f"{completion.value} completion requires matches")
            if (
                isinstance(matches, bool)
                or not isinstance(matches, int)
                or not MIN_MATCHES <= matches <= MAX_MATCHES
            ):
                raise ValueError(
                    f"matches must be between {MIN_MATCHES} and {MAX_MATCHES}"
                )
        elif matches is not None:
            raise ValueError("time completion does not accept matches")

    def get_pending_match_watermark(self, guild_id: int | None) -> int:
        """Return the durable pending-match boundary for a guild."""
        return self.repo.get_pending_match_watermark(guild_id)

    def get_suspension(
        self,
        discord_id: int,
        guild_id: int | None,
    ) -> LobbySuspension | None:
        """Return current state, including inactive state retained for status UI."""
        return self.repo.get_suspension(discord_id, guild_id)

    def get_active_suspension(
        self,
        discord_id: int,
        guild_id: int | None,
        lobby_kind=None,
        now: int | None = None,
    ) -> LobbySuspension | None:
        """Return an active applicable suspension, reconciling natural completion."""
        current = self._now(now)
        # A replacement can land between the read and natural-completion write.
        # Retry a revision conflict so callers never mistake that new term for
        # the expired state they originally observed.
        for _attempt in range(3):
            suspension = self.repo.get_suspension(discord_id, guild_id)
            if suspension is None or not suspension.active:
                return None
            if suspension.is_active_at(current):
                return suspension if suspension.applies_to(lobby_kind) else None
            closed = self.repo.close_suspension(
                discord_id,
                guild_id,
                event_type=ModerationEventType.COMPLETE.value,
                source=ModerationSource.SYSTEM.value,
                actor_id=None,
                reason="Suspension requirements completed",
                expected_revision=suspension.revision,
                now=current,
            )
            if closed is not None:
                return None

        latest = self.repo.get_suspension(discord_id, guild_id)
        if latest is None or not latest.active or not latest.is_active_at(current):
            return None
        return latest if latest.applies_to(lobby_kind) else None

    def create_suspension(
        self,
        discord_id: int,
        guild_id: int | None,
        *,
        actor_id: int,
        reason: str,
        scope: SuspensionScope | str = SuspensionScope.ALL,
        completion: SuspensionCompletion | str = SuspensionCompletion.TIME,
        expires_at: int | None = None,
        matches: int | None = None,
        source: ModerationSource | str = ModerationSource.ADMIN,
        replace: bool = False,
        pending_match_watermark: int | None = None,
        now: int | None = None,
    ) -> LobbySuspension:
        """Create a suspension, optionally replacing the current active one."""
        current = self._now(now)
        normalized_scope = _enum_value(SuspensionScope, scope)
        normalized_completion = _enum_value(SuspensionCompletion, completion)
        normalized_source = _enum_value(ModerationSource, source)
        clean_reason = _clean_required_reason(reason)
        self._validate_requirements(
            normalized_completion,
            expires_at=expires_at,
            matches=matches,
            now=current,
        )
        if pending_match_watermark is not None and (
            isinstance(pending_match_watermark, bool)
            or not isinstance(pending_match_watermark, int)
            or pending_match_watermark < 0
        ):
            raise ValueError("pending_match_watermark cannot be negative")

        existing = self.get_active_suspension(discord_id, guild_id, now=current)
        if existing is not None and not replace:
            raise ActiveSuspensionExistsError(
                "Player already has an active lobby suspension"
            )

        return self.repo.save_suspension(
            discord_id,
            guild_id,
            scope=normalized_scope.value,
            completion=normalized_completion.value,
            expires_at=int(expires_at) if expires_at is not None else None,
            matches_required=int(matches) if matches is not None else None,
            pending_match_watermark=(
                int(pending_match_watermark)
                if pending_match_watermark is not None
                else None
            ),
            reason=clean_reason,
            source=normalized_source.value,
            actor_id=int(actor_id),
            replace=replace,
            now=current,
        )

    def replace_suspension(
        self,
        discord_id: int,
        guild_id: int | None,
        *,
        actor_id: int,
        reason: str,
        scope: SuspensionScope | str = SuspensionScope.ALL,
        completion: SuspensionCompletion | str = SuspensionCompletion.TIME,
        expires_at: int | None = None,
        matches: int | None = None,
        source: ModerationSource | str = ModerationSource.ADMIN,
        pending_match_watermark: int | None = None,
        now: int | None = None,
    ) -> LobbySuspension:
        """Replace an active suspension, or create one when none is active."""
        return self.create_suspension(
            discord_id,
            guild_id,
            actor_id=actor_id,
            reason=reason,
            scope=scope,
            completion=completion,
            expires_at=expires_at,
            matches=matches,
            source=source,
            replace=True,
            pending_match_watermark=pending_match_watermark,
            now=now,
        )

    def lift_suspension(
        self,
        discord_id: int,
        guild_id: int | None,
        *,
        actor_id: int,
        reason: str,
        source: ModerationSource | str = ModerationSource.ADMIN,
        now: int | None = None,
    ) -> LobbySuspension | None:
        """Lift the current active suspension and append an audit event."""
        current = self._now(now)
        active = self.get_active_suspension(discord_id, guild_id, now=current)
        if active is None:
            return None
        return self.repo.close_suspension(
            discord_id,
            guild_id,
            event_type=ModerationEventType.LIFT.value,
            source=_enum_value(ModerationSource, source).value,
            actor_id=int(actor_id),
            reason=_clean_required_reason(reason),
            expected_revision=active.revision,
            now=current,
        )

    def list_active_suspensions(
        self,
        guild_id: int | None,
        lobby_kind=None,
        now: int | None = None,
    ) -> list[LobbySuspension]:
        """List active suspensions, optionally filtered to one lobby kind."""
        current = self._now(now)
        active: list[LobbySuspension] = []
        for suspension in self.repo.list_suspensions(guild_id, active_only=True):
            if not suspension.is_active_at(current):
                closed = self.repo.close_suspension(
                    suspension.discord_id,
                    guild_id,
                    event_type=ModerationEventType.COMPLETE.value,
                    source=ModerationSource.SYSTEM.value,
                    actor_id=None,
                    reason="Suspension requirements completed",
                    expected_revision=suspension.revision,
                    now=current,
                )
                if closed is None:
                    replacement = self.get_active_suspension(
                        suspension.discord_id,
                        guild_id,
                        lobby_kind=lobby_kind,
                        now=current,
                    )
                    if replacement is not None:
                        active.append(replacement)
            elif suspension.applies_to(lobby_kind):
                active.append(suspension)
        return active

    def get_history(
        self,
        guild_id: int | None,
        discord_id: int | None = None,
        *,
        limit: int = 50,
        offset: int = 0,
    ) -> list[ModerationEvent]:
        """Return newest-first immutable moderation history."""
        if isinstance(limit, bool) or not 1 <= int(limit) <= 200:
            raise ValueError("limit must be between 1 and 200")
        if isinstance(offset, bool) or int(offset) < 0:
            raise ValueError("offset cannot be negative")
        return self.repo.get_history(
            guild_id,
            discord_id,
            limit=int(limit),
            offset=int(offset),
        )

    def get_completion_events_for_match(
        self,
        guild_id: int | None,
        match_id: int,
    ) -> list[ModerationEvent]:
        """Return target notifications unlocked by a committed match."""
        if isinstance(match_id, bool) or not isinstance(match_id, int) or match_id <= 0:
            raise ValueError("match_id must be a positive integer")
        return self.repo.get_completion_events_for_match(guild_id, match_id)

    def record_kick(
        self,
        discord_id: int,
        guild_id: int | None,
        *,
        actor_id: int,
        lobby_kind,
        reason: str | None = None,
        source: ModerationSource | str = ModerationSource.ADMIN,
        now: int | None = None,
    ) -> ModerationEvent:
        """Append an audit event for a direct or vote-resolved lobby kick."""
        scope = _enum_value(SuspensionScope, lobby_kind)
        if scope is SuspensionScope.ALL:
            raise ValueError("A kick must identify the concrete lobby kind")
        clean_reason = reason.strip() if isinstance(reason, str) and reason.strip() else None
        return self.repo.record_event(
            discord_id,
            guild_id,
            event_type=ModerationEventType.KICK.value,
            source=_enum_value(ModerationSource, source).value,
            actor_id=int(actor_id),
            reason=clean_reason,
            scope=scope.value,
            now=self._now(now),
        )

    def record_low_priority_event(
        self,
        discord_id: int,
        guild_id: int | None,
        *,
        event_type: ModerationEventType | str,
        wins_required: int,
        wins_remaining: int,
        actor_id: int | None = None,
        reason: str | None = None,
        match_id: int | None = None,
        pending_match_watermark: int | None = None,
        source: ModerationSource | str | None = None,
        now: int | None = None,
    ) -> ModerationEvent:
        """Append an immutable audit event for low-priority state changes."""
        normalized_event = _enum_value(ModerationEventType, event_type)
        if normalized_event not in _LOW_PRIORITY_EVENTS:
            raise ValueError("event_type must be a low-priority moderation event")
        if (
            isinstance(wins_required, bool)
            or not isinstance(wins_required, int)
            or not 1 <= wins_required <= 20
        ):
            raise ValueError("wins_required must be between 1 and 20")
        if (
            isinstance(wins_remaining, bool)
            or not isinstance(wins_remaining, int)
            or not 0 <= wins_remaining <= 20
        ):
            raise ValueError("wins_remaining must be between 0 and 20")
        if wins_remaining > wins_required:
            raise ValueError("wins_remaining cannot exceed wins_required")
        if pending_match_watermark is not None and (
            isinstance(pending_match_watermark, bool)
            or not isinstance(pending_match_watermark, int)
            or pending_match_watermark < 0
        ):
            raise ValueError("pending_match_watermark cannot be negative")
        inferred_source = (
            ModerationSource.SYSTEM
            if normalized_event is ModerationEventType.LOWPRIO_COMPLETE
            else ModerationSource.ADMIN
        )
        normalized_source = _enum_value(
            ModerationSource,
            inferred_source if source is None else source,
        )
        clean_reason = reason.strip() if isinstance(reason, str) and reason.strip() else None
        return self.repo.record_event(
            discord_id,
            guild_id,
            event_type=normalized_event.value,
            source=normalized_source.value,
            actor_id=int(actor_id) if actor_id is not None else None,
            reason=clean_reason,
            pending_match_watermark=pending_match_watermark,
            wins_required=int(wins_required),
            wins_remaining=int(wins_remaining),
            match_id=int(match_id) if match_id is not None else None,
            now=self._now(now),
        )


__all__ = [
    "ActiveSuspensionExistsError",
    "ModerationService",
    "parse_duration_seconds",
    "parse_expiry",
]
