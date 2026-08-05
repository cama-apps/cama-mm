"""Guild-scoped lobby moderation persistence and append-only audit history."""

from __future__ import annotations

from domain.models.moderation import (
    ActiveSuspensionExistsError,
    LobbySuspension,
    ModerationEvent,
    ModerationEventType,
    ModerationSource,
    SuspensionCompletion,
    SuspensionScope,
)
from repositories.base_repository import BaseRepository
from repositories.interfaces import IModerationRepository

_SUSPENSION_COLUMNS = """
    s.discord_id, s.guild_id, s.scope, s.completion, s.reason, s.source,
    s.actor_id, s.expires_at, s.matches_required,
    s.matches_remaining, s.pending_match_watermark, s.revision, s.active,
    s.created_at, s.updated_at
"""

_EVENT_COLUMNS = """
    event_id, guild_id, discord_id, event_type, source, actor_id, reason,
    scope, completion, expires_at, matches_required, matches_remaining,
    pending_match_watermark, wins_required, wins_remaining, match_id, created_at
"""


class ModerationRepository(BaseRepository, IModerationRepository):
    """Persist one current suspension per player and immutable event history."""

    @staticmethod
    def _to_suspension(row) -> LobbySuspension:
        matches_required = row["matches_required"]
        matches_remaining = row["matches_remaining"]
        matches_completed = (
            0
            if matches_required is None
            else max(0, int(matches_required) - int(matches_remaining))
        )
        return LobbySuspension(
            discord_id=int(row["discord_id"]),
            guild_id=int(row["guild_id"]),
            scope=SuspensionScope(row["scope"]),
            completion=SuspensionCompletion(row["completion"]),
            reason=row["reason"],
            source=ModerationSource(row["source"]),
            actor_id=int(row["actor_id"]),
            expires_at=row["expires_at"],
            matches_required=matches_required,
            pending_match_watermark=int(row["pending_match_watermark"]),
            matches_completed=matches_completed,
            matches_remaining=matches_remaining,
            revision=int(row["revision"]),
            active=bool(row["active"]),
            created_at=int(row["created_at"]),
            updated_at=int(row["updated_at"]),
        )

    @staticmethod
    def _to_event(row) -> ModerationEvent:
        return ModerationEvent(
            event_id=int(row["event_id"]),
            guild_id=int(row["guild_id"]),
            discord_id=int(row["discord_id"]),
            event_type=ModerationEventType(row["event_type"]),
            source=ModerationSource(row["source"]),
            actor_id=row["actor_id"],
            reason=row["reason"],
            scope=SuspensionScope(row["scope"]) if row["scope"] else None,
            completion=(
                SuspensionCompletion(row["completion"])
                if row["completion"]
                else None
            ),
            expires_at=row["expires_at"],
            matches_required=row["matches_required"],
            matches_remaining=row["matches_remaining"],
            pending_match_watermark=row["pending_match_watermark"],
            wins_required=row["wins_required"],
            wins_remaining=row["wins_remaining"],
            match_id=row["match_id"],
            created_at=int(row["created_at"]),
        )

    @staticmethod
    def _pending_match_watermark(conn, guild_id: int) -> int:
        row = conn.execute(
            """
            SELECT COALESCE(MAX(pending_match_id), 0) AS watermark
            FROM (
                SELECT pending_match_id
                FROM pending_matches
                WHERE guild_id = ?
                UNION ALL
                SELECT pending_match_id
                FROM matches
                WHERE guild_id = ? AND pending_match_id IS NOT NULL
            )
            """,
            (guild_id, guild_id),
        ).fetchone()
        return int(row["watermark"] or 0)

    @staticmethod
    def _get_suspension_row(conn, discord_id: int, guild_id: int):
        return conn.execute(
            f"""
            SELECT {_SUSPENSION_COLUMNS}
            FROM lobby_suspensions AS s
            WHERE s.discord_id = ? AND s.guild_id = ?
            """,  # noqa: S608 - projection is an internal constant
            (discord_id, guild_id),
        ).fetchone()

    @classmethod
    def _get_event_row(cls, conn, event_id: int):
        row = conn.execute(
            f"""
            SELECT {_EVENT_COLUMNS}
            FROM moderation_events
            WHERE event_id = ?
            """,  # noqa: S608 - projection is an internal constant
            (event_id,),
        ).fetchone()
        if row is None:
            raise RuntimeError("Moderation event write succeeded but row was not found")
        return row

    @classmethod
    def _insert_event(
        cls,
        conn,
        *,
        discord_id: int,
        guild_id: int,
        event_type: str,
        source: str,
        actor_id: int | None,
        reason: str | None,
        scope: str | None,
        completion: str | None,
        expires_at: int | None,
        matches_required: int | None,
        matches_remaining: int | None,
        pending_match_watermark: int | None,
        wins_required: int | None,
        wins_remaining: int | None,
        match_id: int | None,
        now: int,
    ) -> ModerationEvent:
        cursor = conn.execute(
            """
            INSERT INTO moderation_events (
                guild_id, discord_id, event_type, source, actor_id, reason,
                scope, completion, expires_at, matches_required,
                matches_remaining, pending_match_watermark, wins_required,
                wins_remaining, match_id, created_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            (
                guild_id,
                discord_id,
                event_type,
                source,
                actor_id,
                reason,
                scope,
                completion,
                expires_at,
                matches_required,
                matches_remaining,
                pending_match_watermark,
                wins_required,
                wins_remaining,
                match_id,
                now,
            ),
        )
        return cls._to_event(cls._get_event_row(conn, int(cursor.lastrowid)))

    def get_pending_match_watermark(self, guild_id: int | None) -> int:
        normalized = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            return self._pending_match_watermark(conn, normalized)

    def get_suspension(
        self,
        discord_id: int,
        guild_id: int | None,
    ) -> LobbySuspension | None:
        normalized = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            row = self._get_suspension_row(conn, discord_id, normalized)
        return self._to_suspension(row) if row else None

    def save_suspension(
        self,
        discord_id: int,
        guild_id: int | None,
        *,
        scope: str,
        completion: str,
        expires_at: int | None,
        matches_required: int | None,
        pending_match_watermark: int | None,
        reason: str,
        source: str,
        actor_id: int,
        replace: bool,
        now: int,
    ) -> LobbySuspension:
        normalized = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            existing = conn.execute(
                """
                SELECT active
                FROM lobby_suspensions
                WHERE discord_id = ? AND guild_id = ?
                """,
                (discord_id, normalized),
            ).fetchone()
            replacing = bool(existing and existing["active"])
            if replacing and not replace:
                raise ActiveSuspensionExistsError(
                    "Player already has an active lobby suspension"
                )

            watermark = (
                self._pending_match_watermark(conn, normalized)
                if pending_match_watermark is None
                else int(pending_match_watermark)
            )
            conn.execute(
                """
                INSERT INTO lobby_suspensions (
                    guild_id, discord_id, scope, completion, expires_at,
                    matches_required, matches_remaining,
                    pending_match_watermark, reason, source, actor_id, active,
                    created_at, updated_at
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?)
                ON CONFLICT(guild_id, discord_id) DO UPDATE SET
                    scope = excluded.scope,
                    completion = excluded.completion,
                    expires_at = excluded.expires_at,
                    matches_required = excluded.matches_required,
                    matches_remaining = excluded.matches_remaining,
                    pending_match_watermark = excluded.pending_match_watermark,
                    reason = excluded.reason,
                    source = excluded.source,
                    actor_id = excluded.actor_id,
                    active = 1,
                    revision = lobby_suspensions.revision + 1,
                    created_at = excluded.created_at,
                    updated_at = excluded.updated_at
                """,
                (
                    normalized,
                    discord_id,
                    scope,
                    completion,
                    expires_at,
                    matches_required,
                    matches_required,
                    watermark,
                    reason,
                    source,
                    actor_id,
                    now,
                    now,
                ),
            )
            self._insert_event(
                conn,
                discord_id=discord_id,
                guild_id=normalized,
                event_type=(
                    ModerationEventType.REPLACE.value
                    if replacing
                    else ModerationEventType.SUSPEND.value
                ),
                source=source,
                actor_id=actor_id,
                reason=reason,
                scope=scope,
                completion=completion,
                expires_at=expires_at,
                matches_required=matches_required,
                matches_remaining=matches_required,
                pending_match_watermark=watermark,
                wins_required=None,
                wins_remaining=None,
                match_id=None,
                now=now,
            )
            row = self._get_suspension_row(conn, discord_id, normalized)
        if row is None:
            raise RuntimeError("Suspension write succeeded but row was not found")
        return self._to_suspension(row)

    def close_suspension(
        self,
        discord_id: int,
        guild_id: int | None,
        *,
        event_type: str,
        source: str,
        actor_id: int | None,
        reason: str | None,
        expected_revision: int,
        now: int,
    ) -> LobbySuspension | None:
        normalized = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            current = self._get_suspension_row(conn, discord_id, normalized)
            if current is None or not current["active"]:
                return None
            if int(current["revision"]) != int(expected_revision):
                return None
            cursor = conn.execute(
                """
                UPDATE lobby_suspensions
                SET active = 0, revision = revision + 1, updated_at = ?
                WHERE discord_id = ? AND guild_id = ? AND active = 1
                  AND revision = ?
                """,
                (now, discord_id, normalized, expected_revision),
            )
            if cursor.rowcount != 1:
                return None
            self._insert_event(
                conn,
                discord_id=discord_id,
                guild_id=normalized,
                event_type=event_type,
                source=source,
                actor_id=actor_id,
                reason=reason,
                scope=current["scope"],
                completion=current["completion"],
                expires_at=current["expires_at"],
                matches_required=current["matches_required"],
                matches_remaining=current["matches_remaining"],
                pending_match_watermark=current["pending_match_watermark"],
                wins_required=None,
                wins_remaining=None,
                match_id=None,
                now=now,
            )
            row = self._get_suspension_row(conn, discord_id, normalized)
        return self._to_suspension(row) if row else None

    def list_suspensions(
        self,
        guild_id: int | None,
        *,
        active_only: bool = True,
    ) -> list[LobbySuspension]:
        normalized = self.normalize_guild_id(guild_id)
        active_clause = "AND s.active = 1" if active_only else ""
        with self.connection() as conn:
            rows = conn.execute(
                f"""
                SELECT {_SUSPENSION_COLUMNS}
                FROM lobby_suspensions AS s
                WHERE s.guild_id = ? {active_clause}
                ORDER BY s.created_at, s.discord_id
                """,  # noqa: S608 - clause/projection are internal constants
                (normalized,),
            ).fetchall()
        return [self._to_suspension(row) for row in rows]

    def record_event(
        self,
        discord_id: int,
        guild_id: int | None,
        *,
        event_type: str,
        source: str,
        actor_id: int | None,
        reason: str | None,
        scope: str | None = None,
        completion: str | None = None,
        expires_at: int | None = None,
        matches_required: int | None = None,
        matches_remaining: int | None = None,
        pending_match_watermark: int | None = None,
        wins_required: int | None = None,
        wins_remaining: int | None = None,
        match_id: int | None = None,
        now: int,
    ) -> ModerationEvent:
        normalized = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            return self._insert_event(
                conn,
                discord_id=discord_id,
                guild_id=normalized,
                event_type=event_type,
                source=source,
                actor_id=actor_id,
                reason=reason,
                scope=scope,
                completion=completion,
                expires_at=expires_at,
                matches_required=matches_required,
                matches_remaining=matches_remaining,
                pending_match_watermark=pending_match_watermark,
                wins_required=wins_required,
                wins_remaining=wins_remaining,
                match_id=match_id,
                now=now,
            )

    def get_history(
        self,
        guild_id: int | None,
        discord_id: int | None = None,
        *,
        limit: int = 50,
        offset: int = 0,
    ) -> list[ModerationEvent]:
        normalized = self.normalize_guild_id(guild_id)
        params: tuple[int, ...]
        if discord_id is None:
            where = "guild_id = ?"
            params = (normalized, limit, offset)
        else:
            where = "guild_id = ? AND discord_id = ?"
            params = (normalized, discord_id, limit, offset)
        with self.connection() as conn:
            rows = conn.execute(
                f"""
                SELECT {_EVENT_COLUMNS}
                FROM moderation_events
                WHERE {where}
                ORDER BY event_id DESC
                LIMIT ? OFFSET ?
                """,  # noqa: S608 - where/projection are internal constants
                params,
            ).fetchall()
        return [self._to_event(row) for row in rows]

    def get_completion_events_for_match(
        self,
        guild_id: int | None,
        match_id: int,
    ) -> list[ModerationEvent]:
        """Return automatic moderation completions caused by one match."""
        normalized = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            rows = conn.execute(
                f"""
                SELECT {_EVENT_COLUMNS}
                FROM moderation_events
                WHERE guild_id = ? AND match_id = ?
                  AND event_type IN ('complete', 'lowprio_complete')
                ORDER BY event_id
                """,  # noqa: S608 - projection is an internal constant
                (normalized, match_id),
            ).fetchall()
        return [self._to_event(row) for row in rows]
