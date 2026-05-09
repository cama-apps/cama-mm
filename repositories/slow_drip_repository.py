"""Repository for the Slow Drip relic's daily idle-income claims."""

import time

from repositories.base_repository import BaseRepository


class SlowDripRepository(BaseRepository):
    """Tracks per-day idle-income totals for the Slow Drip relic.

    Each row is (discord_id, guild_id, claim_date) → claimed_today and
    last_claim_at. The /dig path computes ``min(elapsed_min × 0.5,
    daily_cap - claimed_today)`` and increments claimed_today.
    """

    def get_today(
        self, discord_id: int, guild_id: int | None, claim_date: str
    ) -> dict:
        """Return {claimed_today, last_claim_at}. New rows return defaults."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT claimed_today, last_claim_at
                FROM slow_drip_claims
                WHERE discord_id = ? AND guild_id = ? AND claim_date = ?
                """,
                (discord_id, gid, claim_date),
            )
            row = cursor.fetchone()
            if row is None:
                return {"claimed_today": 0, "last_claim_at": 0}
            return {
                "claimed_today": int(row["claimed_today"] or 0),
                "last_claim_at": int(row["last_claim_at"] or 0),
            }

    def add_claim(
        self,
        discord_id: int,
        guild_id: int | None,
        claim_date: str,
        amount: int,
    ) -> None:
        """Add ``amount`` to claimed_today for the given date and stamp
        last_claim_at to now. Upserts a fresh row if needed."""
        if amount <= 0:
            return
        gid = self.normalize_guild_id(guild_id)
        now = int(time.time())
        with self.atomic_transaction() as conn:
            conn.execute(
                """
                INSERT INTO slow_drip_claims
                (discord_id, guild_id, claim_date, claimed_today, last_claim_at)
                VALUES (?, ?, ?, ?, ?)
                ON CONFLICT(discord_id, guild_id, claim_date) DO UPDATE SET
                    claimed_today = claimed_today + excluded.claimed_today,
                    last_claim_at = excluded.last_claim_at
                """,
                (discord_id, gid, claim_date, amount, now),
            )

    def stamp_seen(
        self,
        discord_id: int,
        guild_id: int | None,
        claim_date: str,
    ) -> None:
        """Initialise today's row (without crediting) so subsequent claims
        compute elapsed time from the stamp instead of from epoch 0. Called
        when /dig fires for a player who has Slow Drip equipped but already
        hit the daily cap."""
        gid = self.normalize_guild_id(guild_id)
        now = int(time.time())
        with self.connection() as conn:
            conn.execute(
                """
                INSERT OR IGNORE INTO slow_drip_claims
                (discord_id, guild_id, claim_date, claimed_today, last_claim_at)
                VALUES (?, ?, ?, 0, ?)
                """,
                (discord_id, gid, claim_date, now),
            )
