import time

from repositories.base_repository import BaseRepository
from repositories.interfaces import IReminderRepository

_VALID_TYPES = {"wheel", "trivia", "betting", "dig", "lobby", "pet"}


class NotificationRepository(BaseRepository, IReminderRepository):
    def get_preferences(self, discord_id: int, guild_id: int) -> dict:
        normalized = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT wheel_enabled, trivia_enabled, betting_enabled, dig_enabled, "
                "lobby_enabled, pet_enabled "
                "FROM reminder_preferences WHERE discord_id = ? AND guild_id = ?",
                (discord_id, normalized),
            )
            row = cursor.fetchone()
        if row is None:
            return {
                "wheel_enabled": False,
                "trivia_enabled": False,
                "betting_enabled": False,
                "dig_enabled": False,
                "lobby_enabled": False,
                "pet_enabled": False,
            }
        return {
            "wheel_enabled": bool(row["wheel_enabled"]),
            "trivia_enabled": bool(row["trivia_enabled"]),
            "betting_enabled": bool(row["betting_enabled"]),
            "dig_enabled": bool(row["dig_enabled"]),
            "lobby_enabled": bool(row["lobby_enabled"]),
            "pet_enabled": bool(row["pet_enabled"]),
        }

    def set_preference(
        self, discord_id: int, guild_id: int, reminder_type: str, enabled: bool
    ) -> None:
        if reminder_type not in _VALID_TYPES:
            raise ValueError(f"Invalid reminder_type: {reminder_type!r}")
        # Coupling: each _VALID_TYPES entry must have a matching <type>_enabled
        # column in reminder_preferences (the column name is derived below).
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
        # Coupling: see set_preference — col name derived from validated _VALID_TYPES entry.
        col = f"{reminder_type}_enabled"
        normalized = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"SELECT discord_id FROM reminder_preferences WHERE guild_id = ? AND {col} = 1",
                (normalized,),
            )
            return [row["discord_id"] for row in cursor.fetchall()]

    def get_enabled_users_by_type_bulk(
        self,
        guild_id: int,
        reminder_types: tuple[str, ...],
    ) -> dict[str, list[int]]:
        """Load several subscriber lists from one preference snapshot."""
        valid_types = tuple(
            dict.fromkeys(
                reminder_type for reminder_type in reminder_types if reminder_type in _VALID_TYPES
            )
        )
        subscribers = {reminder_type: [] for reminder_type in valid_types}
        if not valid_types:
            return subscribers

        columns = ", ".join(
            ["discord_id", *(f"{reminder_type}_enabled" for reminder_type in valid_types)]
        )
        normalized = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            rows = conn.execute(
                f"SELECT {columns} FROM reminder_preferences WHERE guild_id = ?",
                (normalized,),
            ).fetchall()

        for row in rows:
            for reminder_type in valid_types:
                if row[f"{reminder_type}_enabled"]:
                    subscribers[reminder_type].append(row["discord_id"])
        return subscribers

    def add_lobby_target_subscription(
        self,
        subscriber_id: int,
        target_id: int,
        guild_id: int,
    ) -> bool:
        """Persist one guild-scoped lobby target subscription idempotently."""
        normalized = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            # Assign the watermark only after this insert owns the database
            # write lock. A join that committed first can therefore exclude a
            # later subscription from that already-completed signup event.
            now = time.time_ns()
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT OR IGNORE INTO lobby_target_subscriptions (
                    guild_id, target_id, subscriber_id, created_at
                )
                VALUES (?, ?, ?, ?)
                """,
                (normalized, target_id, subscriber_id, now),
            )
            return cursor.rowcount == 1

    def has_lobby_target_subscription(
        self,
        subscriber_id: int,
        target_id: int,
        guild_id: int,
    ) -> bool:
        """Return whether this one-shot target subscription already exists."""
        normalized = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT 1
                FROM lobby_target_subscriptions
                WHERE guild_id = ? AND target_id = ? AND subscriber_id = ?
                """,
                (normalized, target_id, subscriber_id),
            )
            return cursor.fetchone() is not None

    def claim_lobby_target_subscribers(
        self,
        target_id: int,
        guild_id: int,
        created_at_cutoff: int | None = None,
    ) -> list[int]:
        """Atomically read and consume subscriptions for a target's next join."""
        normalized = self.normalize_guild_id(guild_id)
        cutoff = time.time_ns() if created_at_cutoff is None else created_at_cutoff
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            rows = cursor.execute(
                """
                SELECT subscriber_id
                FROM lobby_target_subscriptions
                WHERE guild_id = ? AND target_id = ? AND created_at <= ?
                ORDER BY created_at, subscriber_id
                """,
                (normalized, target_id, cutoff),
            ).fetchall()
            if not rows:
                return []

            cursor.execute(
                """
                DELETE FROM lobby_target_subscriptions
                WHERE guild_id = ? AND target_id = ? AND created_at <= ?
                """,
                (normalized, target_id, cutoff),
            )
            return [int(row["subscriber_id"]) for row in rows]
