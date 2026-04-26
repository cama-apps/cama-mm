"""
Repository for bankruptcy state data access.
"""

from repositories.base_repository import BaseRepository
from repositories.interfaces import IBankruptcyRepository


class BankruptcyRepository(BaseRepository, IBankruptcyRepository):
    """Data access for bankruptcy state."""

    def get_state(self, discord_id: int, guild_id: int | None = None) -> dict | None:
        """Get bankruptcy state for a player."""
        normalized_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT discord_id, guild_id, last_bankruptcy_at, penalty_games_remaining,
                       COALESCE(bankruptcy_count, 0) as bankruptcy_count
                FROM bankruptcy_state
                WHERE discord_id = ? AND guild_id = ?
                """,
                (discord_id, normalized_id),
            )
            row = cursor.fetchone()
            if not row:
                return None
            return {
                "discord_id": row["discord_id"],
                "guild_id": row["guild_id"],
                "last_bankruptcy_at": row["last_bankruptcy_at"],
                "penalty_games_remaining": row["penalty_games_remaining"],
                "bankruptcy_count": row["bankruptcy_count"],
            }

    def get_bulk_states(self, discord_ids: list[int], guild_id: int | None = None) -> dict[int, dict]:
        """Get bankruptcy states for multiple players in one query.

        Args:
            discord_ids: List of Discord IDs to fetch states for.
            guild_id: Guild ID for multi-guild support.

        Returns:
            Dict mapping discord_id to state dict. Only includes users
            who have bankruptcy records in the database (excludes users
            who have never declared bankruptcy). Empty input returns empty dict.
        """
        if not discord_ids:
            return {}
        normalized_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            placeholders = ",".join("?" * len(discord_ids))
            cursor.execute(
                f"""
                SELECT discord_id, guild_id, last_bankruptcy_at, penalty_games_remaining,
                       COALESCE(bankruptcy_count, 0) as bankruptcy_count
                FROM bankruptcy_state
                WHERE discord_id IN ({placeholders}) AND guild_id = ?
                """,
                (*discord_ids, normalized_id),
            )
            return {row["discord_id"]: dict(row) for row in cursor.fetchall()}

    def execute_bankruptcy_atomic(
        self,
        discord_id: int,
        guild_id: int | None,
        fresh_start_balance: int,
        cooldown_seconds: int,
        penalty_games: int,
        now: int,
    ) -> int:
        """
        Atomically declare bankruptcy: re-check the player is in debt, that the
        cooldown has elapsed, then reset the balance and bump the state — all
        inside one BEGIN IMMEDIATE so two concurrent /bankruptcy calls cannot
        both pass validation.

        Returns the absolute debt cleared.
        Raises ValueError("not_in_debt") or ValueError("cooldown:<remaining_seconds>")
        if validation fails inside the transaction.
        Raises ValueError("not_found") if the player row is missing.
        """
        normalized_id = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            cursor.execute(
                "SELECT COALESCE(jopacoin_balance, 0) as balance FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, normalized_id),
            )
            balance_row = cursor.fetchone()
            if not balance_row:
                raise ValueError("not_found")
            balance = balance_row["balance"]
            if balance >= 0:
                raise ValueError("not_in_debt")

            cursor.execute(
                "SELECT last_bankruptcy_at FROM bankruptcy_state WHERE discord_id = ? AND guild_id = ?",
                (discord_id, normalized_id),
            )
            state_row = cursor.fetchone()
            if state_row and state_row["last_bankruptcy_at"]:
                elapsed = now - state_row["last_bankruptcy_at"]
                if elapsed < cooldown_seconds:
                    raise ValueError(f"cooldown:{cooldown_seconds - elapsed}")

            cursor.execute(
                """
                UPDATE players
                SET jopacoin_balance = ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (fresh_start_balance, discord_id, normalized_id),
            )
            cursor.execute(
                """
                INSERT INTO bankruptcy_state (discord_id, guild_id, last_bankruptcy_at, penalty_games_remaining, bankruptcy_count, updated_at)
                VALUES (?, ?, ?, ?, 1, CURRENT_TIMESTAMP)
                ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                    last_bankruptcy_at = excluded.last_bankruptcy_at,
                    penalty_games_remaining = excluded.penalty_games_remaining,
                    bankruptcy_count = COALESCE(bankruptcy_state.bankruptcy_count, 0) + 1,
                    updated_at = CURRENT_TIMESTAMP
                """,
                (discord_id, normalized_id, now, penalty_games),
            )

            return abs(balance)

    def upsert_state(
        self, discord_id: int, guild_id: int | None, last_bankruptcy_at: int, penalty_games_remaining: int
    ) -> None:
        """Create or update bankruptcy state, incrementing bankruptcy_count."""
        normalized_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO bankruptcy_state (discord_id, guild_id, last_bankruptcy_at, penalty_games_remaining, bankruptcy_count, updated_at)
                VALUES (?, ?, ?, ?, 1, CURRENT_TIMESTAMP)
                ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                    last_bankruptcy_at = excluded.last_bankruptcy_at,
                    penalty_games_remaining = excluded.penalty_games_remaining,
                    bankruptcy_count = COALESCE(bankruptcy_state.bankruptcy_count, 0) + 1,
                    updated_at = CURRENT_TIMESTAMP
                """,
                (discord_id, normalized_id, last_bankruptcy_at, penalty_games_remaining),
            )

    def reset_cooldown_only(
        self, discord_id: int, guild_id: int | None, last_bankruptcy_at: int, penalty_games_remaining: int
    ) -> None:
        """Reset cooldown and penalty without incrementing bankruptcy_count."""
        normalized_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE bankruptcy_state
                SET last_bankruptcy_at = ?,
                    penalty_games_remaining = ?,
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (last_bankruptcy_at, penalty_games_remaining, discord_id, normalized_id),
            )

    def adjust_penalty_games(
        self, discord_id: int, guild_id: int | None, delta: int
    ) -> int:
        """Add ``delta`` to ``penalty_games_remaining`` (clamped to >= 0).

        Returns the resulting total. Use a negative delta to subtract.
        """
        normalized_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE bankruptcy_state
                SET penalty_games_remaining = MAX(0, penalty_games_remaining + ?),
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (delta, discord_id, normalized_id),
            )
            cursor.execute(
                "SELECT penalty_games_remaining FROM bankruptcy_state WHERE discord_id = ? AND guild_id = ?",
                (discord_id, normalized_id),
            )
            row = cursor.fetchone()
            return row["penalty_games_remaining"] if row else max(delta, 0)

    def decrement_penalty_games(self, discord_id: int, guild_id: int | None = None) -> int:
        """
        Decrement penalty games remaining by 1 if > 0.

        Returns the new count.
        """
        normalized_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE bankruptcy_state
                SET penalty_games_remaining = MAX(0, penalty_games_remaining - 1),
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (discord_id, normalized_id),
            )
            cursor.execute(
                "SELECT penalty_games_remaining FROM bankruptcy_state WHERE discord_id = ? AND guild_id = ?",
                (discord_id, normalized_id),
            )
            row = cursor.fetchone()
            return row["penalty_games_remaining"] if row else 0

    def get_penalty_games(self, discord_id: int, guild_id: int | None = None) -> int:
        """Get the number of penalty games remaining for a player."""
        normalized_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT penalty_games_remaining FROM bankruptcy_state WHERE discord_id = ? AND guild_id = ?",
                (discord_id, normalized_id),
            )
            row = cursor.fetchone()
            return row["penalty_games_remaining"] if row else 0
