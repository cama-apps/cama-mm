import time

from repositories.base_repository import BaseRepository
from repositories.interfaces import IReminderRepository

_VALID_TYPES = {"wheel", "trivia", "betting"}


class NotificationRepository(BaseRepository, IReminderRepository):
    def get_preferences(self, discord_id: int, guild_id: int) -> dict:
        normalized = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT wheel_enabled, trivia_enabled, betting_enabled "
                "FROM reminder_preferences WHERE discord_id = ? AND guild_id = ?",
                (discord_id, normalized),
            )
            row = cursor.fetchone()
        if row is None:
            return {"wheel_enabled": False, "trivia_enabled": False, "betting_enabled": False}
        return {
            "wheel_enabled": bool(row["wheel_enabled"]),
            "trivia_enabled": bool(row["trivia_enabled"]),
            "betting_enabled": bool(row["betting_enabled"]),
        }

    def set_preference(
        self, discord_id: int, guild_id: int, reminder_type: str, enabled: bool
    ) -> None:
        if reminder_type not in _VALID_TYPES:
            raise ValueError(f"Invalid reminder_type: {reminder_type!r}")
        col = f"{reminder_type}_enabled"
        normalized = self.normalize_guild_id(guild_id)
        now = int(time.time())
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"INSERT INTO reminder_preferences (discord_id, guild_id, {col}, updated_at) "
                f"VALUES (?, ?, ?, ?) "
                f"ON CONFLICT(discord_id, guild_id) DO UPDATE SET {col} = excluded.{col}, "
                f"updated_at = excluded.updated_at",
                (discord_id, normalized, int(enabled), now),
            )

    def get_enabled_users_for_type(self, guild_id: int, reminder_type: str) -> list[int]:
        if reminder_type not in _VALID_TYPES:
            return []
        col = f"{reminder_type}_enabled"
        normalized = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"SELECT discord_id FROM reminder_preferences WHERE guild_id = ? AND {col} = 1",
                (normalized,),
            )
            return [row["discord_id"] for row in cursor.fetchall()]
