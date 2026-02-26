"""
Repository for daily MTG mana land assignments.
"""

from repositories.base_repository import BaseRepository
from repositories.interfaces import IManaRepository


class ManaRepository(BaseRepository, IManaRepository):
    """Stores and retrieves daily mana assignments (one row per player per guild)."""

    def get_mana(self, discord_id: int, guild_id: int | None) -> dict | None:
        """Return {current_land, assigned_date} for the player, or None if never assigned."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT current_land, assigned_date FROM player_mana WHERE discord_id = ? AND guild_id = ?",
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

    def get_all_mana(self, guild_id: int | None) -> list[dict]:
        """Return all mana rows for the guild, ordered by current_land then discord_id."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT discord_id, current_land, assigned_date
                FROM player_mana
                WHERE guild_id = ?
                ORDER BY current_land, discord_id
                """,
                (gid,),
            )
            return [dict(row) for row in cursor.fetchall()]
