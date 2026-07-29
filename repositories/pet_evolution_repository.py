"""Bounded persistence for Camagotchi upbringing and adult evolution."""

from __future__ import annotations

from collections.abc import Callable, Mapping

from domain.pet_evolution import (
    ActivityRule,
    EvolutionDecision,
    PetActivity,
    PetInstinct,
    activity_rule,
)
from repositories.base_repository import BaseRepository

_DAILY_INSTINCT_CAP = 3
_DAY_SECONDS = 86400


class PetEvolutionRepository(BaseRepository):
    @staticmethod
    def _eligible_pet(conn, discord_id: int, guild_id: int, occurred_at: int):
        return conn.execute(
            """
            SELECT pet_id, evolution_started_at, evolution_due_at
            FROM pets
            WHERE discord_id = ? AND guild_id = ?
              AND died_at IS NULL AND evolved_at IS NULL
              AND evolution_started_at IS NOT NULL
              AND evolution_due_at IS NOT NULL
              AND evolution_started_at <= ?
              AND evolution_due_at > ?
            """,
            (discord_id, guild_id, occurred_at, occurred_at),
        ).fetchone()

    def record_activity(
        self,
        discord_id: int,
        guild_id: int | None,
        *,
        activity: PetActivity,
        source_key: str,
        occurred_at: int,
    ) -> int:
        """Record one eligible activity and return the points actually awarded."""
        activity = PetActivity(activity)
        source_key = str(source_key).strip()
        if not source_key:
            raise ValueError("source_key must not be empty")
        rule = activity_rule(activity)
        gid = self.normalize_guild_id(guild_id)

        # The overwhelming majority of server actions have no eligible pet.
        # Keep that path read-only so it never acquires SQLite's write lock.
        with self.connection() as conn:
            pet = self._eligible_pet(conn, discord_id, gid, occurred_at)
            if pet is None:
                return 0
            upbringing_day = self._upbringing_day(pet, occurred_at)
            if upbringing_day is None:
                return 0
            daily = self._daily_row(
                conn,
                int(pet["pet_id"]),
                upbringing_day,
                rule.instinct,
            )
            if self._available_points(daily, rule, activity, source_key) <= 0:
                return 0

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            # Re-check after taking the lock: death/evolution may have won the
            # race between the cheap preflight and this bounded write.
            pet = self._eligible_pet(conn, discord_id, gid, occurred_at)
            if pet is None:
                return 0
            upbringing_day = self._upbringing_day(pet, occurred_at)
            if upbringing_day is None:
                return 0

            daily = self._daily_row(
                conn,
                int(pet["pet_id"]),
                upbringing_day,
                rule.instinct,
            )
            awarded = self._available_points(daily, rule, activity, source_key)
            if awarded <= 0:
                return 0

            if daily is None:
                cursor.execute(
                    """
                    INSERT INTO pet_evolution_daily (
                        pet_id, guild_id, upbringing_day, instinct, score,
                        first_activity, first_source_key
                    )
                    VALUES (?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        pet["pet_id"],
                        gid,
                        upbringing_day,
                        rule.instinct.value,
                        awarded,
                        activity.value,
                        source_key,
                    ),
                )
                return awarded

            cursor.execute(
                """
                UPDATE pet_evolution_daily
                SET score = score + ?,
                    second_activity = ?,
                    second_source_key = ?
                WHERE pet_id = ? AND upbringing_day = ? AND instinct = ?
                """,
                (
                    awarded,
                    activity.value,
                    source_key,
                    pet["pet_id"],
                    upbringing_day,
                    rule.instinct.value,
                ),
            )
            return awarded

    @staticmethod
    def _upbringing_day(pet: Mapping, occurred_at: int) -> int | None:
        day = (occurred_at - int(pet["evolution_started_at"])) // _DAY_SECONDS
        return int(day) if 0 <= day <= 6 else None

    @staticmethod
    def _daily_row(
        conn,
        pet_id: int,
        upbringing_day: int,
        instinct: PetInstinct,
    ):
        return conn.execute(
            """
            SELECT score, first_activity, first_source_key,
                   second_activity, second_source_key
            FROM pet_evolution_daily
            WHERE pet_id = ? AND upbringing_day = ? AND instinct = ?
            """,
            (pet_id, upbringing_day, instinct.value),
        ).fetchone()

    @staticmethod
    def _available_points(
        daily,
        rule: ActivityRule,
        activity: PetActivity,
        source_key: str,
    ) -> int:
        if daily is None:
            return min(rule.points, _DAILY_INSTINCT_CAP)
        if source_key in (
            daily["first_source_key"],
            daily["second_source_key"],
        ):
            return 0
        if rule.once_per_day and activity.value in (
            daily["first_activity"],
            daily["second_activity"],
        ):
            return 0
        return min(rule.points, _DAILY_INSTINCT_CAP - int(daily["score"]))

    def get_scores(
        self,
        pet_id: int,
        guild_id: int | None,
    ) -> dict[PetInstinct, int]:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            return self._scores(conn, pet_id, gid)

    @staticmethod
    def _scores(conn, pet_id: int, guild_id: int) -> dict[PetInstinct, int]:
        totals = dict.fromkeys(PetInstinct, 0)
        rows = conn.execute(
            """
            SELECT instinct, SUM(score) AS total
            FROM pet_evolution_daily
            WHERE pet_id = ? AND guild_id = ?
            GROUP BY instinct
            """,
            (pet_id, guild_id),
        ).fetchall()
        for row in rows:
            totals[PetInstinct(row["instinct"])] = int(row["total"])
        return totals

    def find_due_refs(self, now: int, *, limit: int = 100) -> list[tuple[int, int]]:
        """Return a bounded, indexed batch of unresolved due pets."""
        with self.connection() as conn:
            rows = conn.execute(
                """
                SELECT pet_id, guild_id
                FROM pets
                WHERE died_at IS NULL AND evolved_at IS NULL
                  AND evolution_due_at IS NOT NULL
                  AND evolution_due_at <= ?
                ORDER BY evolution_due_at, pet_id
                LIMIT ?
                """,
                (now, limit),
            ).fetchall()
        return [(int(row["pet_id"]), int(row["guild_id"])) for row in rows]

    def claim_evolution(
        self,
        pet_id: int,
        guild_id: int | None,
        *,
        expected_due_at: int,
        decide: Callable[[Mapping[PetInstinct, int]], EvolutionDecision],
    ) -> bool:
        """Resolve scores and persist the calling under one write lock."""
        gid = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            eligible = conn.execute(
                """
                SELECT pet_id
                FROM pets
                WHERE pet_id = ? AND guild_id = ?
                  AND evolved_at IS NULL
                  AND evolution_due_at = ?
                  AND (died_at IS NULL OR died_at > evolution_due_at)
                """,
                (pet_id, gid, expected_due_at),
            ).fetchone()
            if eligible is None:
                return False
            decision = decide(self._scores(conn, pet_id, gid))
            cursor = conn.execute(
                """
                UPDATE pets
                SET evolved_at = evolution_due_at,
                    evolution_calling = ?,
                    evolution_primary = ?,
                    evolution_secondary = ?
                WHERE pet_id = ? AND guild_id = ?
                  AND evolved_at IS NULL
                  AND evolution_due_at = ?
                  AND (died_at IS NULL OR died_at > evolution_due_at)
                """,
                (
                    decision.calling.value,
                    decision.primary.value if decision.primary is not None else None,
                    (
                        decision.secondary.value
                        if decision.secondary is not None
                        else None
                    ),
                    pet_id,
                    gid,
                    expected_due_at,
                ),
            )
            return cursor.rowcount == 1

    def find_unannounced_refs(
        self,
        *,
        limit: int = 100,
    ) -> list[tuple[int, int]]:
        with self.connection() as conn:
            rows = conn.execute(
                """
                SELECT pet_id, guild_id
                FROM pets
                WHERE evolved_at IS NOT NULL
                  AND evolution_announced_at IS NULL
                ORDER BY evolved_at, pet_id
                LIMIT ?
                """,
                (limit,),
            ).fetchall()
        return [(int(row["pet_id"]), int(row["guild_id"])) for row in rows]

    def callings_raised(
        self,
        discord_id: int,
        guild_id: int | None,
    ) -> list[str]:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            rows = conn.execute(
                """
                SELECT DISTINCT evolution_calling
                FROM pets
                WHERE discord_id = ? AND guild_id = ?
                  AND evolved_at IS NOT NULL
                  AND evolution_calling IS NOT NULL
                ORDER BY evolution_calling
                """,
                (discord_id, gid),
            ).fetchall()
        return [str(row["evolution_calling"]) for row in rows]

    def mark_announced(
        self,
        pet_id: int,
        guild_id: int | None,
        announced_at: int,
    ) -> None:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            conn.execute(
                """
                UPDATE pets
                SET evolution_announced_at = ?
                WHERE pet_id = ? AND guild_id = ?
                  AND evolved_at IS NOT NULL
                """,
                (announced_at, pet_id, gid),
            )
