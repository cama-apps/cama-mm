"""Repository for pet brawls: challenge lifecycle rows and hunger settlement.

No jopacoin ever moves through a brawl — the only pet mutation is the
hunger settlement at the end, done inside one atomic transaction that
re-reads both pets so a death claimed mid-battle can never be overwritten
(writing fresh anchors onto a starved pet would resurrect it).

Validation errors are raised as ``ValueError(code)``:
    brawl_busy, no_brawl, not_recipient, not_challenger, not_pending,
    expired, not_open, not_active, already_done
"""

from __future__ import annotations

import sqlite3

from domain.models.pet import Pet
from domain.models.pet_brawl import PetBrawl, PetBrawlStatus
from repositories.base_repository import BaseRepository
from repositories.pet_repository import write_hunger_anchor
from utils.game_date import game_day_start_ts

_BRAWL_COLUMNS = (
    "brawl_id, guild_id, channel_id, challenger_id, recipient_id, "
    "challenger_pet_id, recipient_pet_id, status, created_at, expires_at, "
    "resolved_at, winner_id, winner_pet_id, loser_pet_id, rounds, "
    "winner_hunger_delta, loser_hunger_delta"
)


def _row_to_brawl(row: sqlite3.Row) -> PetBrawl:
    return PetBrawl.from_row(dict(row))


class PetBrawlRepository(BaseRepository):
    # --- reads ---

    def get_brawl(self, brawl_id: int, guild_id: int | None) -> PetBrawl | None:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            row = conn.execute(
                f"SELECT {_BRAWL_COLUMNS} FROM pet_brawls "
                "WHERE brawl_id = ? AND guild_id = ?",
                (brawl_id, gid),
            ).fetchone()
        return _row_to_brawl(row) if row else None

    def get_pet_record(self, pet_id: int, guild_id: int | None) -> tuple[int, int]:
        """(wins, losses) for a pet, derived from done rows."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            wins = conn.execute(
                "SELECT COUNT(*) AS n FROM pet_brawls "
                "WHERE winner_pet_id = ? AND guild_id = ? AND status = 'done'",
                (pet_id, gid),
            ).fetchone()["n"]
            losses = conn.execute(
                "SELECT COUNT(*) AS n FROM pet_brawls "
                "WHERE loser_pet_id = ? AND guild_id = ? AND status = 'done'",
                (pet_id, gid),
            ).fetchone()["n"]
        return int(wins), int(losses)

    def get_records_for(
        self, pet_ids: list[int], guild_id: int | None
    ) -> dict[int, tuple[int, int]]:
        """W/L per pet for a batch (leaderboard annotation)."""
        if not pet_ids:
            return {}
        gid = self.normalize_guild_id(guild_id)
        placeholders = ", ".join("?" for _ in pet_ids)
        records = {pet_id: [0, 0] for pet_id in pet_ids}
        with self.connection() as conn:
            for row in conn.execute(
                f"SELECT winner_pet_id AS pet_id, COUNT(*) AS n FROM pet_brawls "
                f"WHERE guild_id = ? AND status = 'done' "
                f"AND winner_pet_id IN ({placeholders}) GROUP BY winner_pet_id",
                (gid, *pet_ids),
            ):
                records[row["pet_id"]][0] = row["n"]
            for row in conn.execute(
                f"SELECT loser_pet_id AS pet_id, COUNT(*) AS n FROM pet_brawls "
                f"WHERE guild_id = ? AND status = 'done' "
                f"AND loser_pet_id IN ({placeholders}) GROUP BY loser_pet_id",
                (gid, *pet_ids),
            ):
                records[row["pet_id"]][1] = row["n"]
        return {pet_id: (w, losses) for pet_id, (w, losses) in records.items()}

    # --- lifecycle ---

    def create_brawl_atomic(
        self,
        guild_id: int | None,
        channel_id: int,
        challenger_id: int,
        recipient_id: int,
        challenger_pet_id: int,
        *,
        now: int,
        expires_at: int,
    ) -> PetBrawl:
        gid = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            # One open brawl per player, in either role. A partial unique
            # index can't express the cross-role case, so the invariant is
            # enforced here under BEGIN IMMEDIATE.
            busy = cursor.execute(
                "SELECT 1 FROM pet_brawls "
                "WHERE guild_id = ? AND status IN ('pending', 'active') "
                "AND (challenger_id IN (?, ?) OR recipient_id IN (?, ?)) "
                "LIMIT 1",
                (gid, challenger_id, recipient_id, challenger_id, recipient_id),
            ).fetchone()
            if busy:
                raise ValueError("brawl_busy")
            cursor.execute(
                "INSERT INTO pet_brawls (guild_id, channel_id, challenger_id, "
                "recipient_id, challenger_pet_id, status, created_at, expires_at) "
                "VALUES (?, ?, ?, ?, ?, 'pending', ?, ?)",
                (
                    gid,
                    channel_id,
                    challenger_id,
                    recipient_id,
                    challenger_pet_id,
                    now,
                    expires_at,
                ),
            )
            row = cursor.execute(
                f"SELECT {_BRAWL_COLUMNS} FROM pet_brawls WHERE brawl_id = ?",
                (cursor.lastrowid,),
            ).fetchone()
        return _row_to_brawl(row)

    def accept_atomic(
        self,
        brawl_id: int,
        guild_id: int | None,
        recipient_id: int,
        recipient_pet_id: int,
        now: int,
    ) -> PetBrawl:
        gid = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "UPDATE pet_brawls SET status = 'active', recipient_pet_id = ? "
                "WHERE brawl_id = ? AND guild_id = ? AND recipient_id = ? "
                "AND status = 'pending' AND expires_at > ?",
                (recipient_pet_id, brawl_id, gid, recipient_id, now),
            )
            if cursor.rowcount != 1:
                self._raise_transition_error(cursor, brawl_id, gid, recipient_id, now)
            row = cursor.execute(
                f"SELECT {_BRAWL_COLUMNS} FROM pet_brawls WHERE brawl_id = ?",
                (brawl_id,),
            ).fetchone()
        return _row_to_brawl(row)

    def decline_atomic(
        self, brawl_id: int, guild_id: int | None, recipient_id: int, now: int
    ) -> None:
        gid = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "UPDATE pet_brawls SET status = 'declined', resolved_at = ? "
                "WHERE brawl_id = ? AND guild_id = ? AND recipient_id = ? "
                "AND status = 'pending'",
                (now, brawl_id, gid, recipient_id),
            )
            if cursor.rowcount != 1:
                self._raise_transition_error(cursor, brawl_id, gid, recipient_id, now)

    def withdraw_atomic(
        self, brawl_id: int, guild_id: int | None, challenger_id: int, now: int
    ) -> None:
        """Void a still-pending challenge, on behalf of its challenger only."""
        gid = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "UPDATE pet_brawls SET status = 'void', resolved_at = ? "
                "WHERE brawl_id = ? AND guild_id = ? AND challenger_id = ? "
                "AND status = 'pending'",
                (now, brawl_id, gid, challenger_id),
            )
            if cursor.rowcount != 1:
                row = cursor.execute(
                    "SELECT challenger_id FROM pet_brawls "
                    "WHERE brawl_id = ? AND guild_id = ?",
                    (brawl_id, gid),
                ).fetchone()
                if row is None:
                    raise ValueError("no_brawl")
                if row["challenger_id"] != challenger_id:
                    raise ValueError("not_challenger")
                raise ValueError("not_pending")

    def void_atomic(self, brawl_id: int, guild_id: int | None, now: int) -> None:
        """Void an open brawl (withdrawn, invalidated, or abandoned)."""
        gid = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "UPDATE pet_brawls SET status = 'void', resolved_at = ? "
                "WHERE brawl_id = ? AND guild_id = ? "
                "AND status IN ('pending', 'active')",
                (now, brawl_id, gid),
            )
            if cursor.rowcount != 1:
                raise ValueError("not_open")

    def sweep_stale(
        self, now: int, *, active_ttl_seconds: int
    ) -> dict[str, int]:
        """Expire overdue challenges; void battles stuck past the TTL."""
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "UPDATE pet_brawls SET status = 'expired', resolved_at = ? "
                "WHERE status = 'pending' AND expires_at <= ?",
                (now, now),
            )
            expired = cursor.rowcount
            cursor.execute(
                "UPDATE pet_brawls SET status = 'void', resolved_at = ? "
                "WHERE status = 'active' AND created_at < ?",
                (now, now - active_ttl_seconds),
            )
            voided = cursor.rowcount
        return {"expired": expired, "voided": voided}

    def _raise_transition_error(
        self,
        cursor: sqlite3.Cursor,
        brawl_id: int,
        gid: int,
        recipient_id: int,
        now: int,
    ) -> None:
        row = cursor.execute(
            "SELECT recipient_id, status, expires_at FROM pet_brawls "
            "WHERE brawl_id = ? AND guild_id = ?",
            (brawl_id, gid),
        ).fetchone()
        if row is None:
            raise ValueError("no_brawl")
        if row["recipient_id"] != recipient_id:
            raise ValueError("not_recipient")
        if row["status"] == PetBrawlStatus.PENDING and row["expires_at"] <= now:
            raise ValueError("expired")
        raise ValueError("not_pending")

    # --- settlement ---

    def settle_brawl_atomic(
        self,
        brawl_id: int,
        guild_id: int | None,
        *,
        winner_id: int,
        winner_pet_id: int,
        loser_pet_id: int,
        rounds: int,
        now: int,
        decay_per_day: int,
        winner_gain: int,
        loser_loss: int,
        loss_floor: int,
        daily_win_cap: int,
    ) -> dict:
        """Apply hunger stakes and finalize the row, all in one transaction.

        Never sets died_at and never writes anchors onto a pet that is dead
        or derived-starved — a pet that starved mid-battle keeps its original
        starvation time for the lazy death claim.
        """
        gid = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            row = cursor.execute(
                "SELECT status FROM pet_brawls WHERE brawl_id = ? AND guild_id = ?",
                (brawl_id, gid),
            ).fetchone()
            if row is None:
                raise ValueError("no_brawl")
            if row["status"] == PetBrawlStatus.DONE:
                raise ValueError("already_done")
            if row["status"] != PetBrawlStatus.ACTIVE:
                raise ValueError("not_active")

            winner_delta = self._apply_winner_gain(
                cursor,
                winner_pet_id,
                gid,
                now=now,
                decay_per_day=decay_per_day,
                gain=winner_gain,
                daily_win_cap=daily_win_cap,
            )
            loser_delta = self._apply_loser_loss(
                cursor,
                loser_pet_id,
                gid,
                now=now,
                decay_per_day=decay_per_day,
                loss=loser_loss,
                floor=loss_floor,
            )
            cursor.execute(
                "UPDATE pet_brawls SET status = 'done', resolved_at = ?, "
                "winner_id = ?, winner_pet_id = ?, loser_pet_id = ?, rounds = ?, "
                "winner_hunger_delta = ?, loser_hunger_delta = ? "
                "WHERE brawl_id = ?",
                (
                    now,
                    winner_id,
                    winner_pet_id,
                    loser_pet_id,
                    rounds,
                    winner_delta,
                    loser_delta,
                    brawl_id,
                ),
            )
        return {"winner_delta": winner_delta, "loser_delta": loser_delta}

    def _read_living_hunger(
        self, cursor: sqlite3.Cursor, pet_id: int, gid: int, now: int,
        decay_per_day: int,
    ) -> int | None:
        """Current derived hunger, or None if dead / starved / missing."""
        row = cursor.execute(
            "SELECT * FROM pets WHERE pet_id = ? AND guild_id = ?",
            (pet_id, gid),
        ).fetchone()
        if row is None or row["died_at"] is not None:
            return None
        pet = Pet.from_row(dict(row))
        hunger = pet.current_hunger(now, decay_per_day)
        return hunger if hunger > 0 else None

    def _apply_winner_gain(
        self,
        cursor: sqlite3.Cursor,
        pet_id: int,
        gid: int,
        *,
        now: int,
        decay_per_day: int,
        gain: int,
        daily_win_cap: int,
    ) -> int:
        hunger = self._read_living_hunger(cursor, pet_id, gid, now, decay_per_day)
        if hunger is None or gain <= 0:
            return 0
        rewarded_today = cursor.execute(
            "SELECT COUNT(*) AS n FROM pet_brawls "
            "WHERE winner_pet_id = ? AND guild_id = ? AND status = 'done' "
            "AND winner_hunger_delta > 0 AND resolved_at >= ?",
            (pet_id, gid, game_day_start_ts(now)),
        ).fetchone()["n"]
        if rewarded_today >= daily_win_cap:
            return 0
        new_hunger = min(100, hunger + gain)
        if new_hunger == hunger:
            return 0
        write_hunger_anchor(cursor, pet_id, now, new_hunger)
        return new_hunger - hunger

    def _apply_loser_loss(
        self,
        cursor: sqlite3.Cursor,
        pet_id: int,
        gid: int,
        *,
        now: int,
        decay_per_day: int,
        loss: int,
        floor: int,
    ) -> int:
        hunger = self._read_living_hunger(cursor, pet_id, gid, now, decay_per_day)
        if hunger is None:
            return 0
        applied = min(loss, max(0, hunger - floor))
        if applied <= 0:
            return 0
        write_hunger_anchor(cursor, pet_id, now, hunger - applied)
        return -applied
