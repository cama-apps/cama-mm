"""Persistence and atomic economy operations for duel challenges."""

from __future__ import annotations

from domain.models.duel import DuelChallenge
from repositories.base_repository import BaseRepository


class DuelChallengeRepository(BaseRepository):
    """Store duel challenges and guard their economy-sensitive creation."""

    @staticmethod
    def _challenge_from_row(row) -> DuelChallenge | None:
        return DuelChallenge.from_row(row) if row is not None else None

    def get_challenge(self, challenge_id: int, guild_id: int) -> DuelChallenge | None:
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            row = conn.execute(
                "SELECT * FROM duel_challenges WHERE challenge_id = ? AND guild_id = ?",
                (challenge_id, guild_id),
            ).fetchone()
        return self._challenge_from_row(row)

    def get_pending_for_recipient(
        self, recipient_id: int, guild_id: int
    ) -> DuelChallenge | None:
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            row = conn.execute(
                """
                SELECT *
                FROM duel_challenges
                WHERE guild_id = ? AND recipient_id = ? AND status = 'pending'
                ORDER BY created_at DESC, challenge_id DESC
                LIMIT 1
                """,
                (guild_id, recipient_id),
            ).fetchone()
        return self._challenge_from_row(row)

    def list_outstanding(self, guild_id: int) -> list[DuelChallenge]:
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            rows = conn.execute(
                """
                SELECT *
                FROM duel_challenges
                WHERE guild_id = ? AND status IN ('pending', 'accepted')
                ORDER BY created_at DESC, challenge_id DESC
                """,
                (guild_id,),
            ).fetchall()
        return [DuelChallenge.from_row(row) for row in rows]

    def list_pending_all(self) -> list[DuelChallenge]:
        with self.connection() as conn:
            rows = conn.execute(
                """
                SELECT *
                FROM duel_challenges
                WHERE status = 'pending'
                ORDER BY created_at DESC, challenge_id DESC
                """
            ).fetchall()
        return [DuelChallenge.from_row(row) for row in rows]

    def create_challenge_atomic(
        self,
        guild_id: int,
        channel_id: int,
        challenger_id: int,
        recipient_id: int,
        wager: int,
        now: int,
        challenger_cooldown_seconds: int,
        recipient_cooldown_seconds: int,
        response_seconds: int,
        actor_id: int,
    ) -> DuelChallenge:
        guild_id = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            if challenger_id == recipient_id:
                raise ValueError("You cannot challenge yourself to a duel.")
            if not 500 <= wager <= 1000:
                raise ValueError("The wager must be between 500 and 1000 jopacoin.")

            players = {}
            for player_id in (challenger_id, recipient_id):
                row = cursor.execute(
                    """
                    SELECT discord_id, glicko_rating, glicko_rd,
                           COALESCE(jopacoin_balance, 0) AS balance
                    FROM players
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    (player_id, guild_id),
                ).fetchone()
                if row is None:
                    raise ValueError("Both duelists must be registered in this server.")
                if row["glicko_rating"] is None or row["glicko_rd"] is None:
                    raise ValueError("Both duelists must be Glicko rated.")
                players[player_id] = row

            challenger = players[challenger_id]
            recipient = players[recipient_id]
            if float(recipient["glicko_rating"]) < float(challenger["glicko_rating"]):
                raise ValueError("Challenges of honor forbid punching down in Glicko rating.")

            unresolved = cursor.execute(
                """
                SELECT 1
                FROM duel_challenges
                WHERE guild_id = ?
                  AND status IN ('pending', 'accepted')
                  AND (
                      challenger_id IN (?, ?)
                      OR recipient_id IN (?, ?)
                  )
                LIMIT 1
                """,
                (
                    guild_id,
                    challenger_id,
                    recipient_id,
                    challenger_id,
                    recipient_id,
                ),
            ).fetchone()
            if unresolved is not None:
                raise ValueError("One of these players is already involved in an unresolved duel.")

            challenger_history = cursor.execute(
                """
                SELECT 1
                FROM duel_challenges
                WHERE guild_id = ? AND challenger_id = ?
                  AND status != 'delivery_failed' AND created_at > ?
                LIMIT 1
                """,
                (guild_id, challenger_id, now - challenger_cooldown_seconds),
            ).fetchone()
            if challenger_history is not None:
                raise ValueError("Your monthly duel challenge cooldown has not elapsed.")

            recipient_history = cursor.execute(
                """
                SELECT 1
                FROM duel_challenges
                WHERE guild_id = ? AND recipient_id = ?
                  AND status != 'delivery_failed' AND created_at > ?
                LIMIT 1
                """,
                (guild_id, recipient_id, now - recipient_cooldown_seconds),
            ).fetchone()
            if recipient_history is not None:
                raise ValueError("That player was recently challenged and has weekly protection.")

            if int(challenger["balance"]) < wager:
                raise ValueError("Your jopacoin balance cannot cover that wager.")

            expires_at = now + response_seconds
            cursor.execute(
                """
                INSERT INTO duel_challenges (
                    guild_id, channel_id, challenger_id, recipient_id, wager,
                    status, challenger_glicko, challenger_rd, recipient_glicko,
                    recipient_rd, created_at, expires_at, next_reminder_at
                ) VALUES (?, ?, ?, ?, ?, 'pending', ?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    guild_id,
                    channel_id,
                    challenger_id,
                    recipient_id,
                    wager,
                    float(challenger["glicko_rating"]),
                    float(challenger["glicko_rd"]),
                    float(recipient["glicko_rating"]),
                    float(recipient["glicko_rd"]),
                    now,
                    expires_at,
                    min(now + 86400, expires_at),
                ),
            )
            challenge_id = cursor.lastrowid

            self._set_economy_ledger_context(
                cursor,
                source="duel_challenge",
                actor_id=actor_id,
                related_type="duel_challenge",
                related_id=challenge_id,
                reason="challenger_escrow",
                metadata={"wager": wager, "recipient_id": recipient_id},
            )
            try:
                cursor.execute(
                    """
                    UPDATE players
                    SET jopacoin_balance = jopacoin_balance - ?
                    WHERE discord_id = ? AND guild_id = ? AND jopacoin_balance >= ?
                    """,
                    (wager, challenger_id, guild_id, wager),
                )
                if cursor.rowcount != 1:
                    raise ValueError("Your jopacoin balance cannot cover that wager.")
            finally:
                self._clear_economy_ledger_context(cursor)

            row = cursor.execute(
                "SELECT * FROM duel_challenges WHERE challenge_id = ? AND guild_id = ?",
                (challenge_id, guild_id),
            ).fetchone()
            challenge = self._challenge_from_row(row)
            if challenge is None:
                raise RuntimeError("Duel challenge insert did not persist.")
            return challenge

    def bind_message(
        self, challenge_id: int, guild_id: int, message_id: int
    ) -> DuelChallenge:
        guild_id = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE duel_challenges
                SET message_id = ?
                WHERE challenge_id = ? AND guild_id = ?
                  AND status = 'pending' AND message_id IS NULL
                """,
                (message_id, challenge_id, guild_id),
            )
            if cursor.rowcount != 1:
                raise ValueError("The duel message could not be bound.")
            row = cursor.execute(
                "SELECT * FROM duel_challenges WHERE challenge_id = ? AND guild_id = ?",
                (challenge_id, guild_id),
            ).fetchone()
            challenge = self._challenge_from_row(row)
            if challenge is None:
                raise RuntimeError("Bound duel challenge could not be read.")
            return challenge

    def mark_delivery_failed_atomic(
        self, challenge_id: int, guild_id: int, now: int, actor_id: int
    ) -> DuelChallenge:
        guild_id = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            row = cursor.execute(
                """
                SELECT * FROM duel_challenges
                WHERE challenge_id = ? AND guild_id = ? AND status = 'pending'
                """,
                (challenge_id, guild_id),
            ).fetchone()
            challenge = self._challenge_from_row(row)
            if challenge is None:
                raise ValueError("Only a pending duel can fail initial delivery.")

            self._set_economy_ledger_context(
                cursor,
                source="duel_challenge",
                actor_id=actor_id,
                related_type="duel_challenge",
                related_id=challenge_id,
                reason="initial_delivery_refund",
                metadata={"wager": challenge.wager},
            )
            try:
                cursor.execute(
                    """
                    UPDATE players
                    SET jopacoin_balance = jopacoin_balance + ?
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    (challenge.wager, challenge.challenger_id, guild_id),
                )
                if cursor.rowcount != 1:
                    raise RuntimeError("Duel escrow refund recipient was not found.")
            finally:
                self._clear_economy_ledger_context(cursor)

            cursor.execute(
                """
                UPDATE duel_challenges
                SET status = 'delivery_failed', next_reminder_at = NULL,
                    resolved_at = ?, resolution_actor_id = ?
                WHERE challenge_id = ? AND guild_id = ? AND status = 'pending'
                """,
                (now, actor_id, challenge_id, guild_id),
            )
            if cursor.rowcount != 1:
                raise RuntimeError("Duel delivery failure state was not recorded.")
            row = cursor.execute(
                "SELECT * FROM duel_challenges WHERE challenge_id = ? AND guild_id = ?",
                (challenge_id, guild_id),
            ).fetchone()
            failed = self._challenge_from_row(row)
            if failed is None:
                raise RuntimeError("Failed duel challenge could not be read.")
            return failed
