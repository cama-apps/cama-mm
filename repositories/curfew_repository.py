"""
Repository for named per-player curfew windows.
"""

from domain.models.curfew import CurfewWindow
from repositories.base_repository import BaseRepository
from repositories.interfaces import ICurfewRepository


class CurfewRepository(BaseRepository, ICurfewRepository):
    def add_or_replace(
        self,
        discord_id: int,
        guild_id: int,
        name: str,
        *,
        start_hour: int,
        start_minute: int,
        end_hour: int,
        end_minute: int,
        timezone: str | None,
    ) -> None:
        """Create a named window, or overwrite it if that name already exists for this player."""
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO player_curfew_windows
                    (discord_id, guild_id, name, start_hour, start_minute, end_hour, end_minute, timezone)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(discord_id, guild_id, name) DO UPDATE SET
                    start_hour = excluded.start_hour,
                    start_minute = excluded.start_minute,
                    end_hour = excluded.end_hour,
                    end_minute = excluded.end_minute,
                    timezone = excluded.timezone
                """,
                (
                    discord_id,
                    guild_id,
                    name,
                    start_hour,
                    start_minute,
                    end_hour,
                    end_minute,
                    timezone,
                ),
            )

    def remove(self, discord_id: int, guild_id: int, name: str) -> bool:
        """Delete a named window. Returns True if a row was actually removed."""
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                DELETE FROM player_curfew_windows
                WHERE discord_id = ? AND guild_id = ? AND name = ?
                """,
                (discord_id, guild_id, name),
            )
            return cursor.rowcount > 0

    def list_for_player(self, discord_id: int, guild_id: int) -> list[CurfewWindow]:
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT * FROM player_curfew_windows
                WHERE discord_id = ? AND guild_id = ?
                ORDER BY name
                """,
                (discord_id, guild_id),
            )
            return [self._row_to_window(row) for row in cursor.fetchall()]

    def list_for_players(
        self, discord_ids: list[int], guild_id: int
    ) -> dict[int, list[CurfewWindow]]:
        """Bulk-fetch windows for a set of players (e.g. everyone in a lobby)."""
        if not discord_ids:
            return {}
        guild_id = self.normalize_guild_id(guild_id)
        placeholders = ",".join("?" * len(discord_ids))
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"""
                SELECT * FROM player_curfew_windows
                WHERE guild_id = ? AND discord_id IN ({placeholders})
                """,
                (guild_id, *discord_ids),
            )
            result: dict[int, list[CurfewWindow]] = {}
            for row in cursor.fetchall():
                window = self._row_to_window(row)
                result.setdefault(window.discord_id, []).append(window)
            return result

    def _row_to_window(self, row) -> CurfewWindow:
        return CurfewWindow(
            discord_id=row["discord_id"],
            guild_id=row["guild_id"],
            name=row["name"],
            start_hour=row["start_hour"],
            start_minute=row["start_minute"],
            end_hour=row["end_hour"],
            end_minute=row["end_minute"],
            timezone=row["timezone"],
        )
