"""Repository for per-(player, guild) quest progression state.

Quest state intentionally survives prestige resets — quests are lifetime
one-shots per guild, so a player partway through an arc keeps their
progress across runs. Completed quests are kept in a per-row JSON array so
the eligibility check in ``DigQuestService`` is one round-trip.
"""

from __future__ import annotations

import json
import time
from dataclasses import dataclass, field

from repositories.base_repository import BaseRepository, safe_json_loads


@dataclass(frozen=True)
class QuestState:
    discord_id: int
    guild_id: int
    active_quest_id: str | None = None
    active_quest_step: int | None = None
    completed_quests: tuple[str, ...] = field(default_factory=tuple)
    last_updated_at: int | None = None

    def has_completed(self, quest_id: str) -> bool:
        return quest_id in self.completed_quests

    def is_active(self, quest_id: str) -> bool:
        return self.active_quest_id == quest_id and self.active_quest_step is not None


class DigQuestRepository(BaseRepository):
    """Persistence for quest progress. One active quest at a time per player."""

    def get_state(self, discord_id: int, guild_id: int | None) -> QuestState:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT active_quest_id, active_quest_step,
                       completed_quests, last_updated_at
                  FROM dig_quests
                 WHERE discord_id = ? AND guild_id = ?
                """,
                (discord_id, gid),
            )
            row = cursor.fetchone()
        if row is None:
            return QuestState(discord_id=discord_id, guild_id=gid)
        completed_raw = row["completed_quests"] if row["completed_quests"] else "[]"
        completed = safe_json_loads(completed_raw, [], context="dig_quests.completed_quests")
        if not isinstance(completed, list):
            completed = []
        step_raw = row["active_quest_step"]
        return QuestState(
            discord_id=discord_id,
            guild_id=gid,
            active_quest_id=row["active_quest_id"],
            active_quest_step=int(step_raw) if step_raw is not None else None,
            completed_quests=tuple(str(q) for q in completed),
            last_updated_at=int(row["last_updated_at"]) if row["last_updated_at"] is not None else None,
        )

    def set_active(
        self,
        discord_id: int,
        guild_id: int | None,
        quest_id: str,
        step: int,
    ) -> QuestState:
        """Start or advance a quest. Returns the resulting state.

        Refuses to overwrite a different active quest (the service layer is
        responsible for ensuring one-at-a-time concurrency, but defense in
        depth: raise if a mismatched active row exists).
        """
        gid = self.normalize_guild_id(guild_id)
        now = int(time.time())
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT active_quest_id, completed_quests
                  FROM dig_quests
                 WHERE discord_id = ? AND guild_id = ?
                """,
                (discord_id, gid),
            )
            row = cursor.fetchone()
            completed: list[str] = []
            if row is None:
                cursor.execute(
                    """
                    INSERT INTO dig_quests
                        (discord_id, guild_id, active_quest_id,
                         active_quest_step, completed_quests, last_updated_at)
                    VALUES (?, ?, ?, ?, '[]', ?)
                    """,
                    (discord_id, gid, quest_id, int(step), now),
                )
            else:
                existing_id = row["active_quest_id"]
                if existing_id is not None and existing_id != quest_id:
                    raise ValueError(
                        f"player {discord_id} guild {gid} already has active quest "
                        f"{existing_id!r}; refusing to overwrite with {quest_id!r}"
                    )
                parsed = safe_json_loads(
                    row["completed_quests"] or "[]",
                    [],
                    context="dig_quests.completed_quests",
                )
                if isinstance(parsed, list):
                    completed = [str(q) for q in parsed]
                cursor.execute(
                    """
                    UPDATE dig_quests
                       SET active_quest_id = ?,
                           active_quest_step = ?,
                           last_updated_at = ?
                     WHERE discord_id = ? AND guild_id = ?
                    """,
                    (quest_id, int(step), now, discord_id, gid),
                )
        return QuestState(
            discord_id=discord_id,
            guild_id=gid,
            active_quest_id=quest_id,
            active_quest_step=int(step),
            completed_quests=tuple(completed),
            last_updated_at=now,
        )

    def complete_quest(
        self,
        discord_id: int,
        guild_id: int | None,
        quest_id: str,
    ) -> QuestState:
        """Clear active state and append quest_id to completed_quests if absent.

        Idempotent: re-completing the same quest is a no-op (the id is only
        appended once).
        """
        gid = self.normalize_guild_id(guild_id)
        now = int(time.time())
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT completed_quests FROM dig_quests
                 WHERE discord_id = ? AND guild_id = ?
                """,
                (discord_id, gid),
            )
            row = cursor.fetchone()
            if row is None:
                completed = [quest_id]
                cursor.execute(
                    """
                    INSERT INTO dig_quests
                        (discord_id, guild_id, active_quest_id,
                         active_quest_step, completed_quests, last_updated_at)
                    VALUES (?, ?, NULL, NULL, ?, ?)
                    """,
                    (discord_id, gid, json.dumps(completed), now),
                )
            else:
                parsed = safe_json_loads(
                    row["completed_quests"] or "[]",
                    [],
                    context="dig_quests.completed_quests",
                )
                completed = [str(q) for q in parsed] if isinstance(parsed, list) else []
                if quest_id not in completed:
                    completed.append(quest_id)
                cursor.execute(
                    """
                    UPDATE dig_quests
                       SET active_quest_id = NULL,
                           active_quest_step = NULL,
                           completed_quests = ?,
                           last_updated_at = ?
                     WHERE discord_id = ? AND guild_id = ?
                    """,
                    (json.dumps(completed), now, discord_id, gid),
                )
        return QuestState(
            discord_id=discord_id,
            guild_id=gid,
            active_quest_id=None,
            active_quest_step=None,
            completed_quests=tuple(completed),
            last_updated_at=now,
        )

    def abandon_active(self, discord_id: int, guild_id: int | None) -> QuestState:
        """Test/admin escape hatch: clear active quest without completing it."""
        gid = self.normalize_guild_id(guild_id)
        now = int(time.time())
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE dig_quests
                   SET active_quest_id = NULL,
                       active_quest_step = NULL,
                       last_updated_at = ?
                 WHERE discord_id = ? AND guild_id = ?
                """,
                (now, discord_id, gid),
            )
        return self.get_state(discord_id, gid)
