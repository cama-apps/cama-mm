"""
Repository for managing per-guild configuration.
"""

from config import AI_FEATURES_ENABLED
from repositories.base_repository import BaseRepository
from repositories.interfaces import IGuildConfigRepository


class GuildConfigRepository(BaseRepository, IGuildConfigRepository):
    """
    Handles CRUD operations for guild-specific configuration.
    """

    def get_config(self, guild_id: int) -> dict | None:
        """Get configuration for a guild."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT guild_id, league_id, auto_enrich_matches, created_at, updated_at
                FROM guild_config
                WHERE guild_id = ?
                """,
                (guild_id,),
            )
            row = cursor.fetchone()
            return dict(row) if row else None

    def set_league_id(self, guild_id: int, league_id: int) -> None:
        """Set the league ID for a guild."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO guild_config (guild_id, league_id)
                VALUES (?, ?)
                ON CONFLICT(guild_id) DO UPDATE SET
                    league_id = ?,
                    updated_at = CURRENT_TIMESTAMP
                """,
                (guild_id, league_id, league_id),
            )

    def get_league_id(self, guild_id: int) -> int | None:
        """Get the league ID for a guild."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT league_id FROM guild_config WHERE guild_id = ?",
                (guild_id,),
            )
            row = cursor.fetchone()
            return row["league_id"] if row else None

    def set_auto_enrich(self, guild_id: int, enabled: bool) -> None:
        """Set whether to auto-enrich matches for a guild."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO guild_config (guild_id, auto_enrich_matches)
                VALUES (?, ?)
                ON CONFLICT(guild_id) DO UPDATE SET
                    auto_enrich_matches = ?,
                    updated_at = CURRENT_TIMESTAMP
                """,
                (guild_id, 1 if enabled else 0, 1 if enabled else 0),
            )

    def get_auto_enrich(self, guild_id: int) -> bool:
        """Get whether to auto-enrich matches for a guild. Defaults to True."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT auto_enrich_matches FROM guild_config WHERE guild_id = ?",
                (guild_id,),
            )
            row = cursor.fetchone()
            return bool(row["auto_enrich_matches"]) if row else True

    def set_ai_enabled(self, guild_id: int, enabled: bool) -> None:
        """Set whether AI features are enabled for a guild."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO guild_config (guild_id, ai_features_enabled)
                VALUES (?, ?)
                ON CONFLICT(guild_id) DO UPDATE SET
                    ai_features_enabled = ?,
                    updated_at = CURRENT_TIMESTAMP
                """,
                (guild_id, 1 if enabled else 0, 1 if enabled else 0),
            )

    def get_ai_enabled(self, guild_id: int) -> bool:
        """Get whether AI features are enabled for a guild. Defaults to AI_FEATURES_ENABLED config."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT ai_features_enabled FROM guild_config WHERE guild_id = ?",
                (guild_id,),
            )
            row = cursor.fetchone()
            # Default to config value when no config exists or column is NULL
            if row is None:
                return AI_FEATURES_ENABLED
            if row["ai_features_enabled"] is None:
                return AI_FEATURES_ENABLED
            return bool(row["ai_features_enabled"])
