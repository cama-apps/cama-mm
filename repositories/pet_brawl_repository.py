"""Repository for pet brawls: lifecycle, escrow, and pet settlement.

Wagers and fees are escrowed or refunded with each lifecycle transition.
Final payout, Reserve fee, hunger, training, and the completed brawl row
share one atomic transaction. Settlement re-reads both pets so a death
claimed mid-battle can never be overwritten (writing fresh anchors onto a
starved pet would resurrect it).

Validation errors are raised as ``ValueError(code)``:
    brawl_busy, no_brawl, not_recipient, not_challenger, not_pending,
    expired, not_open, not_active, already_done
"""

from __future__ import annotations

import sqlite3

from domain.models.pet import Pet
from domain.models.pet_brawl import PetBrawl, PetBrawlStatus
from domain.pet_constants import (
    BRAWL_LOSS_XP,
    BRAWL_TRAINING_LEVEL_CAP,
    BRAWL_TRAINING_STAT_CAP,
    BRAWL_TRAINING_XP_CAP,
    BRAWL_WIN_XP,
    BRAWL_XP_DAILY_CAP,
    BRAWL_XP_PER_LEVEL,
)
from repositories.base_repository import BaseRepository
from repositories.pet_repository import write_hunger_anchor
from utils.game_date import game_day_start_ts

_BRAWL_COLUMNS = (
    "brawl_id, guild_id, channel_id, challenger_id, recipient_id, "
    "challenger_pet_id, recipient_pet_id, status, created_at, expires_at, "
    "resolved_at, winner_id, winner_pet_id, loser_pet_id, rounds, "
    "winner_hunger_delta, loser_hunger_delta, wager, fee, "
    "challenger_xp_delta, recipient_xp_delta, challenger_stat_gain, "
    "recipient_stat_gain, personality_event_key"
)


def _row_to_brawl(row: sqlite3.Row) -> PetBrawl:
    return PetBrawl.from_row(dict(row))


class PetBrawlRepository(BaseRepository):
    def _update_player_balance(
        self,
        cursor: sqlite3.Cursor,
        *,
        brawl: PetBrawl,
        player_id: int,
        delta: int,
        actor_id: int | None,
        reason: str,
        require_nonnegative: bool = False,
        error_code: str = "player_funds",
    ) -> None:
        if delta == 0:
            return
        self._set_economy_ledger_context(
            cursor,
            source="pet_brawl",
            actor_id=actor_id,
            related_type="pet_brawl",
            related_id=brawl.brawl_id,
            reason=reason,
            metadata={"wager": brawl.wager, "fee": brawl.fee},
        )
        try:
            sql = (
                "UPDATE players "
                "SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ? "
                "WHERE discord_id = ? AND guild_id = ?"
            )
            params: tuple = (delta, player_id, brawl.guild_id)
            if require_nonnegative:
                sql += " AND COALESCE(jopacoin_balance, 0) + ? >= 0"
                params += (delta,)
            cursor.execute(sql, params)
            if cursor.rowcount != 1:
                if require_nonnegative:
                    # A guarded debit that matched nothing means the player
                    # cannot cover it — that is a real, caller-handled refusal.
                    raise ValueError(error_code)
                if delta > 0 and not self._player_row_exists(cursor, player_id, brawl.guild_id):
                    # Credit owed to a player whose row is gone (purged
                    # account). Forfeit it to the nonprofit fund instead of
                    # raising: this runs inside sweep_stale's single
                    # transaction over every stale brawl, so one dead row used
                    # to abort the whole sweep — every tick, forever — leaving
                    # all wagered brawls in that guild permanently unrefunded.
                    # Mirrors pay_refunds_atomic's handling of the same case.
                    self._forfeit_to_nonprofit(cursor, brawl.guild_id, delta)
                    return
                raise RuntimeError(
                    "A pet-brawl participant balance could not be updated."
                )
        finally:
            self._clear_economy_ledger_context(cursor)

    def _player_row_exists(
        self, cursor: sqlite3.Cursor, player_id: int, guild_id: int | None,
    ) -> bool:
        return cursor.execute(
            "SELECT 1 FROM players WHERE discord_id = ? AND guild_id = ?",
            (player_id, self.normalize_guild_id(guild_id)),
        ).fetchone() is not None

    def _forfeit_to_nonprofit(
        self, cursor: sqlite3.Cursor, guild_id: int | None, amount: int,
    ) -> None:
        """Park coins that have no payee left, rather than destroying them.

        Same upsert every other nonprofit credit uses. The guild id must be
        normalized: nonprofit_fund.guild_id is an INTEGER PRIMARY KEY, i.e. a
        rowid alias, so a NULL would be silently auto-assigned a bogus id
        instead of failing.
        """
        cursor.execute(
            """
            INSERT INTO nonprofit_fund (guild_id, total_collected, updated_at)
            VALUES (?, ?, CURRENT_TIMESTAMP)
            ON CONFLICT(guild_id) DO UPDATE SET
                total_collected = total_collected + excluded.total_collected,
                updated_at = CURRENT_TIMESTAMP
            """,
            (self.normalize_guild_id(guild_id), amount),
        )

    def _refund_escrow(
        self,
        cursor: sqlite3.Cursor,
        brawl: PetBrawl,
        *,
        prior_status: str,
        actor_id: int | None,
    ) -> None:
        if brawl.wager <= 0:
            return
        self._update_player_balance(
            cursor,
            brawl=brawl,
            player_id=brawl.challenger_id,
            delta=brawl.wager + brawl.fee,
            actor_id=actor_id,
            reason="challenger_refund",
        )
        if prior_status == PetBrawlStatus.ACTIVE:
            self._update_player_balance(
                cursor,
                brawl=brawl,
                player_id=brawl.recipient_id,
                delta=brawl.wager,
                actor_id=actor_id,
                reason="recipient_refund",
            )

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
        wager: int = 0,
        fee: int = 0,
    ) -> PetBrawl:
        gid = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            if type(wager) is not int or not 0 <= wager <= 100:
                raise ValueError("invalid_wager")
            if type(fee) is not int or fee < 0:
                raise ValueError("invalid_fee")
            if (wager == 0) != (fee == 0):
                raise ValueError("invalid_fee")
            stale_pending = cursor.execute(
                f"SELECT {_BRAWL_COLUMNS} FROM pet_brawls "
                "WHERE guild_id = ? AND status = 'pending' AND expires_at <= ? "
                "AND (challenger_id IN (?, ?) OR recipient_id IN (?, ?))",
                (
                    gid,
                    now,
                    challenger_id,
                    recipient_id,
                    challenger_id,
                    recipient_id,
                ),
            ).fetchall()
            for row in stale_pending:
                stale_brawl = _row_to_brawl(row)
                cursor.execute(
                    "UPDATE pet_brawls SET status = 'expired', resolved_at = ? "
                    "WHERE brawl_id = ? AND guild_id = ? AND status = 'pending' "
                    "AND expires_at <= ?",
                    (now, stale_brawl.brawl_id, gid, now),
                )
                if cursor.rowcount == 1:
                    self._refund_escrow(
                        cursor,
                        stale_brawl,
                        prior_status=PetBrawlStatus.PENDING,
                        actor_id=None,
                    )
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
            challenger_pet = cursor.execute(
                "SELECT 1 FROM pets WHERE pet_id = ? AND discord_id = ? "
                "AND guild_id = ? AND died_at IS NULL AND hatched_at <= ?",
                (challenger_pet_id, challenger_id, gid, now),
            ).fetchone()
            if challenger_pet is None:
                raise ValueError("challenger_pet_unavailable")
            recipient_pet = cursor.execute(
                "SELECT 1 FROM pets WHERE discord_id = ? AND guild_id = ? "
                "AND died_at IS NULL AND hatched_at <= ? LIMIT 1",
                (recipient_id, gid, now),
            ).fetchone()
            if recipient_pet is None:
                raise ValueError("recipient_pet_unavailable")
            if wager:
                balances = {}
                for player_id in (challenger_id, recipient_id):
                    player = cursor.execute(
                        "SELECT COALESCE(jopacoin_balance, 0) AS balance "
                        "FROM players WHERE discord_id = ? AND guild_id = ?",
                        (player_id, gid),
                    ).fetchone()
                    balances[player_id] = (
                        int(player["balance"]) if player is not None else None
                    )
                if (
                    balances[challenger_id] is None
                    or balances[challenger_id] < wager + fee
                ):
                    raise ValueError("challenger_funds")
                if (
                    balances[recipient_id] is None
                    or balances[recipient_id] < wager
                ):
                    raise ValueError("recipient_funds")
            cursor.execute(
                "INSERT INTO pet_brawls (guild_id, channel_id, challenger_id, "
                "recipient_id, challenger_pet_id, status, created_at, expires_at, "
                "wager, fee) "
                "VALUES (?, ?, ?, ?, ?, 'pending', ?, ?, ?, ?)",
                (
                    gid,
                    channel_id,
                    challenger_id,
                    recipient_id,
                    challenger_pet_id,
                    now,
                    expires_at,
                    wager,
                    fee,
                ),
            )
            row = cursor.execute(
                f"SELECT {_BRAWL_COLUMNS} FROM pet_brawls WHERE brawl_id = ?",
                (cursor.lastrowid,),
            ).fetchone()
            brawl = _row_to_brawl(row)
            if wager:
                self._update_player_balance(
                    cursor,
                    brawl=brawl,
                    player_id=challenger_id,
                    delta=-(wager + fee),
                    actor_id=challenger_id,
                    reason="challenger_escrow",
                    require_nonnegative=True,
                    error_code="challenger_funds",
                )
        return brawl

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
            current = cursor.execute(
                f"SELECT {_BRAWL_COLUMNS} FROM pet_brawls "
                "WHERE brawl_id = ? AND guild_id = ?",
                (brawl_id, gid),
            ).fetchone()
            if current is None:
                raise ValueError("no_brawl")
            brawl = _row_to_brawl(current)
            if brawl.recipient_id != recipient_id:
                raise ValueError("not_recipient")
            if brawl.status == PetBrawlStatus.PENDING and brawl.expires_at <= now:
                raise ValueError("expired")
            if brawl.status != PetBrawlStatus.PENDING:
                raise ValueError("not_pending")
            if brawl.wager:
                self._update_player_balance(
                    cursor,
                    brawl=brawl,
                    player_id=recipient_id,
                    delta=-brawl.wager,
                    actor_id=recipient_id,
                    reason="recipient_escrow",
                    require_nonnegative=True,
                    error_code="recipient_funds",
                )
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
            row = cursor.execute(
                f"SELECT {_BRAWL_COLUMNS} FROM pet_brawls WHERE brawl_id = ?",
                (brawl_id,),
            ).fetchone()
            self._refund_escrow(
                cursor,
                _row_to_brawl(row),
                prior_status=PetBrawlStatus.PENDING,
                actor_id=recipient_id,
            )

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
            row = cursor.execute(
                f"SELECT {_BRAWL_COLUMNS} FROM pet_brawls WHERE brawl_id = ?",
                (brawl_id,),
            ).fetchone()
            self._refund_escrow(
                cursor,
                _row_to_brawl(row),
                prior_status=PetBrawlStatus.PENDING,
                actor_id=challenger_id,
            )

    def void_atomic(self, brawl_id: int, guild_id: int | None, now: int) -> None:
        """Void an open brawl (withdrawn, invalidated, or abandoned)."""
        gid = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            current = cursor.execute(
                f"SELECT {_BRAWL_COLUMNS} FROM pet_brawls "
                "WHERE brawl_id = ? AND guild_id = ?",
                (brawl_id, gid),
            ).fetchone()
            if current is None:
                raise ValueError("not_open")
            brawl = _row_to_brawl(current)
            if not brawl.is_open:
                raise ValueError("not_open")
            cursor.execute(
                "UPDATE pet_brawls SET status = 'void', resolved_at = ? "
                "WHERE brawl_id = ? AND guild_id = ? "
                "AND status IN ('pending', 'active')",
                (now, brawl_id, gid),
            )
            if cursor.rowcount != 1:
                raise ValueError("not_open")
            self._refund_escrow(
                cursor,
                brawl,
                prior_status=brawl.status,
                actor_id=None,
            )

    def sweep_stale(
        self, now: int, *, active_ttl_seconds: int
    ) -> dict[str, int]:
        """Expire overdue challenges; void battles stuck past the TTL."""
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            stale = cursor.execute(
                f"SELECT {_BRAWL_COLUMNS} FROM pet_brawls "
                "WHERE (status = 'pending' AND expires_at <= ?) "
                "OR (status = 'active' AND created_at < ?)",
                (now, now - active_ttl_seconds),
            ).fetchall()
            expired = 0
            voided = 0
            for row in stale:
                brawl = _row_to_brawl(row)
                new_status = (
                    PetBrawlStatus.EXPIRED
                    if brawl.status == PetBrawlStatus.PENDING
                    else PetBrawlStatus.VOID
                )
                cursor.execute(
                    "UPDATE pet_brawls SET status = ?, resolved_at = ? "
                    "WHERE brawl_id = ? AND status = ?",
                    (new_status, now, brawl.brawl_id, brawl.status),
                )
                if cursor.rowcount != 1:
                    continue
                self._refund_escrow(
                    cursor,
                    brawl,
                    prior_status=brawl.status,
                    actor_id=None,
                )
                if new_status == PetBrawlStatus.EXPIRED:
                    expired += 1
                else:
                    voided += 1
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

    def settle_draw_atomic(
        self,
        brawl_id: int,
        guild_id: int | None,
        *,
        participant_pet_ids: tuple[int, int],
        rounds: int,
        now: int,
    ) -> dict:
        """Finalize a draw and refund both stakes without pet progression."""
        gid = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            row = cursor.execute(
                f"SELECT {_BRAWL_COLUMNS} FROM pet_brawls "
                "WHERE brawl_id = ? AND guild_id = ?",
                (brawl_id, gid),
            ).fetchone()
            if row is None:
                raise ValueError("no_brawl")
            brawl = _row_to_brawl(row)
            if brawl.status == PetBrawlStatus.DONE:
                raise ValueError("already_done")
            if brawl.status != PetBrawlStatus.ACTIVE:
                raise ValueError("not_active")
            if (
                brawl.recipient_pet_id is None
                or set(participant_pet_ids)
                != {brawl.challenger_pet_id, brawl.recipient_pet_id}
            ):
                raise ValueError("participant_mismatch")

            cursor.execute(
                "UPDATE pet_brawls SET status = 'done', resolved_at = ?, "
                "winner_id = NULL, winner_pet_id = NULL, loser_pet_id = NULL, "
                "rounds = ? WHERE brawl_id = ? AND guild_id = ?",
                (now, rounds, brawl_id, gid),
            )
            self._refund_escrow(
                cursor,
                brawl,
                prior_status=PetBrawlStatus.ACTIVE,
                actor_id=None,
            )
        return {"wager": brawl.wager, "fee": brawl.fee}

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
        winner_stat_gain: str | None = None,
        loser_stat_gain: str | None = None,
        personality_event_key: str | None = None,
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
                f"SELECT {_BRAWL_COLUMNS} FROM pet_brawls "
                "WHERE brawl_id = ? AND guild_id = ?",
                (brawl_id, gid),
            ).fetchone()
            if row is None:
                raise ValueError("no_brawl")
            brawl = _row_to_brawl(row)
            if brawl.status == PetBrawlStatus.DONE:
                raise ValueError("already_done")
            if brawl.status != PetBrawlStatus.ACTIVE:
                raise ValueError("not_active")
            participants = {
                (brawl.challenger_id, brawl.challenger_pet_id),
                (brawl.recipient_id, brawl.recipient_pet_id),
            }
            if (
                brawl.recipient_pet_id is None
                or (winner_id, winner_pet_id) not in participants
                or {winner_pet_id, loser_pet_id}
                != {brawl.challenger_pet_id, brawl.recipient_pet_id}
            ):
                raise ValueError("participant_mismatch")

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
            (
                winner_xp_delta,
                applied_winner_stat,
                winner_reached_training_cap,
            ) = self._apply_training_xp(
                cursor,
                winner_pet_id,
                gid,
                now=now,
                award=BRAWL_WIN_XP,
                proposed_stat=winner_stat_gain,
            )
            (
                loser_xp_delta,
                applied_loser_stat,
                loser_reached_training_cap,
            ) = self._apply_training_xp(
                cursor,
                loser_pet_id,
                gid,
                now=now,
                award=BRAWL_LOSS_XP,
                proposed_stat=loser_stat_gain,
            )
            if winner_pet_id == brawl.challenger_pet_id:
                challenger_xp_delta = winner_xp_delta
                recipient_xp_delta = loser_xp_delta
                challenger_stat_gain = applied_winner_stat
                recipient_stat_gain = applied_loser_stat
            else:
                challenger_xp_delta = loser_xp_delta
                recipient_xp_delta = winner_xp_delta
                challenger_stat_gain = applied_loser_stat
                recipient_stat_gain = applied_winner_stat
            if (
                winner_reached_training_cap or loser_reached_training_cap
            ) and personality_event_key != "milestone.champion":
                personality_event_key = "milestone.build"
            payout = 2 * brawl.wager
            if payout:
                self._update_player_balance(
                    cursor,
                    brawl=brawl,
                    player_id=winner_id,
                    delta=payout,
                    actor_id=winner_id,
                    reason="winner_payout",
                )
                self._set_economy_ledger_context(
                    cursor,
                    source="pet_brawl",
                    actor_id=winner_id,
                    related_type="pet_brawl",
                    related_id=brawl.brawl_id,
                    reason="brawl_fee_reserve_credit",
                    metadata={"wager": brawl.wager, "fee": brawl.fee},
                )
                try:
                    cursor.execute(
                        "INSERT INTO nonprofit_fund "
                        "(guild_id, total_collected, updated_at) "
                        "VALUES (?, ?, CURRENT_TIMESTAMP) "
                        "ON CONFLICT(guild_id) DO UPDATE SET "
                        "total_collected = total_collected + excluded.total_collected, "
                        "updated_at = CURRENT_TIMESTAMP",
                        (gid, brawl.fee),
                    )
                finally:
                    self._clear_economy_ledger_context(cursor)
            cursor.execute(
                "UPDATE pet_brawls SET status = 'done', resolved_at = ?, "
                "winner_id = ?, winner_pet_id = ?, loser_pet_id = ?, rounds = ?, "
                "winner_hunger_delta = ?, loser_hunger_delta = ?, "
                "challenger_xp_delta = ?, recipient_xp_delta = ?, "
                "challenger_stat_gain = ?, recipient_stat_gain = ?, "
                "personality_event_key = ? "
                "WHERE brawl_id = ?",
                (
                    now,
                    winner_id,
                    winner_pet_id,
                    loser_pet_id,
                    rounds,
                    winner_delta,
                    loser_delta,
                    challenger_xp_delta,
                    recipient_xp_delta,
                    challenger_stat_gain,
                    recipient_stat_gain,
                    personality_event_key,
                    brawl_id,
                ),
            )
        return {
            "winner_delta": winner_delta,
            "loser_delta": loser_delta,
            "winner_xp_delta": winner_xp_delta,
            "loser_xp_delta": loser_xp_delta,
            "winner_stat_gain": applied_winner_stat,
            "loser_stat_gain": applied_loser_stat,
            "payout": payout,
            "wager": brawl.wager,
            "fee": brawl.fee,
            "personality_event_key": personality_event_key,
        }

    def _apply_training_xp(
        self,
        cursor: sqlite3.Cursor,
        pet_id: int,
        gid: int,
        *,
        now: int,
        award: int,
        proposed_stat: str | None,
    ) -> tuple[int, str | None, bool]:
        row = cursor.execute(
            "SELECT training_xp, training_str, training_int, training_dex "
            "FROM pets WHERE pet_id = ? AND guild_id = ?",
            (pet_id, gid),
        ).fetchone()
        if row is None or award <= 0 or row["training_xp"] >= BRAWL_TRAINING_XP_CAP:
            return 0, None, False
        rewarded_today = cursor.execute(
            "SELECT COUNT(*) AS n FROM pet_brawls "
            "WHERE guild_id = ? AND status = 'done' AND resolved_at >= ? "
            "AND ((challenger_pet_id = ? AND challenger_xp_delta > 0) "
            "OR (recipient_pet_id = ? AND recipient_xp_delta > 0))",
            (gid, game_day_start_ts(now), pet_id, pet_id),
        ).fetchone()["n"]
        if rewarded_today >= BRAWL_XP_DAILY_CAP:
            return 0, None, False

        old_xp = int(row["training_xp"])
        new_xp = min(BRAWL_TRAINING_XP_CAP, old_xp + award)
        xp_delta = new_xp - old_xp
        old_level = old_xp // BRAWL_XP_PER_LEVEL
        new_level = new_xp // BRAWL_XP_PER_LEVEL
        stat_gain = None
        stats = {
            "str": int(row["training_str"]),
            "int": int(row["training_int"]),
            "dex": int(row["training_dex"]),
        }
        if (
            new_level > old_level
            and sum(stats.values()) < BRAWL_TRAINING_LEVEL_CAP
        ):
            eligible = [
                stat for stat, value in stats.items()
                if value < BRAWL_TRAINING_STAT_CAP
            ]
            if eligible:
                stat_gain = (
                    proposed_stat if proposed_stat in eligible else eligible[0]
                )
                stats[stat_gain] += 1

        cursor.execute(
            "UPDATE pets SET training_xp = ?, training_str = ?, "
            "training_int = ?, training_dex = ? "
            "WHERE pet_id = ? AND guild_id = ?",
            (
                new_xp,
                stats["str"],
                stats["int"],
                stats["dex"],
                pet_id,
                gid,
            ),
        )
        return xp_delta, stat_gain, new_xp == BRAWL_TRAINING_XP_CAP

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
