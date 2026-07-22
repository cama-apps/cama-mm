"""Atomic persistence for hostile-loss protection."""

from __future__ import annotations

import json
import math
import time
from dataclasses import asdict
from typing import Any

from domain.models.hostile_loss import (
    HostileLossDestination,
    HostileLossKind,
    HostileLossResult,
    NonJcProtectionResult,
    ProtectionDetail,
)
from repositories.base_repository import BaseRepository, safe_json_loads

_POOL_DEFAULTS: dict[str, tuple[int, float]] = {
    "reprieve": (25, 0.5),
    "aegis": (75, 1.0),
    "sanctuary": (150, 1.0),
}


class ProtectionRepository(BaseRepository):
    """Resolve protection and its balance movement under one write lock."""

    @staticmethod
    def _rate_absorption(amount: int, rate: float) -> int:
        if amount <= 0 or rate <= 0:
            return 0
        if rate >= 1:
            return amount
        return max(1, math.floor(amount * rate))

    @staticmethod
    def _decode_pool(row: Any) -> tuple[dict, int, float]:
        data = safe_json_loads(
            row["data"], {}, context=f"manashop_buffs[{row['id']}].data"
        )
        if not isinstance(data, dict):
            data = {}
        default_capacity, default_rate = _POOL_DEFAULTS[row["buff_type"]]
        try:
            capacity = max(
                0, int(data.get("capacity_remaining", default_capacity))
            )
        except (TypeError, ValueError):
            capacity = default_capacity
        try:
            rate = min(1.0, max(0.0, float(data.get("rate", default_rate))))
        except (TypeError, ValueError):
            rate = default_rate
        return data, capacity, rate

    @staticmethod
    def _details_json(details: list[ProtectionDetail]) -> str:
        return json.dumps([asdict(detail) for detail in details], sort_keys=True)

    @staticmethod
    def _details_from_json(raw: str | None) -> tuple[ProtectionDetail, ...]:
        payload = safe_json_loads(raw, [], context="hostile_loss_events.details")
        if not isinstance(payload, list):
            return ()
        details: list[ProtectionDetail] = []
        for item in payload:
            if not isinstance(item, dict):
                continue
            try:
                details.append(
                    ProtectionDetail(
                        source=str(item["source"]),
                        absorbed=int(item.get("absorbed", 0)),
                        rate=float(item.get("rate", 0)),
                        capacity_before=(
                            int(item["capacity_before"])
                            if item.get("capacity_before") is not None
                            else None
                        ),
                        capacity_after=(
                            int(item["capacity_after"])
                            if item.get("capacity_after") is not None
                            else None
                        ),
                        buff_id=(
                            int(item["buff_id"])
                            if item.get("buff_id") is not None
                            else None
                        ),
                        retroactive=bool(item.get("retroactive", False)),
                    )
                )
            except (KeyError, TypeError, ValueError):
                continue
        return tuple(details)

    @classmethod
    def _row_to_result(
        cls, row: Any, *, duplicate: bool
    ) -> HostileLossResult:
        return HostileLossResult(
            event_id=int(row["event_id"]),
            event_key=str(row["event_key"]),
            kind=HostileLossKind(row["kind"]),
            destination=HostileLossDestination(row["destination"]),
            victim_id=int(row["victim_id"]),
            guild_id=int(row["guild_id"]),
            actor_id=(int(row["actor_id"]) if row["actor_id"] is not None else None),
            recipient_id=(
                int(row["recipient_id"])
                if row["recipient_id"] is not None
                else None
            ),
            requested=int(row["requested"]),
            attempted=int(row["attempted"]),
            absorbed=int(row["absorbed"]),
            applied=int(row["applied"]),
            victim_balance_before=int(row["victim_balance_before"]),
            victim_balance_after=int(row["victim_balance_after"]),
            destination_balance_before=(
                int(row["destination_balance_before"])
                if row["destination_balance_before"] is not None
                else None
            ),
            destination_balance_after=(
                int(row["destination_balance_after"])
                if row["destination_balance_after"] is not None
                else None
            ),
            shieldable=bool(row["shieldable"]),
            duplicate=duplicate,
            details=cls._details_from_json(row["protection_details"]),
        )

    @staticmethod
    def _active_buffs(
        cursor,
        *,
        victim_id: int,
        guild_id: int,
        buff_type: str,
        now: int,
        shared: bool = False,
    ) -> list[Any]:
        ownership = "(discord_id = ? OR target_id = ?)" if shared else "discord_id = ?"
        params: tuple = (
            (victim_id, victim_id, guild_id, buff_type, now)
            if shared
            else (victim_id, guild_id, buff_type, now)
        )
        return list(
            cursor.execute(
                f"""
                SELECT id, discord_id, target_id, buff_type, expires_at, data
                FROM manashop_buffs
                WHERE {ownership}
                  AND guild_id = ? AND buff_type = ?
                  AND triggered = 0 AND expires_at > ?
                ORDER BY expires_at ASC, granted_at ASC, id ASC
                """,  # noqa: S608 - ownership is an internal fixed fragment
                params,
            ).fetchall()
        )

    @staticmethod
    def _persist_pool(cursor, row: Any, data: dict, remaining: int) -> None:
        data = dict(data)
        data["capacity_remaining"] = remaining
        cursor.execute(
            "UPDATE manashop_buffs SET data = ?, triggered = ? WHERE id = ?",
            (json.dumps(data, sort_keys=True), int(remaining <= 0), row["id"]),
        )

    @staticmethod
    def _insert_protection_event(
        cursor,
        *,
        hostile_event_id: int,
        guild_id: int,
        victim_id: int,
        detail: ProtectionDetail,
        pool_key: str,
        created_at: int,
        details: dict | None = None,
    ) -> None:
        cursor.execute(
            """
            INSERT INTO mana_protection_events (
                hostile_loss_event_id, guild_id, victim_id, protection_type,
                pool_key, buff_id, amount, rate, capacity_before,
                capacity_after, retroactive, created_at, details
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            (
                hostile_event_id,
                guild_id,
                victim_id,
                detail.source,
                pool_key,
                detail.buff_id,
                detail.absorbed,
                detail.rate,
                detail.capacity_before,
                detail.capacity_after,
                int(detail.retroactive),
                created_at,
                json.dumps(details, sort_keys=True) if details else None,
            ),
        )

    def apply_hostile_loss(
        self,
        *,
        victim_id: int,
        guild_id: int | None,
        requested: int,
        kind: HostileLossKind,
        actor_id: int | None,
        event_key: str,
        destination: HostileLossDestination,
        recipient_id: int | None,
        clamp_to_balance: bool,
        min_balance: int | None,
        metadata: dict,
        occurred_at: int,
        mana_date: str,
    ) -> HostileLossResult:
        """Apply one hostile loss, consuming protection and moving JC atomically."""
        gid = self.normalize_guild_id(guild_id)
        now = int(time.time())
        with self.atomic_transaction() as conn:
            return self._apply_hostile_loss_with_cursor(
                conn.cursor(),
                victim_id=victim_id,
                guild_id=gid,
                requested=requested,
                kind=kind,
                actor_id=actor_id,
                event_key=event_key,
                destination=destination,
                recipient_id=recipient_id,
                clamp_to_balance=clamp_to_balance,
                min_balance=min_balance,
                metadata=metadata,
                occurred_at=occurred_at,
                mana_date=mana_date,
                now=now,
            )

    def apply_hostile_losses(
        self, losses: list[dict[str, Any]]
    ) -> list[HostileLossResult | Exception]:
        """Settle ordered hostile losses in one write transaction.

        Each victim is isolated by a savepoint so a rejected settlement is
        returned in its input position without discarding successful victims.
        Updates made by earlier victims remain visible to later ones, which is
        required for shared Sanctuary capacity and shared destinations.
        """
        if not losses:
            return []

        outcomes: list[HostileLossResult | Exception] = []
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            for index, loss in enumerate(losses):
                savepoint = f"hostile_loss_{index}"
                cursor.execute(f"SAVEPOINT {savepoint}")
                try:
                    normalized = dict(loss)
                    normalized["guild_id"] = self.normalize_guild_id(normalized["guild_id"])
                    outcome = self._apply_hostile_loss_with_cursor(
                        cursor,
                        **normalized,
                        now=int(time.time()),
                    )
                except Exception as exc:
                    cursor.execute(f"ROLLBACK TO SAVEPOINT {savepoint}")
                    cursor.execute(f"RELEASE SAVEPOINT {savepoint}")
                    outcomes.append(exc)
                    continue
                cursor.execute(f"RELEASE SAVEPOINT {savepoint}")
                outcomes.append(outcome)
        return outcomes

    def _apply_hostile_loss_with_cursor(
        self,
        cursor,
        *,
        victim_id: int,
        guild_id: int,
        requested: int,
        kind: HostileLossKind,
        actor_id: int | None,
        event_key: str,
        destination: HostileLossDestination,
        recipient_id: int | None,
        clamp_to_balance: bool,
        min_balance: int | None,
        metadata: dict,
        occurred_at: int,
        mana_date: str,
        now: int,
    ) -> HostileLossResult:
        """Apply one normalized settlement using the caller's transaction."""
        existing = cursor.execute(
            "SELECT * FROM hostile_loss_events "
            "WHERE guild_id = ? AND victim_id = ? AND event_key = ?",
            (guild_id, victim_id, event_key),
        ).fetchone()
        if existing is not None:
            expected = (
                requested,
                kind.value,
                destination.value,
                actor_id,
                recipient_id,
            )
            actual = (
                int(existing["requested"]),
                existing["kind"],
                existing["destination"],
                existing["actor_id"],
                existing["recipient_id"],
            )
            if actual != expected:
                raise ValueError("event_key already exists with a different payload")
            return self._row_to_result(existing, duplicate=True)

        victim = cursor.execute(
            "SELECT jopacoin_balance FROM players WHERE discord_id = ? AND guild_id = ?",
            (victim_id, guild_id),
        ).fetchone()
        if victim is None:
            raise ValueError("victim is not registered in this guild")
        victim_before = int(victim["jopacoin_balance"] or 0)

        attempted = requested
        if min_balance is not None and victim_before < min_balance:
            attempted = 0
        if clamp_to_balance:
            attempted = min(attempted, max(0, victim_before))

        destination_before: int | None = None
        if destination is HostileLossDestination.PLAYER:
            if recipient_id is None:
                raise ValueError("recipient_id is required for player destination")
            if recipient_id == victim_id:
                raise ValueError("recipient_id cannot be the victim")
            recipient = cursor.execute(
                "SELECT jopacoin_balance FROM players WHERE discord_id = ? AND guild_id = ?",
                (recipient_id, guild_id),
            ).fetchone()
            if recipient is None:
                raise ValueError("recipient is not registered in this guild")
            destination_before = int(recipient["jopacoin_balance"] or 0)
        elif destination is HostileLossDestination.RESERVE:
            reserve = cursor.execute(
                "SELECT total_collected FROM nonprofit_fund WHERE guild_id = ?",
                (guild_id,),
            ).fetchone()
            destination_before = int(reserve["total_collected"] or 0) if reserve else 0

        shieldable = actor_id != victim_id
        remaining = attempted
        details: list[ProtectionDetail] = []
        pool_keys: list[str] = []

        if shieldable and remaining > 0:
            counterspells = self._active_buffs(
                cursor,
                victim_id=victim_id,
                guild_id=guild_id,
                buff_type="counterspell",
                now=now,
            )
            if counterspells:
                row = counterspells[0]
                absorbed = remaining
                details.append(
                    ProtectionDetail(
                        source="counterspell",
                        absorbed=absorbed,
                        rate=1.0,
                        buff_id=int(row["id"]),
                    )
                )
                pool_keys.append(f"counterspell:{row['id']}")
                remaining = 0

        for buff_type, shared in (
            ("aegis", False),
            ("sanctuary", True),
            ("reprieve", False),
        ):
            if not shieldable or remaining <= 0:
                break
            for row in self._active_buffs(
                cursor,
                victim_id=victim_id,
                guild_id=guild_id,
                buff_type=buff_type,
                now=now,
                shared=shared,
            ):
                data, capacity, rate = self._decode_pool(row)
                if capacity <= 0 or rate <= 0:
                    self._persist_pool(cursor, row, data, 0)
                    continue
                rate_limit = self._rate_absorption(remaining, rate)
                absorbed = min(remaining, capacity, rate_limit)
                if absorbed <= 0:
                    continue
                after = capacity - absorbed
                self._persist_pool(cursor, row, data, after)
                details.append(
                    ProtectionDetail(
                        source=buff_type,
                        absorbed=absorbed,
                        rate=rate,
                        capacity_before=capacity,
                        capacity_after=after,
                        buff_id=int(row["id"]),
                    )
                )
                pool_keys.append(f"buff:{row['id']}")
                remaining -= absorbed
                if remaining <= 0:
                    break

        if shieldable and remaining > 0:
            mana = cursor.execute(
                """
                SELECT white_shield_remaining
                FROM player_mana
                WHERE discord_id = ? AND guild_id = ?
                  AND current_land = 'Plains' AND assigned_date = ?
                  AND consumed_today = 0
                """,
                (victim_id, guild_id, mana_date),
            ).fetchone()
            capacity = int(mana["white_shield_remaining"] or 0) if mana else 0
            if capacity > 0:
                absorbed = min(capacity, self._rate_absorption(remaining, 0.5))
                after = capacity - absorbed
                cursor.execute(
                    "UPDATE player_mana SET white_shield_remaining = ?, "
                    "updated_at = CURRENT_TIMESTAMP "
                    "WHERE discord_id = ? AND guild_id = ?",
                    (after, victim_id, guild_id),
                )
                details.append(
                    ProtectionDetail(
                        source="guardian",
                        absorbed=absorbed,
                        rate=0.5,
                        capacity_before=capacity,
                        capacity_after=after,
                    )
                )
                pool_keys.append(f"guardian:{mana_date}")
                remaining -= absorbed

        applied = remaining
        absorbed_total = attempted - applied
        victim_after = victim_before - applied
        destination_after = destination_before + applied if destination_before is not None else None
        details_json = self._details_json(details)
        metadata_json = json.dumps(metadata, sort_keys=True) if metadata else None

        cursor.execute(
            """
            INSERT INTO hostile_loss_events (
                guild_id, victim_id, actor_id, event_key, kind, destination,
                recipient_id, requested, attempted, absorbed, applied,
                victim_balance_before, victim_balance_after,
                destination_balance_before, destination_balance_after,
                shieldable, retro_covered, protection_details, metadata,
                occurred_at, created_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?, ?, ?)
            """,
            (
                guild_id,
                victim_id,
                actor_id,
                event_key,
                kind.value,
                destination.value,
                recipient_id,
                requested,
                attempted,
                absorbed_total,
                applied,
                victim_before,
                victim_after,
                destination_before,
                destination_after,
                int(shieldable),
                details_json,
                metadata_json,
                occurred_at,
                now,
            ),
        )
        event_id = int(cursor.lastrowid)

        for detail, pool_key in zip(details, pool_keys, strict=True):
            self._insert_protection_event(
                cursor,
                hostile_event_id=event_id,
                guild_id=guild_id,
                victim_id=victim_id,
                detail=detail,
                pool_key=pool_key,
                created_at=now,
            )

        if applied > 0:
            ledger_metadata = {
                **metadata,
                "kind": kind.value,
                "event_key": event_key,
                "requested": requested,
                "attempted": attempted,
                "absorbed": absorbed_total,
                "applied": applied,
                "destination": destination.value,
                "recipient_id": recipient_id,
            }
            self._set_economy_ledger_context(
                cursor,
                source="hostile_loss",
                actor_id=actor_id,
                related_type="hostile_loss_event",
                related_id=event_id,
                reason=f"{kind.value} hostile loss",
                metadata=ledger_metadata,
            )
            try:
                cursor.execute(
                    "UPDATE players SET jopacoin_balance = ?, "
                    "updated_at = CURRENT_TIMESTAMP "
                    "WHERE discord_id = ? AND guild_id = ?",
                    (victim_after, victim_id, guild_id),
                )
                if cursor.rowcount != 1:
                    raise ValueError("victim disappeared during settlement")
                cursor.execute(
                    """
                    UPDATE players
                    SET lowest_balance_ever = jopacoin_balance
                    WHERE discord_id = ? AND guild_id = ?
                      AND (lowest_balance_ever IS NULL
                           OR jopacoin_balance < lowest_balance_ever)
                    """,
                    (victim_id, guild_id),
                )
                if destination is HostileLossDestination.PLAYER:
                    cursor.execute(
                        "UPDATE players SET jopacoin_balance = ?, "
                        "updated_at = CURRENT_TIMESTAMP "
                        "WHERE discord_id = ? AND guild_id = ?",
                        (destination_after, recipient_id, guild_id),
                    )
                    if cursor.rowcount != 1:
                        raise ValueError("recipient disappeared during settlement")
                elif destination is HostileLossDestination.RESERVE:
                    cursor.execute(
                        """
                        INSERT INTO nonprofit_fund (
                            guild_id, total_collected, updated_at
                        )
                        VALUES (?, ?, CURRENT_TIMESTAMP)
                        ON CONFLICT(guild_id) DO UPDATE SET
                            total_collected = excluded.total_collected,
                            updated_at = CURRENT_TIMESTAMP
                        """,
                        (guild_id, destination_after),
                    )
            finally:
                self._clear_economy_ledger_context(cursor)

        row = cursor.execute(
            "SELECT * FROM hostile_loss_events WHERE event_id = ?",
            (event_id,),
        ).fetchone()
        return self._row_to_result(row, duplicate=False)

    def _eligible_retro_events(
        self,
        cursor,
        *,
        victim_id: int,
        guild_id: int,
        since_ts: int,
        through_ts: int,
        pool_key: str,
    ) -> list[Any]:
        return list(
            cursor.execute(
                """
                SELECT h.*
                FROM hostile_loss_events h
                WHERE h.guild_id = ? AND h.victim_id = ?
                  AND h.shieldable = 1
                  AND h.occurred_at >= ? AND h.occurred_at <= ?
                  AND h.applied > h.retro_covered
                  AND NOT EXISTS (
                      SELECT 1
                      FROM mana_protection_events p
                      WHERE p.hostile_loss_event_id = h.event_id
                        AND p.pool_key = ?
                  )
                ORDER BY h.occurred_at ASC, h.event_id ASC
                """,
                (guild_id, victim_id, since_ts, through_ts, pool_key),
            ).fetchall()
        )

    def _credit_retro_event(
        self,
        cursor,
        *,
        event: Any,
        guild_id: int,
        victim_id: int,
        amount: int,
        detail: ProtectionDetail,
        pool_key: str,
        now: int,
    ) -> None:
        self._set_economy_ledger_context(
            cursor,
            source="mana_protection",
            related_type="hostile_loss_event",
            related_id=event["event_id"],
            reason=f"retroactive {detail.source} reimbursement",
            metadata={
                "event_key": event["event_key"],
                "kind": event["kind"],
                "amount": amount,
                "protection_type": detail.source,
                "pool_key": pool_key,
            },
        )
        try:
            cursor.execute(
                "UPDATE players SET jopacoin_balance = "
                "COALESCE(jopacoin_balance, 0) + ?, "
                "updated_at = CURRENT_TIMESTAMP "
                "WHERE discord_id = ? AND guild_id = ?",
                (amount, victim_id, guild_id),
            )
            if cursor.rowcount != 1:
                raise ValueError("retroactive protection victim is not registered")
        finally:
            self._clear_economy_ledger_context(cursor)

        cursor.execute(
            "UPDATE hostile_loss_events "
            "SET retro_covered = retro_covered + ? WHERE event_id = ?",
            (amount, event["event_id"]),
        )
        self._insert_protection_event(
            cursor,
            hostile_event_id=int(event["event_id"]),
            guild_id=guild_id,
            victim_id=victim_id,
            detail=detail,
            pool_key=pool_key,
            created_at=now,
            details={"event_key": event["event_key"]},
        )

    def reconcile_guardian(
        self,
        *,
        discord_id: int,
        guild_id: int | None,
        since_ts: int,
        mana_date: str,
        now: int,
    ) -> int:
        """Retroactively reimburse eligible losses from the current mana day."""
        gid = self.normalize_guild_id(guild_id)
        pool_key = f"guardian:{mana_date}"
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            mana = cursor.execute(
                """
                SELECT white_shield_remaining
                FROM player_mana
                WHERE discord_id = ? AND guild_id = ?
                  AND current_land = 'Plains' AND assigned_date = ?
                  AND consumed_today = 0
                """,
                (discord_id, gid, mana_date),
            ).fetchone()
            capacity = int(mana["white_shield_remaining"] or 0) if mana else 0
            if capacity <= 0:
                return 0

            refunded = 0
            for event in self._eligible_retro_events(
                cursor,
                victim_id=discord_id,
                guild_id=gid,
                since_ts=since_ts,
                through_ts=now,
                pool_key=pool_key,
            ):
                uncovered = int(event["applied"]) - int(event["retro_covered"])
                amount = min(capacity, self._rate_absorption(uncovered, 0.5))
                if amount <= 0:
                    continue
                after = capacity - amount
                detail = ProtectionDetail(
                    source="guardian",
                    absorbed=amount,
                    rate=0.5,
                    capacity_before=capacity,
                    capacity_after=after,
                    retroactive=True,
                )
                self._credit_retro_event(
                    cursor,
                    event=event,
                    guild_id=gid,
                    victim_id=discord_id,
                    amount=amount,
                    detail=detail,
                    pool_key=pool_key,
                    now=now,
                )
                capacity = after
                refunded += amount
                if capacity <= 0:
                    break

            cursor.execute(
                "UPDATE player_mana SET white_shield_remaining = ?, "
                "updated_at = CURRENT_TIMESTAMP "
                "WHERE discord_id = ? AND guild_id = ?",
                (capacity, discord_id, gid),
            )
            return refunded

    def reconcile_purchased_pool(
        self,
        *,
        discord_id: int,
        guild_id: int | None,
        buff_id: int,
        since_ts: int,
        now: int,
    ) -> int:
        """Retroactively reimburse losses with a specified purchased pool."""
        gid = self.normalize_guild_id(guild_id)
        pool_key = f"buff:{buff_id}"
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            row = cursor.execute(
                """
                SELECT id, discord_id, target_id, buff_type, expires_at, data
                FROM manashop_buffs
                WHERE id = ? AND guild_id = ? AND discord_id = ?
                  AND triggered = 0 AND expires_at > ?
                """,
                (buff_id, gid, discord_id, now),
            ).fetchone()
            if row is None:
                return 0
            if row["buff_type"] not in _POOL_DEFAULTS:
                raise ValueError("buff is not a protection pool")
            data, capacity, rate = self._decode_pool(row)
            if row["buff_type"] != "reprieve" and not bool(
                data.get("rolling_retroactive", False)
            ):
                raise ValueError("protection pool is not retroactive")
            if capacity <= 0 or rate <= 0:
                self._persist_pool(cursor, row, data, 0)
                return 0

            refunded = 0
            for event in self._eligible_retro_events(
                cursor,
                victim_id=discord_id,
                guild_id=gid,
                since_ts=since_ts,
                through_ts=now,
                pool_key=pool_key,
            ):
                uncovered = int(event["applied"]) - int(event["retro_covered"])
                amount = min(capacity, self._rate_absorption(uncovered, rate))
                if amount <= 0:
                    continue
                after = capacity - amount
                detail = ProtectionDetail(
                    source=str(row["buff_type"]),
                    absorbed=amount,
                    rate=rate,
                    capacity_before=capacity,
                    capacity_after=after,
                    buff_id=int(row["id"]),
                    retroactive=True,
                )
                self._credit_retro_event(
                    cursor,
                    event=event,
                    guild_id=gid,
                    victim_id=discord_id,
                    amount=amount,
                    detail=detail,
                    pool_key=pool_key,
                    now=now,
                )
                capacity = after
                refunded += amount
                if capacity <= 0:
                    break

            self._persist_pool(cursor, row, data, capacity)
            return refunded

    def block_non_jc_attack(
        self,
        *,
        victim_id: int,
        guild_id: int | None,
        actor_id: int | None,
        event_key: str,
        occurred_at: int,
    ) -> NonJcProtectionResult:
        """Check persistent wards and consume Aegis for a non-JC attack."""
        gid = self.normalize_guild_id(guild_id)
        now = int(time.time())
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            existing = cursor.execute(
                "SELECT * FROM hostile_loss_events "
                "WHERE guild_id = ? AND victim_id = ? AND event_key = ?",
                (gid, victim_id, event_key),
            ).fetchone()
            if existing is not None:
                details = self._details_from_json(existing["protection_details"])
                detail = details[0] if details else None
                return NonJcProtectionResult(
                    blocked=detail is not None,
                    source=detail.source if detail else None,
                    buff_id=detail.buff_id if detail else None,
                    event_id=int(existing["event_id"]),
                    duplicate=True,
                )

            victim = cursor.execute(
                "SELECT jopacoin_balance FROM players "
                "WHERE discord_id = ? AND guild_id = ?",
                (victim_id, gid),
            ).fetchone()
            if victim is None:
                raise ValueError("victim is not registered in this guild")
            balance = int(victim["jopacoin_balance"] or 0)
            shieldable = actor_id != victim_id
            source: str | None = None
            buff_id: int | None = None

            if shieldable:
                for buff_type, shared, consume in (
                    ("counterspell", False, False),
                    ("sanctuary", True, False),
                    ("aegis", False, True),
                    ("first_aegis_today", False, True),
                ):
                    rows = self._active_buffs(
                        cursor,
                        victim_id=victim_id,
                        guild_id=gid,
                        buff_type=buff_type,
                        now=now,
                        shared=shared,
                    )
                    if not rows:
                        continue
                    row = rows[0]
                    source = buff_type
                    buff_id = int(row["id"])
                    if consume:
                        cursor.execute(
                            "UPDATE manashop_buffs SET triggered = 1 WHERE id = ?",
                            (buff_id,),
                        )
                    break

            details = (
                [
                    ProtectionDetail(
                        source=source,
                        absorbed=0,
                        rate=1.0,
                        buff_id=buff_id,
                    )
                ]
                if source is not None
                else []
            )
            cursor.execute(
                """
                INSERT INTO hostile_loss_events (
                    guild_id, victim_id, actor_id, event_key, kind, destination,
                    recipient_id, requested, attempted, absorbed, applied,
                    victim_balance_before, victim_balance_after,
                    destination_balance_before, destination_balance_after,
                    shieldable, retro_covered, protection_details, metadata,
                    occurred_at, created_at
                )
                VALUES (?, ?, ?, ?, 'sabotage', 'burn', NULL, 0, 0, 0, 0,
                        ?, ?, NULL, NULL, ?, 0, ?, NULL, ?, ?)
                """,
                (
                    gid,
                    victim_id,
                    actor_id,
                    event_key,
                    balance,
                    balance,
                    int(shieldable),
                    self._details_json(details),
                    occurred_at,
                    now,
                ),
            )
            event_id = int(cursor.lastrowid)
            if details:
                self._insert_protection_event(
                    cursor,
                    hostile_event_id=event_id,
                    guild_id=gid,
                    victim_id=victim_id,
                    detail=details[0],
                    pool_key=f"buff:{buff_id}",
                    created_at=now,
                    details={"non_jc": True},
                )
            return NonJcProtectionResult(
                blocked=source is not None,
                source=source,
                buff_id=buff_id,
                event_id=event_id,
            )

    def get_event(
        self, victim_id: int, guild_id: int | None, event_key: str
    ) -> dict | None:
        """Return a hostile-loss event for diagnostics and focused tests."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            row = conn.execute(
                "SELECT * FROM hostile_loss_events "
                "WHERE guild_id = ? AND victim_id = ? AND event_key = ?",
                (gid, victim_id, event_key),
            ).fetchone()
            return dict(row) if row else None
