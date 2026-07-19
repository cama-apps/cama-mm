"""
Repository for soft avoid data access.
"""

import logging
import time
from dataclasses import dataclass
from typing import Literal

from domain.soft_avoid_constants import SOFT_AVOID_GAMES
from repositories.base_repository import BaseRepository
from repositories.interfaces import ISoftAvoidRepository

logger = logging.getLogger("cama_bot.repositories.soft_avoid")
SOFT_AVOID_MAX_GAMES = SOFT_AVOID_GAMES


def _validate_soft_avoid_games(games: int) -> None:
    if not 1 <= games <= SOFT_AVOID_MAX_GAMES:
        raise ValueError(f"Soft avoid duration must be between 1 and {SOFT_AVOID_MAX_GAMES} games")


@dataclass
class SoftAvoid:
    """Represents an active soft avoid."""
    id: int
    guild_id: int
    avoider_discord_id: int
    avoided_discord_id: int
    games_remaining: int
    created_at: int
    updated_at: int


@dataclass(frozen=True)
class SoftAvoidPurchaseResult:
    """Outcome of an atomic soft-avoid purchase attempt."""

    success: bool
    reason: Literal["already_active", "insufficient_balance"] | None
    balance: int
    avoid: SoftAvoid | None


class SoftAvoidRepository(BaseRepository, ISoftAvoidRepository):
    """Repository for soft avoid feature."""

    def create_or_reactivate_avoid(
        self,
        guild_id: int | None,
        avoider_id: int,
        avoided_id: int,
        games: int = SOFT_AVOID_GAMES,
    ) -> SoftAvoid:
        """
        Create a new soft avoid or reactivate an expired one.

        Active avoids cannot be extended. Returns the created/reactivated avoid.

        Raises:
            ValueError: If avoider_id equals avoided_id (cannot avoid oneself)
        """
        if avoider_id == avoided_id:
            raise ValueError("Cannot create soft avoid: avoider and avoided cannot be the same player")
        _validate_soft_avoid_games(games)

        normalized_guild = self.normalize_guild_id(guild_id)
        now = int(time.time())

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            cursor.execute(
                """
                SELECT id, games_remaining
                FROM soft_avoids
                WHERE guild_id = ? AND avoider_discord_id = ? AND avoided_discord_id = ?
                """,
                (normalized_guild, avoider_id, avoided_id),
            )
            existing = cursor.fetchone()
            if existing and existing["games_remaining"] > 0:
                raise ValueError(
                    f"Soft avoid is already active with {existing['games_remaining']} games remaining"
                )

            if existing:
                cursor.execute(
                    """
                    UPDATE soft_avoids
                    SET games_remaining = ?, updated_at = ?
                    WHERE id = ?
                    """,
                    (games, now, existing["id"]),
                )
            else:
                cursor.execute(
                    """
                    INSERT INTO soft_avoids
                        (guild_id, avoider_discord_id, avoided_discord_id, games_remaining, created_at, updated_at)
                    VALUES (?, ?, ?, ?, ?, ?)
                    """,
                    (normalized_guild, avoider_id, avoided_id, games, now, now),
                )

            # Fetch the resulting row to return complete avoid data
            cursor.execute(
                """
                SELECT id, guild_id, avoider_discord_id, avoided_discord_id,
                       games_remaining, created_at, updated_at
                FROM soft_avoids
                WHERE guild_id = ? AND avoider_discord_id = ? AND avoided_discord_id = ?
                """,
                (normalized_guild, avoider_id, avoided_id),
            )
            row = cursor.fetchone()

            if row is None:
                raise RuntimeError(
                    f"Soft avoid write succeeded but row not found for avoid "
                    f"({avoider_id} -> {avoided_id}, guild={normalized_guild})"
                )

            return SoftAvoid(
                id=row["id"],
                guild_id=row["guild_id"],
                avoider_discord_id=row["avoider_discord_id"],
                avoided_discord_id=row["avoided_discord_id"],
                games_remaining=row["games_remaining"],
                created_at=row["created_at"],
                updated_at=row["updated_at"],
            )

    def purchase_avoid(
        self,
        guild_id: int | None,
        avoider_id: int,
        avoided_id: int,
        *,
        cost: int,
        games: int = SOFT_AVOID_GAMES,
    ) -> SoftAvoidPurchaseResult:
        """Atomically debit and activate a soft avoid when allowed."""
        if avoider_id == avoided_id:
            raise ValueError("Cannot create soft avoid: avoider and avoided cannot be the same player")
        if cost < 0:
            raise ValueError("Soft avoid cost cannot be negative")
        _validate_soft_avoid_games(games)

        normalized_guild = self.normalize_guild_id(guild_id)
        now = int(time.time())

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            player_row = cursor.execute(
                """
                SELECT COALESCE(jopacoin_balance, 0) AS balance
                FROM players
                WHERE discord_id = ? AND guild_id = ?
                """,
                (avoider_id, normalized_guild),
            ).fetchone()
            if player_row is None:
                raise RuntimeError(
                    f"Registered player row not found for soft avoid buyer {avoider_id} "
                    f"in guild {normalized_guild}"
                )
            balance = int(player_row["balance"])

            existing = cursor.execute(
                """
                SELECT id, guild_id, avoider_discord_id, avoided_discord_id,
                       games_remaining, created_at, updated_at
                FROM soft_avoids
                WHERE guild_id = ? AND avoider_discord_id = ? AND avoided_discord_id = ?
                """,
                (normalized_guild, avoider_id, avoided_id),
            ).fetchone()
            if existing and existing["games_remaining"] > 0:
                return SoftAvoidPurchaseResult(
                    success=False,
                    reason="already_active",
                    balance=balance,
                    avoid=SoftAvoid(**dict(existing)),
                )
            if balance < cost:
                return SoftAvoidPurchaseResult(
                    success=False,
                    reason="insufficient_balance",
                    balance=balance,
                    avoid=None,
                )

            self._set_economy_ledger_context(
                cursor,
                source="soft_avoid",
                actor_id=avoider_id,
                related_type="player",
                related_id=avoided_id,
                reason="soft avoid purchase",
                metadata={"cost": cost, "games": games},
            )
            try:
                cursor.execute(
                    """
                    UPDATE players
                    SET jopacoin_balance = COALESCE(jopacoin_balance, 0) - ?,
                        updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                      AND COALESCE(jopacoin_balance, 0) >= ?
                    """,
                    (cost, avoider_id, normalized_guild, cost),
                )
                if cursor.rowcount == 0:
                    raise RuntimeError("Soft avoid debit failed after balance validation")
            finally:
                self._clear_economy_ledger_context(cursor)

            cursor.execute(
                """
                UPDATE players
                SET lowest_balance_ever = jopacoin_balance
                WHERE discord_id = ? AND guild_id = ?
                  AND (lowest_balance_ever IS NULL OR jopacoin_balance < lowest_balance_ever)
                """,
                (avoider_id, normalized_guild),
            )

            if existing:
                cursor.execute(
                    """
                    UPDATE soft_avoids
                    SET games_remaining = ?, updated_at = ?
                    WHERE id = ?
                    """,
                    (games, now, existing["id"]),
                )
            else:
                cursor.execute(
                    """
                    INSERT INTO soft_avoids
                        (guild_id, avoider_discord_id, avoided_discord_id, games_remaining, created_at, updated_at)
                    VALUES (?, ?, ?, ?, ?, ?)
                    """,
                    (normalized_guild, avoider_id, avoided_id, games, now, now),
                )

            row = cursor.execute(
                """
                SELECT id, guild_id, avoider_discord_id, avoided_discord_id,
                       games_remaining, created_at, updated_at
                FROM soft_avoids
                WHERE guild_id = ? AND avoider_discord_id = ? AND avoided_discord_id = ?
                """,
                (normalized_guild, avoider_id, avoided_id),
            ).fetchone()
            if row is None:
                raise RuntimeError("Soft avoid purchase succeeded but activation row was not found")

            return SoftAvoidPurchaseResult(
                success=True,
                reason=None,
                balance=balance - cost,
                avoid=SoftAvoid(**dict(row)),
            )

    def get_active_avoids_for_players(
        self,
        guild_id: int | None,
        player_ids: list[int],
    ) -> list[SoftAvoid]:
        """
        Get all active avoids where BOTH avoider and avoided are in player_ids.

        This is used during shuffle to find avoids relevant to the current match.
        Only returns avoids with games_remaining > 0.
        """
        if not player_ids:
            return []

        normalized_guild = self.normalize_guild_id(guild_id)
        placeholders = ",".join("?" * len(player_ids))

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"""
                SELECT id, guild_id, avoider_discord_id, avoided_discord_id,
                       games_remaining, created_at, updated_at
                FROM soft_avoids
                WHERE guild_id = ?
                  AND games_remaining > 0
                  AND avoider_discord_id IN ({placeholders})
                  AND avoided_discord_id IN ({placeholders})
                """,
                (normalized_guild, *player_ids, *player_ids),
            )
            rows = cursor.fetchall()

        return [
            SoftAvoid(
                id=row["id"],
                guild_id=row["guild_id"],
                avoider_discord_id=row["avoider_discord_id"],
                avoided_discord_id=row["avoided_discord_id"],
                games_remaining=row["games_remaining"],
                created_at=row["created_at"],
                updated_at=row["updated_at"],
            )
            for row in rows
        ]

    def get_user_avoids(
        self,
        guild_id: int | None,
        discord_id: int,
    ) -> list[SoftAvoid]:
        """
        Get all active avoids created by a user.

        Used for /shop avoids.
        """
        normalized_guild = self.normalize_guild_id(guild_id)

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT id, guild_id, avoider_discord_id, avoided_discord_id,
                       games_remaining, created_at, updated_at
                FROM soft_avoids
                WHERE guild_id = ? AND avoider_discord_id = ? AND games_remaining > 0
                ORDER BY created_at DESC
                """,
                (normalized_guild, discord_id),
            )
            rows = cursor.fetchall()

        return [
            SoftAvoid(
                id=row["id"],
                guild_id=row["guild_id"],
                avoider_discord_id=row["avoider_discord_id"],
                avoided_discord_id=row["avoided_discord_id"],
                games_remaining=row["games_remaining"],
                created_at=row["created_at"],
                updated_at=row["updated_at"],
            )
            for row in rows
        ]

    def decrement_avoids(
        self,
        guild_id: int | None,
        avoid_ids: list[int],
    ) -> int:
        """
        Decrement games_remaining for the given avoid IDs.

        Avoids that reach 0 are kept but will be filtered out in future queries.
        Returns the number of avoids decremented.
        """
        if not avoid_ids:
            return 0

        normalized_guild = self.normalize_guild_id(guild_id)
        now = int(time.time())
        placeholders = ",".join("?" * len(avoid_ids))

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"""
                UPDATE soft_avoids
                SET games_remaining = games_remaining - 1, updated_at = ?
                WHERE guild_id = ? AND id IN ({placeholders}) AND games_remaining > 0
                """,
                (now, normalized_guild, *avoid_ids),
            )
            return cursor.rowcount

    def delete_expired_avoids(self, guild_id: int | None) -> int:
        """
        Delete avoids with games_remaining = 0.

        This is optional cleanup - expired avoids are ignored anyway.
        Returns the number of avoids deleted.
        """
        normalized_guild = self.normalize_guild_id(guild_id)

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                DELETE FROM soft_avoids
                WHERE guild_id = ? AND games_remaining <= 0
                """,
                (normalized_guild,),
            )
            return cursor.rowcount
