"""
Repository for daily MTG mana land assignments.
"""

import time
from uuid import uuid4

from repositories.base_repository import BaseRepository
from repositories.interfaces import IManaRepository

_BANKRUPT_BUFF_COLUMNS = {
    "insurance": "bankrupt_insurance_used",
    "reroll": "bankrupt_reroll_used",
}
_PENDING_PURCHASE_STALE_SECONDS = 60


def _buff_column(buff: str) -> str:
    if buff not in _BANKRUPT_BUFF_COLUMNS:
        raise ValueError(f"Unknown bankrupt buff: {buff!r}")
    return _BANKRUPT_BUFF_COLUMNS[buff]


class ManaRepository(BaseRepository, IManaRepository):
    """Stores and retrieves daily mana assignments (one row per player per guild)."""

    def get_mana(self, discord_id: int, guild_id: int | None) -> dict | None:
        """Return {current_land, assigned_date} for the player, or None if never assigned."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT current_land, assigned_date, consumed_today, "
                "white_shield_remaining FROM player_mana "
                "WHERE discord_id = ? AND guild_id = ?",
                (discord_id, gid),
            )
            row = cursor.fetchone()
            return dict(row) if row else None

    def set_mana(self, discord_id: int, guild_id: int | None, land: str, assigned_date: str) -> None:
        """Upsert today's mana for the player (replaces any previous value)."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            conn.execute(
                """
                INSERT INTO player_mana (discord_id, guild_id, current_land, assigned_date, updated_at)
                VALUES (?, ?, ?, ?, CURRENT_TIMESTAMP)
                ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                    current_land  = excluded.current_land,
                    assigned_date = excluded.assigned_date,
                    updated_at    = CURRENT_TIMESTAMP
                """,
                (discord_id, gid, land, assigned_date),
            )

    def claim_mana_atomic(
        self, discord_id: int, guild_id: int | None, land: str, assigned_date: str
    ) -> bool:
        """Claim today's mana only if not already assigned for ``assigned_date``.

        Runs under BEGIN IMMEDIATE so two concurrent /mana calls can't both
        pass a pre-check and each roll a different land — the second caller
        observes the committed row and returns False.

        Resets the per-day bankruptcy buff flags so a new mana day grants fresh
        insurance / re-roll allowances.

        Returns True if the claim was applied, False if the player already
        has mana assigned for ``assigned_date``.
        """
        gid = self.normalize_guild_id(guild_id)
        white_shield_remaining = 25 if land == "Plains" else 0
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT assigned_date FROM player_mana WHERE discord_id = ? AND guild_id = ?",
                (discord_id, gid),
            )
            row = cursor.fetchone()
            if row and row["assigned_date"] == assigned_date:
                return False
            cursor.execute(
                """
                INSERT INTO player_mana (
                    discord_id, guild_id, current_land, assigned_date,
                    bankrupt_insurance_used, bankrupt_reroll_used, consumed_today,
                    white_shield_remaining, updated_at
                )
                VALUES (?, ?, ?, ?, 0, 0, 0, ?, CURRENT_TIMESTAMP)
                ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                    current_land            = excluded.current_land,
                    assigned_date           = excluded.assigned_date,
                    bankrupt_insurance_used = 0,
                    bankrupt_reroll_used    = 0,
                    consumed_today          = 0,
                    white_shield_remaining  = excluded.white_shield_remaining,
                    updated_at              = CURRENT_TIMESTAMP
                """,
                (
                    discord_id,
                    gid,
                    land,
                    assigned_date,
                    white_shield_remaining,
                ),
            )
            return True

    def claim_mana_batch_atomic(
        self,
        assignments: list[tuple[int, str]],
        guild_id: int | None,
        assigned_date: str,
    ) -> list[dict]:
        """Claim one day's mana for a player batch in a single transaction.

        The first assignment for each player wins if ``assignments`` contains
        duplicates. Results preserve that input order and contain only players
        whose row was inserted or advanced to ``assigned_date`` by this call.
        A concurrent single or batch claim is serialized by ``BEGIN IMMEDIATE``
        and therefore cannot be returned as newly claimed twice.
        """
        if not assignments:
            return []

        unique_assignments: list[tuple[int, str]] = []
        seen_ids: set[int] = set()
        for discord_id, land in assignments:
            if discord_id in seen_ids:
                continue
            seen_ids.add(discord_id)
            unique_assignments.append((discord_id, land))

        gid = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            already_claimed: set[int] = set()
            # Stay below SQLite's host-parameter limit for unusually large
            # guilds while retaining one connection and one write transaction.
            for offset in range(0, len(unique_assignments), 900):
                player_ids = [
                    discord_id for discord_id, _ in unique_assignments[offset : offset + 900]
                ]
                placeholders = ",".join("?" for _ in player_ids)
                rows = cursor.execute(
                    f"""
                    SELECT discord_id
                    FROM player_mana
                    WHERE guild_id = ? AND assigned_date = ?
                      AND discord_id IN ({placeholders})
                    """,
                    (gid, assigned_date, *player_ids),
                ).fetchall()
                already_claimed.update(int(row["discord_id"]) for row in rows)

            newly_claimed = [
                (discord_id, land)
                for discord_id, land in unique_assignments
                if discord_id not in already_claimed
            ]
            cursor.executemany(
                """
                INSERT INTO player_mana (
                    discord_id, guild_id, current_land, assigned_date,
                    bankrupt_insurance_used, bankrupt_reroll_used, consumed_today,
                    white_shield_remaining, updated_at
                )
                VALUES (?, ?, ?, ?, 0, 0, 0, ?, CURRENT_TIMESTAMP)
                ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                    current_land            = excluded.current_land,
                    assigned_date           = excluded.assigned_date,
                    bankrupt_insurance_used = 0,
                    bankrupt_reroll_used    = 0,
                    consumed_today          = 0,
                    white_shield_remaining  = excluded.white_shield_remaining,
                    updated_at              = CURRENT_TIMESTAMP
                """,
                [
                    (
                        discord_id,
                        gid,
                        land,
                        assigned_date,
                        25 if land == "Plains" else 0,
                    )
                    for discord_id, land in newly_claimed
                ],
            )

            return [
                {
                    "discord_id": discord_id,
                    "current_land": land,
                    "assigned_date": assigned_date,
                    "white_shield_remaining": 25 if land == "Plains" else 0,
                }
                for discord_id, land in newly_claimed
            ]

    def get_white_shield_remaining(self, discord_id: int, guild_id: int | None) -> int:
        """Return the player's current Guardian capacity, or zero."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            row = conn.execute(
                "SELECT white_shield_remaining FROM player_mana "
                "WHERE discord_id = ? AND guild_id = ?",
                (discord_id, gid),
            ).fetchone()
            return int(row["white_shield_remaining"] or 0) if row else 0

    def is_bankrupt_buff_used(
        self, discord_id: int, guild_id: int | None, buff: str
    ) -> bool:
        column = _buff_column(buff)
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"SELECT {column} FROM player_mana WHERE discord_id = ? AND guild_id = ?",  # noqa: S608
                (discord_id, gid),
            )
            row = cursor.fetchone()
            return bool(row and row[column])

    def claim_bankrupt_buff_atomic(
        self, discord_id: int, guild_id: int | None, buff: str
    ) -> bool:
        """Atomically set the buff flag if not already set today.

        Returns True if the flag flipped from 0 → 1 (claim succeeded), False if
        already used for the current mana day or no mana row exists.
        """
        column = _buff_column(buff)
        gid = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"SELECT {column} FROM player_mana WHERE discord_id = ? AND guild_id = ?",  # noqa: S608
                (discord_id, gid),
            )
            row = cursor.fetchone()
            if row is None or row[column]:
                return False
            cursor.execute(
                f"UPDATE player_mana SET {column} = 1, updated_at = CURRENT_TIMESTAMP "  # noqa: S608
                "WHERE discord_id = ? AND guild_id = ?",
                (discord_id, gid),
            )
            return True

    def is_mana_consumed(self, discord_id: int, guild_id: int | None) -> bool:
        """Return True if today's mana has been tapped (used on an ultimate)."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT consumed_today FROM player_mana "
                "WHERE discord_id = ? AND guild_id = ?",
                (discord_id, gid),
            )
            row = cursor.fetchone()
            return bool(row and row["consumed_today"])

    def mark_mana_consumed_atomic(
        self, discord_id: int, guild_id: int | None
    ) -> bool:
        """Atomically flip ``consumed_today`` 0→1.

        Returns True if the flag flipped (claim succeeded), False if already
        consumed today or no mana row exists. Uses BEGIN IMMEDIATE for
        race-safety against two concurrent /shop mana ultimate calls.
        """
        gid = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "UPDATE player_mana SET consumed_today = 1, "
                "updated_at = CURRENT_TIMESTAMP "
                "WHERE discord_id = ? AND guild_id = ? AND consumed_today = 0",
                (discord_id, gid),
            )
            return cursor.rowcount > 0

    def was_item_used_today(
        self, discord_id: int, guild_id: int | None, item_id: str, used_date: str
    ) -> bool:
        """Return True if a row exists in manashop_daily_uses for the
        (player, guild, item, date) tuple."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT 1 FROM manashop_daily_uses "
                "WHERE discord_id = ? AND guild_id = ? "
                "AND item_id = ? AND used_date = ?",
                (discord_id, gid, item_id, used_date),
            )
            return cursor.fetchone() is not None

    def mark_item_used_atomic(
        self, discord_id: int, guild_id: int | None, item_id: str, used_date: str
    ) -> bool:
        """Atomically insert a daily-use row. Returns True on first use,
        False if the (player, guild, item, date) row already exists."""
        gid = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "INSERT OR IGNORE INTO manashop_daily_uses "
                "(discord_id, guild_id, item_id, used_date) VALUES (?, ?, ?, ?)",
                (discord_id, gid, item_id, used_date),
            )
            return cursor.rowcount > 0

    def try_purchase_item_atomic(
        self,
        discord_id: int,
        guild_id: int | None,
        item_id: str,
        used_date: str,
        *,
        cost: int,
        tap_mana: bool,
    ) -> dict[str, int | str | bool | None]:
        """Claim, conditionally charge, and optionally tap a mana item atomically."""
        if cost < 0:
            raise ValueError("Manashop cost cannot be negative")

        gid = self.normalize_guild_id(guild_id)
        purchase_id = uuid4().hex
        now = int(time.time())
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            player_row = cursor.execute(
                """
                SELECT COALESCE(jopacoin_balance, 0) AS balance
                FROM players
                WHERE discord_id = ? AND guild_id = ?
                """,
                (discord_id, gid),
            ).fetchone()
            if player_row is None:
                return {
                    "success": False,
                    "reason": "not_registered",
                    "balance": 0,
                }
            balance = int(player_row["balance"])

            already_used = cursor.execute(
                """
                SELECT uses.purchase_id, purchases.status, purchases.cost,
                       purchases.tap_mana, purchases.updated_at
                FROM manashop_daily_uses AS uses
                LEFT JOIN manashop_purchases AS purchases
                  ON purchases.purchase_id = uses.purchase_id
                WHERE uses.discord_id = ? AND uses.guild_id = ?
                  AND uses.item_id = ? AND uses.used_date = ?
                """,
                (discord_id, gid, item_id, used_date),
            ).fetchone()
            if already_used is not None:
                stale_pending = (
                    already_used["purchase_id"] is not None
                    and already_used["status"] == "pending"
                    and int(already_used["updated_at"] or 0)
                    <= now - _PENDING_PURCHASE_STALE_SECONDS
                )
                if not stale_pending:
                    return {
                        "success": False,
                        "reason": (
                            "purchase_in_progress"
                            if already_used["status"] == "pending"
                            else "already_used"
                        ),
                        "balance": balance,
                    }

                stale_purchase_id = str(already_used["purchase_id"])
                stale_cost = int(already_used["cost"])
                cursor.execute(
                    """
                    UPDATE manashop_purchases
                    SET status = 'refunded', updated_at = ?
                    WHERE purchase_id = ? AND status = 'pending'
                    """,
                    (now, stale_purchase_id),
                )
                if cursor.rowcount != 1:
                    raise RuntimeError("Pending manashop purchase changed during recovery")
                cursor.execute(
                    "DELETE FROM manashop_daily_uses WHERE purchase_id = ?",
                    (stale_purchase_id,),
                )
                cursor.execute(
                    """
                    UPDATE players
                    SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?,
                        updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    (stale_cost, discord_id, gid),
                )
                balance += stale_cost
                if bool(already_used["tap_mana"]):
                    cursor.execute(
                        """
                        UPDATE player_mana
                        SET consumed_today = 0, updated_at = CURRENT_TIMESTAMP
                        WHERE discord_id = ? AND guild_id = ?
                          AND NOT EXISTS (
                              SELECT 1
                              FROM manashop_purchases
                              WHERE discord_id = ? AND guild_id = ?
                                AND used_date = ? AND tap_mana = 1
                                AND status IN ('applying', 'completed')
                          )
                        """,
                        (discord_id, gid, discord_id, gid, used_date),
                    )

            if tap_mana:
                mana_row = cursor.execute(
                    """
                    SELECT consumed_today
                    FROM player_mana
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    (discord_id, gid),
                ).fetchone()
                if mana_row is None or bool(mana_row["consumed_today"]):
                    return {
                        "success": False,
                        "reason": "mana_tapped",
                        "balance": balance,
                    }

            if balance < cost:
                return {
                    "success": False,
                    "reason": "insufficient_balance",
                    "balance": balance,
                }

            cursor.execute(
                """
                INSERT INTO manashop_purchases
                    (purchase_id, discord_id, guild_id, item_id, used_date,
                     cost, tap_mana, status, created_at, updated_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, 'pending', ?, ?)
                """,
                (
                    purchase_id,
                    discord_id,
                    gid,
                    item_id,
                    used_date,
                    cost,
                    int(tap_mana),
                    now,
                    now,
                ),
            )
            cursor.execute(
                """
                INSERT INTO manashop_daily_uses
                    (discord_id, guild_id, item_id, used_date, purchase_id)
                VALUES (?, ?, ?, ?, ?)
                """,
                (discord_id, gid, item_id, used_date, purchase_id),
            )
            if tap_mana:
                cursor.execute(
                    """
                    UPDATE player_mana
                    SET consumed_today = 1, updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ? AND consumed_today = 0
                    """,
                    (discord_id, gid),
                )
                if cursor.rowcount != 1:
                    raise RuntimeError("Mana tap changed during atomic purchase")

            cursor.execute(
                """
                UPDATE players
                SET jopacoin_balance = COALESCE(jopacoin_balance, 0) - ?,
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                  AND COALESCE(jopacoin_balance, 0) >= ?
                """,
                (cost, discord_id, gid, cost),
            )
            if cursor.rowcount != 1:
                raise RuntimeError("Player balance changed during atomic purchase")
            cursor.execute(
                """
                UPDATE players
                SET lowest_balance_ever = jopacoin_balance
                WHERE discord_id = ? AND guild_id = ?
                  AND (lowest_balance_ever IS NULL OR jopacoin_balance < lowest_balance_ever)
                """,
                (discord_id, gid),
            )
            return {
                "success": True,
                "reason": None,
                "balance": balance - cost,
                "purchase_id": purchase_id,
                "status": "pending",
            }

    def get_item_purchase(self, purchase_id: str) -> dict | None:
        """Return one immutable purchase identity and its lifecycle status."""
        with self.connection() as conn:
            row = conn.execute(
                """
                SELECT purchase_id, discord_id, guild_id, item_id, used_date,
                       cost, tap_mana, status, created_at, updated_at
                FROM manashop_purchases
                WHERE purchase_id = ?
                """,
                (purchase_id,),
            ).fetchone()
            return dict(row) if row else None

    def mark_item_purchase_applying_atomic(self, purchase_id: str) -> bool:
        """Move a durable purchase into effect delivery exactly once."""
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE manashop_purchases
                SET status = 'applying', updated_at = ?
                WHERE purchase_id = ? AND status = 'pending'
                """,
                (int(time.time()), purchase_id),
            )
            return cursor.rowcount == 1

    def complete_item_purchase_atomic(self, purchase_id: str) -> bool:
        """Mark effect delivery complete without reopening a daily slot."""
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE manashop_purchases
                SET status = 'completed', updated_at = ?
                WHERE purchase_id = ? AND status = 'applying'
                """,
                (int(time.time()), purchase_id),
            )
            return cursor.rowcount == 1

    def refund_item_purchase_atomic(self, purchase_id: str) -> bool:
        """Reverse the exact stored debit/tap once, keyed by purchase identity."""
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            purchase = cursor.execute(
                """
                SELECT discord_id, guild_id, used_date, cost, tap_mana, status
                FROM manashop_purchases
                WHERE purchase_id = ?
                """,
                (purchase_id,),
            ).fetchone()
            if purchase is None or purchase["status"] not in ("pending", "applying"):
                return False
            discord_id = int(purchase["discord_id"])
            gid = int(purchase["guild_id"])
            cost = int(purchase["cost"])
            cursor.execute(
                """
                UPDATE manashop_purchases
                SET status = 'refunded', updated_at = ?
                WHERE purchase_id = ? AND status IN ('pending', 'applying')
                """,
                (int(time.time()), purchase_id),
            )
            if cursor.rowcount != 1:
                return False

            cursor.execute(
                "DELETE FROM manashop_daily_uses WHERE purchase_id = ?",
                (purchase_id,),
            )

            cursor.execute(
                """
                UPDATE players
                SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?,
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (cost, discord_id, gid),
            )
            if cursor.rowcount != 1:
                raise RuntimeError("Purchased player no longer exists")
            if bool(purchase["tap_mana"]):
                cursor.execute(
                    """
                    UPDATE player_mana
                    SET consumed_today = 0, updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                      AND NOT EXISTS (
                          SELECT 1
                          FROM manashop_purchases
                          WHERE discord_id = ? AND guild_id = ?
                            AND used_date = ? AND tap_mana = 1
                            AND status IN ('pending', 'applying', 'completed')
                      )
                    """,
                    (
                        discord_id,
                        gid,
                        discord_id,
                        gid,
                        purchase["used_date"],
                    ),
                )
            return True

    def unmark_item_used(
        self, discord_id: int, guild_id: int | None, item_id: str, used_date: str
    ) -> None:
        """Delete the daily-use row so the slot is released on failure.

        Called from _refund paths that fire after mark_item_used_atomic succeeded
        but before the item effect ran. Idempotent: no-op if the row is absent.
        """
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            conn.execute(
                "DELETE FROM manashop_daily_uses "
                "WHERE discord_id = ? AND guild_id = ? AND item_id = ? AND used_date = ?",
                (discord_id, gid, item_id, used_date),
            )

    def get_all_mana(self, guild_id: int | None) -> list[dict]:
        """Return all mana rows for the guild, ordered by current_land then discord_id."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT discord_id, current_land, assigned_date, consumed_today
                FROM player_mana
                WHERE guild_id = ?
                ORDER BY current_land, discord_id
                """,
                (gid,),
            )
            return [dict(row) for row in cursor.fetchall()]
