"""
Repository for soft avoid data access.
"""

import logging
import time
from dataclasses import dataclass

from repositories.base_repository import BaseRepository

logger = logging.getLogger("cama_bot.repositories.soft_avoid")


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


class SoftAvoidRepository(BaseRepository):
    """Repository for soft avoid feature."""

    def create_or_extend_avoid(
        self,
        guild_id: int | None,
        avoider_id: int,
        avoided_id: int,
        games: int = 10,
    ) -> SoftAvoid:
        """
        Create a new soft avoid or extend existing one.

        If an avoid already exists for this pair, adds games to games_remaining.
        Returns the created/updated avoid.

        Raises:
            ValueError: If avoider_id equals avoided_id (cannot avoid oneself)
        """
        if avoider_id == avoided_id:
            raise ValueError("Cannot create soft avoid: avoider and avoided cannot be the same player")

        normalized_guild = self.normalize_guild_id(guild_id)
        now = int(time.time())

        with self.connection() as conn:
            cursor = conn.cursor()

            # Use UPSERT for atomic create-or-extend operation
            # ON CONFLICT updates games_remaining by adding the new games
            cursor.execute(
                """
                INSERT INTO soft_avoids
                    (guild_id, avoider_discord_id, avoided_discord_id, games_remaining, created_at, updated_at)
                VALUES (?, ?, ?, ?, ?, ?)
                ON CONFLICT(guild_id, avoider_discord_id, avoided_discord_id)
                DO UPDATE SET
                    games_remaining = games_remaining + excluded.games_remaining,
                    updated_at = excluded.updated_at
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
                    f"UPSERT succeeded but row not found for avoid "
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

        Used for /myavoids command.
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
