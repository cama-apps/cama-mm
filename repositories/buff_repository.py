"""Repository for time-limited manashop buffs.

Wraps the ``manashop_buffs`` table. Buffs are produced by manashop ultimates
(Counterspell, Aegis, Overgrowth, Sanctuary, Blood Pact, Dark Bargain) and
24h Mid items like Aegis. Each row is a single buff instance with a hard
``expires_at`` epoch second; ``triggered`` flips to 1 when the buff is
consumed (e.g. Aegis absorbing one PvP attack) for its lifetime.
"""

import json
import time
from typing import Any

from repositories.base_repository import BaseRepository, safe_json_loads


class BuffRepository(BaseRepository):
    """Stores time-limited manashop buffs."""

    def grant(
        self,
        discord_id: int,
        guild_id: int | None,
        buff_type: str,
        expires_at: int,
        *,
        target_id: int | None = None,
        data: dict | None = None,
    ) -> int:
        """Insert a new buff row. Returns the new buff id."""
        gid = self.normalize_guild_id(guild_id)
        granted_at = int(time.time())
        data_json = json.dumps(data) if data else None
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO manashop_buffs
                (discord_id, guild_id, buff_type, target_id, granted_at, expires_at, triggered, data)
                VALUES (?, ?, ?, ?, ?, ?, 0, ?)
                """,
                (discord_id, gid, buff_type, target_id, granted_at, expires_at, data_json),
            )
            return int(cursor.lastrowid or 0)

    def active_for(
        self, discord_id: int, guild_id: int | None, buff_type: str
    ) -> list[dict]:
        """Return all non-triggered, non-expired buffs of ``buff_type`` owned
        by ``discord_id``. Most recent first."""
        gid = self.normalize_guild_id(guild_id)
        now = int(time.time())
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT id, discord_id, guild_id, buff_type, target_id,
                       granted_at, expires_at, triggered, data
                FROM manashop_buffs
                WHERE discord_id = ? AND guild_id = ? AND buff_type = ?
                  AND triggered = 0 AND expires_at > ?
                ORDER BY granted_at DESC
                """,
                (discord_id, gid, buff_type, now),
            )
            rows = [dict(row) for row in cursor.fetchall()]
            for row in rows:
                row["data"] = safe_json_loads(row.get("data"), {}, context="manashop_buffs.data")
            return rows

    def has_active(
        self, discord_id: int, guild_id: int | None, buff_type: str
    ) -> bool:
        """Return True if any non-triggered, non-expired buff of ``buff_type``
        exists for the player."""
        return bool(self.active_for(discord_id, guild_id, buff_type))

    def active_targeted_at(
        self, target_id: int, guild_id: int | None, buff_type: str
    ) -> list[dict]:
        """Return all non-triggered, non-expired buffs of ``buff_type`` whose
        ``target_id`` is the given player (e.g. Sanctuary on an ally,
        Blood Pact on a victim)."""
        gid = self.normalize_guild_id(guild_id)
        now = int(time.time())
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT id, discord_id, guild_id, buff_type, target_id,
                       granted_at, expires_at, triggered, data
                FROM manashop_buffs
                WHERE target_id = ? AND guild_id = ? AND buff_type = ?
                  AND triggered = 0 AND expires_at > ?
                ORDER BY granted_at DESC
                """,
                (target_id, gid, buff_type, now),
            )
            rows = [dict(row) for row in cursor.fetchall()]
            for row in rows:
                row["data"] = safe_json_loads(row.get("data"), {}, context="manashop_buffs.data")
            return rows

    def refresh_atomic(
        self,
        discord_id: int,
        guild_id: int | None,
        buff_type: str,
        expires_at: int,
        *,
        target_id: int | None = None,
        data: dict | None = None,
    ) -> int:
        """Atomically expire all active buffs of ``buff_type`` for the player
        and insert a fresh one. Closes the consume-then-grant race so
        concurrent re-purchases cannot leave two active rows.
        """
        gid = self.normalize_guild_id(guild_id)
        granted_at = int(time.time())
        data_json = json.dumps(data) if data else None
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "UPDATE manashop_buffs SET triggered = 1 "
                "WHERE discord_id = ? AND guild_id = ? AND buff_type = ? "
                "AND triggered = 0 AND expires_at > ?",
                (discord_id, gid, buff_type, granted_at),
            )
            cursor.execute(
                """
                INSERT INTO manashop_buffs
                (discord_id, guild_id, buff_type, target_id, granted_at, expires_at, triggered, data)
                VALUES (?, ?, ?, ?, ?, ?, 0, ?)
                """,
                (discord_id, gid, buff_type, target_id, granted_at, expires_at, data_json),
            )
            return int(cursor.lastrowid or 0)

    def consume_atomic(self, buff_id: int) -> bool:
        """Atomically mark a buff as triggered. Returns True if the flag
        flipped (claim succeeded), False if it was already triggered or the
        row doesn't exist."""
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "UPDATE manashop_buffs SET triggered = 1 "
                "WHERE id = ? AND triggered = 0",
                (buff_id,),
            )
            return cursor.rowcount > 0

    def update_data(self, buff_id: int, data: dict[str, Any]) -> None:
        """Overwrite the JSON ``data`` blob for a buff. Used for buffs that
        accumulate state (e.g. Blood Pact's running skim total)."""
        data_json = json.dumps(data)
        with self.connection() as conn:
            conn.execute(
                "UPDATE manashop_buffs SET data = ? WHERE id = ?",
                (data_json, buff_id),
            )

    def claim_blood_pact_skim_atomic(
        self,
        target_id: int,
        guild_id: int | None,
        earning: int,
    ) -> dict | None:
        """Atomically reserve a Blood Pact skim against ``target_id``'s earnings.

        Returns ``{buff_id, skimmer_id, amount, new_total}`` when a skim was
        reserved, else ``None``. Callers must transfer balances and call
        ``revert_blood_pact_skim`` if the transfer fails.
        """
        if earning <= 0:
            return None
        gid = self.normalize_guild_id(guild_id)
        now = int(time.time())
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT id, discord_id, data
                FROM manashop_buffs
                WHERE target_id = ? AND guild_id = ? AND buff_type = 'blood_pact'
                  AND triggered = 0 AND expires_at > ?
                ORDER BY granted_at DESC
                LIMIT 1
                """,
                (target_id, gid, now),
            )
            row = cursor.fetchone()
            if row is None:
                return None

            data = safe_json_loads(row["data"], {}, context="manashop_buffs.data")
            cap = int(data.get("cap") or 0)
            skimmed_total = int(data.get("skimmed_total") or 0)
            remaining = cap - skimmed_total
            if remaining <= 0:
                return None
            skim_rate = float(data.get("skim_rate") or 0)
            amount = min(remaining, max(1, int(earning * skim_rate)))
            if amount <= 0:
                return None

            new_total = skimmed_total + amount
            data["skimmed_total"] = new_total
            cursor.execute(
                "UPDATE manashop_buffs SET data = ? WHERE id = ?",
                (json.dumps(data), row["id"]),
            )
            return {
                "buff_id": int(row["id"]),
                "skimmer_id": int(row["discord_id"]),
                "amount": amount,
                "new_total": new_total,
            }

    def revert_blood_pact_skim(self, buff_id: int, amount: int) -> None:
        """Undo a reserved skim when the balance transfer could not complete."""
        if amount <= 0:
            return
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT data FROM manashop_buffs WHERE id = ?",
                (buff_id,),
            )
            row = cursor.fetchone()
            if row is None:
                return
            data = safe_json_loads(row["data"], {}, context="manashop_buffs.data")
            skimmed_total = int(data.get("skimmed_total") or 0)
            data["skimmed_total"] = max(0, skimmed_total - amount)
            cursor.execute(
                "UPDATE manashop_buffs SET data = ? WHERE id = ?",
                (json.dumps(data), buff_id),
            )

    def consume_data_charge_atomic(
        self,
        discord_id: int,
        guild_id: int | None,
        buff_type: str,
        data_key: str,
    ) -> bool:
        """Atomically decrement a positive integer charge in a buff data blob.

        Marks the buff triggered when the consumed charge was the last one.
        Returns False when no active charged buff exists.
        """
        gid = self.normalize_guild_id(guild_id)
        now = int(time.time())
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT id, data
                FROM manashop_buffs
                WHERE discord_id = ? AND guild_id = ? AND buff_type = ?
                  AND triggered = 0 AND expires_at > ?
                ORDER BY granted_at DESC
                LIMIT 1
                """,
                (discord_id, gid, buff_type, now),
            )
            row = cursor.fetchone()
            if row is None:
                return False

            data = safe_json_loads(row["data"], {}, context="manashop_buffs.data")
            charges = int(data.get(data_key) or 0)
            if charges <= 0:
                cursor.execute(
                    "UPDATE manashop_buffs SET triggered = 1 WHERE id = ?",
                    (row["id"],),
                )
                return False

            data[data_key] = charges - 1
            if data[data_key] <= 0:
                cursor.execute(
                    "UPDATE manashop_buffs SET data = ?, triggered = 1 WHERE id = ?",
                    (json.dumps(data), row["id"]),
                )
            else:
                cursor.execute(
                    "UPDATE manashop_buffs SET data = ? WHERE id = ?",
                    (json.dumps(data), row["id"]),
                )
            return True

    def settle_due_dark_bargains(
        self,
        *,
        now: int,
    ) -> list[dict]:
        """Settle expired Dark Bargain debts exactly once."""
        results: list[dict] = []
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT id, discord_id, guild_id, data
                FROM manashop_buffs
                WHERE buff_type = 'dark_bargain'
                  AND triggered = 0
                  AND expires_at <= ?
                ORDER BY expires_at ASC
                """,
                (now,),
            )
            debts = [dict(row) for row in cursor.fetchall()]

            for debt in debts:
                data = safe_json_loads(debt.get("data"), {}, context="manashop_buffs.data")
                discord_id = int(debt["discord_id"])
                guild_id = int(debt["guild_id"])
                amount_due = int(data.get("amount_due") or 0)
                default_penalty = int(data.get("default_penalty") or 1600)
                default_penalty_games = int(data.get("default_penalty_games") or 5)

                cursor.execute(
                    """
                    SELECT COALESCE(jopacoin_balance, 0) as balance
                    FROM players
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    (discord_id, guild_id),
                )
                row = cursor.fetchone()
                if row is None:
                    results.append({
                        "discord_id": discord_id,
                        "guild_id": guild_id,
                        "status": "missing_player",
                        "amount": 0,
                    })
                    continue

                if int(row["balance"]) >= amount_due:
                    amount = amount_due
                    status = "paid"
                else:
                    amount = default_penalty
                    status = "defaulted"

                cursor.execute(
                    """
                    UPDATE players
                    SET jopacoin_balance = COALESCE(jopacoin_balance, 0) - ?,
                        updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    (amount, discord_id, guild_id),
                )
                cursor.execute(
                    """
                    UPDATE players
                    SET lowest_balance_ever = jopacoin_balance
                    WHERE discord_id = ? AND guild_id = ?
                      AND (lowest_balance_ever IS NULL OR jopacoin_balance < lowest_balance_ever)
                    """,
                    (discord_id, guild_id),
                )
                if status == "defaulted":
                    cursor.execute(
                        """
                        INSERT INTO bankruptcy_state (
                            discord_id, guild_id, last_bankruptcy_at,
                            penalty_games_remaining, bankruptcy_count, updated_at
                        )
                        VALUES (?, ?, ?, ?, 1, CURRENT_TIMESTAMP)
                        ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                            last_bankruptcy_at = excluded.last_bankruptcy_at,
                            penalty_games_remaining = bankruptcy_state.penalty_games_remaining
                                + excluded.penalty_games_remaining,
                            bankruptcy_count = COALESCE(bankruptcy_state.bankruptcy_count, 0) + 1,
                            updated_at = CURRENT_TIMESTAMP
                        """,
                        (discord_id, guild_id, now, default_penalty_games),
                    )
                cursor.execute(
                    "UPDATE manashop_buffs SET triggered = 1 WHERE id = ?",
                    (debt["id"],),
                )
                results.append({
                    "discord_id": discord_id,
                    "guild_id": guild_id,
                    "status": status,
                    "amount": amount,
                })

        return results

    def cleanup_expired(self, *, before: int | None = None) -> int:
        """Delete expired/triggered buff rows. Returns number of rows pruned.

        Lazy maintenance — call from a daily reset path or periodic cleanup.
        """
        cutoff = before if before is not None else int(time.time())
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "DELETE FROM manashop_buffs "
                "WHERE triggered = 1 OR (expires_at <= ? AND buff_type != 'dark_bargain')",
                (cutoff,),
            )
            return cursor.rowcount or 0
