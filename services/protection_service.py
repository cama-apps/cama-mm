"""Application service for White-mana hostile-loss protection."""

from __future__ import annotations

import time
from collections.abc import Iterable, Mapping
from typing import Any

from domain.models.hostile_loss import (
    HostileLossDestination,
    HostileLossKind,
    HostileLossResult,
    NonJcProtectionResult,
)
from repositories.protection_repository import ProtectionRepository
from services.mana_service import get_today_pst


class ProtectionService:
    """Validate and atomically settle already-scaled hostile JC losses."""

    def __init__(self, protection_repo: ProtectionRepository):
        self.protection_repo = protection_repo

    def apply_hostile_loss(
        self,
        victim_id: int,
        guild_id: int | None,
        amount: int,
        kind: HostileLossKind | str,
        *,
        actor_id: int | None,
        event_key: str,
        destination: HostileLossDestination | str = HostileLossDestination.BURN,
        recipient_id: int | None = None,
        clamp_to_balance: bool = False,
        min_balance: int | None = None,
        metadata: Mapping[str, object] | None = None,
        occurred_at: int | None = None,
    ) -> HostileLossResult:
        """Settle one hostile loss after source-specific economy scaling.

        ``amount`` must be the positive amount the source would currently debit;
        this service does not apply the global minigame multiplier a second time.
        Reusing ``event_key`` for the same victim returns the stored outcome.
        """
        normalized = self._normalize_hostile_loss(
            victim_id=victim_id,
            guild_id=guild_id,
            amount=amount,
            kind=kind,
            actor_id=actor_id,
            event_key=event_key,
            destination=destination,
            recipient_id=recipient_id,
            clamp_to_balance=clamp_to_balance,
            min_balance=min_balance,
            metadata=metadata,
            occurred_at=occurred_at,
        )
        return self.protection_repo.apply_hostile_loss(**normalized)

    def apply_hostile_losses(
        self, losses: Iterable[Mapping[str, Any]]
    ) -> list[HostileLossResult | Exception]:
        """Settle an ordered victim batch under one operation transaction.

        Results align with the input. Invalid or rejected victims occupy their
        position as an exception while all other victims still settle.
        """
        raw_losses = list(losses)
        if not raw_losses:
            return []

        prepared: list[dict[str, Any]] = []
        prepared_positions: list[int] = []
        errors: dict[int, Exception] = {}
        for index, loss in enumerate(raw_losses):
            try:
                normalized = self._normalize_hostile_loss(**dict(loss))
            except Exception as exc:
                errors[index] = exc
                continue
            prepared.append(normalized)
            prepared_positions.append(index)

        settled = self.protection_repo.apply_hostile_losses(prepared)
        settled_by_position = dict(zip(prepared_positions, settled, strict=True))
        return [
            errors[index] if index in errors else settled_by_position[index]
            for index in range(len(raw_losses))
        ]

    @staticmethod
    def _normalize_hostile_loss(
        victim_id: int,
        guild_id: int | None,
        amount: int,
        kind: HostileLossKind | str,
        *,
        actor_id: int | None,
        event_key: str,
        destination: HostileLossDestination | str = HostileLossDestination.BURN,
        recipient_id: int | None = None,
        clamp_to_balance: bool = False,
        min_balance: int | None = None,
        metadata: Mapping[str, object] | None = None,
        occurred_at: int | None = None,
    ) -> dict[str, Any]:
        """Validate public inputs and build repository settlement arguments."""
        requested = int(amount)
        if requested <= 0:
            raise ValueError("amount must be a positive, already-scaled integer")
        if not event_key or not event_key.strip():
            raise ValueError("event_key is required")
        if min_balance is not None:
            min_balance = int(min_balance)
        normalized_kind = (
            kind if isinstance(kind, HostileLossKind) else HostileLossKind(str(kind).lower())
        )
        normalized_destination = (
            destination
            if isinstance(destination, HostileLossDestination)
            else HostileLossDestination(str(destination).lower())
        )
        if (
            normalized_destination is HostileLossDestination.PLAYER
            and recipient_id is None
        ):
            raise ValueError("recipient_id is required for player destination")

        return {
            "victim_id": int(victim_id),
            "guild_id": guild_id,
            "requested": requested,
            "kind": normalized_kind,
            "actor_id": int(actor_id) if actor_id is not None else None,
            "event_key": event_key,
            "destination": normalized_destination,
            "recipient_id": (int(recipient_id) if recipient_id is not None else None),
            "clamp_to_balance": bool(clamp_to_balance),
            "min_balance": min_balance,
            "metadata": dict(metadata or {}),
            "occurred_at": int(occurred_at if occurred_at is not None else time.time()),
            "mana_date": get_today_pst(),
        }

    def reconcile_guardian(
        self, discord_id: int, guild_id: int | None, since_ts: int
    ) -> int:
        """Apply today's Guardian pool to uncovered losses since ``since_ts``."""
        return self.protection_repo.reconcile_guardian(
            discord_id=int(discord_id),
            guild_id=guild_id,
            since_ts=int(since_ts),
            mana_date=get_today_pst(),
            now=int(time.time()),
        )

    def reconcile_guardians(
        self,
        discord_ids: list[int],
        guild_id: int | None,
        since_ts: int,
    ) -> dict[int, int | Exception]:
        """Reconcile an ordered Guardian batch under one repository txn."""
        return self.protection_repo.reconcile_guardians(
            discord_ids=discord_ids,
            guild_id=guild_id,
            since_ts=int(since_ts),
            mana_date=get_today_pst(),
            now=int(time.time()),
        )

    def reconcile_purchased_pool(
        self,
        discord_id: int,
        guild_id: int | None,
        buff_id: int,
        lookback_seconds: int,
        *,
        now: int | None = None,
    ) -> int:
        """Apply a specified rolling-retroactive pool to prior hostile losses."""
        current = int(now if now is not None else time.time())
        lookback = int(lookback_seconds)
        if lookback < 0:
            raise ValueError("lookback_seconds cannot be negative")
        return self.protection_repo.reconcile_purchased_pool(
            discord_id=int(discord_id),
            guild_id=guild_id,
            buff_id=int(buff_id),
            since_ts=current - lookback,
            now=current,
        )

    def block_non_jc_attack(
        self,
        victim_id: int,
        guild_id: int | None,
        *,
        actor_id: int | None,
        event_key: str,
        occurred_at: int | None = None,
    ) -> NonJcProtectionResult:
        """Block sabotage with persistent wards or consume one whole Aegis."""
        if not event_key or not event_key.strip():
            raise ValueError("event_key is required")
        return self.protection_repo.block_non_jc_attack(
            victim_id=int(victim_id),
            guild_id=guild_id,
            actor_id=int(actor_id) if actor_id is not None else None,
            event_key=event_key,
            occurred_at=int(occurred_at if occurred_at is not None else time.time()),
        )
