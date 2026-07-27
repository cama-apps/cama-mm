"""Repository for cama pets: pet rows, supplies, and weekly refund claims.

Follows the newer vertical pattern (mafia/duel): extends BaseRepository only
and returns domain objects directly. Every method that both moves jopacoin
and mutates pet state does so inside one atomic_transaction(), with economy
ledger context set around the balance write so the trigger-based ledger
records the spend.

Validation errors are raised as ``ValueError(code)`` with machine-readable
codes; the service layer maps them to Result failures:
    insufficient_funds, already_has_pet, no_pet, pet_dead, stale_pet,
    no_supplies, feed_cap, already_pampered, stack_cap, not_owned
"""

from __future__ import annotations

import dataclasses
import json
import logging
import sqlite3

from domain.models.pet import Pet, RefundNotice, RefundPayout
from domain.pet_constants import UNHATCHED_SPECIES
from repositories.base_repository import BaseRepository, safe_json_loads

logger = logging.getLogger("cama_bot.repositories.pet")

_PET_COLUMNS = (
    "pet_id, discord_id, guild_id, name, species, adopted_at, hatched_at, "
    "adopt_fee, last_fed_at, hunger_at_last_fed, times_fed, feeds_today, "
    "feed_date, week_consumed_jc, week_key, prev_week_consumed_jc, "
    "prev_week_key, pampered_until, accessory, aegis_used, "
    "hatch_announced_at, died_at, death_cause, death_announced_at, egg_tier"
)

# Shared SET fragment: accumulate care JC into the current week, shifting the
# old accumulator into the prev-week slot on rollover so the weekly refund
# sweep can still read last week's consumption after the first new-week feed.
# Parameter order: (week_key, week_key, week_key, consumed, consumed, week_key)
_WEEK_ACCUMULATE_SQL = """
    prev_week_key = CASE
        WHEN week_key = ? OR week_key IS NULL THEN prev_week_key ELSE week_key END,
    prev_week_consumed_jc = CASE
        WHEN week_key = ? OR week_key IS NULL
        THEN prev_week_consumed_jc ELSE week_consumed_jc END,
    week_consumed_jc = CASE
        WHEN week_key = ? THEN week_consumed_jc + ? ELSE ? END,
    week_key = ?
"""


def _week_params(week_key: str, consumed: int) -> tuple:
    return (week_key, week_key, week_key, consumed, consumed, week_key)


def _row_to_pet(row: sqlite3.Row) -> Pet:
    return Pet.from_row(dict(row))


class PetRepository(BaseRepository):
    # --- reads ---

    def get_active_pet(self, discord_id: int, guild_id: int | None) -> Pet | None:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            row = conn.execute(
                f"SELECT {_PET_COLUMNS} FROM pets "
                "WHERE discord_id = ? AND guild_id = ? AND died_at IS NULL",
                (discord_id, gid),
            ).fetchone()
        return _row_to_pet(row) if row else None

    def get_pet_by_id(self, pet_id: int, guild_id: int | None) -> Pet | None:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            row = conn.execute(
                f"SELECT {_PET_COLUMNS} FROM pets WHERE pet_id = ? AND guild_id = ?",
                (pet_id, gid),
            ).fetchone()
        return _row_to_pet(row) if row else None

    def count_dead_pets(self, discord_id: int, guild_id: int | None) -> int:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            row = conn.execute(
                "SELECT COUNT(*) AS n FROM pets "
                "WHERE discord_id = ? AND guild_id = ? AND died_at IS NOT NULL",
                (discord_id, gid),
            ).fetchone()
        return int(row["n"])

    def get_latest_dead_pet(self, discord_id: int, guild_id: int | None) -> Pet | None:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            row = conn.execute(
                f"SELECT {_PET_COLUMNS} FROM pets "
                "WHERE discord_id = ? AND guild_id = ? AND died_at IS NOT NULL "
                "ORDER BY died_at DESC LIMIT 1",
                (discord_id, gid),
            ).fetchone()
        return _row_to_pet(row) if row else None

    def get_graveyard(
        self, discord_id: int, guild_id: int | None, limit: int = 10
    ) -> list[Pet]:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            rows = conn.execute(
                f"SELECT {_PET_COLUMNS} FROM pets "
                "WHERE discord_id = ? AND guild_id = ? AND died_at IS NOT NULL "
                "ORDER BY died_at DESC LIMIT ?",
                (discord_id, gid, limit),
            ).fetchall()
        return [_row_to_pet(row) for row in rows]

    def get_oldest_living(self, guild_id: int | None, limit: int = 10) -> list[Pet]:
        """Oldest living hatched pets in a guild (eggs excluded by hatch check
        in the service; ordering by hatched_at ASC is oldest-first)."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            rows = conn.execute(
                f"SELECT {_PET_COLUMNS} FROM pets "
                "WHERE guild_id = ? AND died_at IS NULL "
                "ORDER BY hatched_at ASC LIMIT ?",
                (gid, limit),
            ).fetchall()
        return [_row_to_pet(row) for row in rows]

    def get_supplies(self, discord_id: int, guild_id: int | None) -> dict[str, int]:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            rows = conn.execute(
                "SELECT item_id, qty FROM pet_supplies "
                "WHERE discord_id = ? AND guild_id = ? AND qty > 0",
                (discord_id, gid),
            ).fetchall()
        return {row["item_id"]: row["qty"] for row in rows}

    def get_active_pets_for(
        self, discord_ids: list[int], guild_id: int | None
    ) -> list[Pet]:
        if not discord_ids:
            return []
        gid = self.normalize_guild_id(guild_id)
        placeholders = ", ".join("?" for _ in discord_ids)
        with self.connection() as conn:
            rows = conn.execute(
                f"SELECT {_PET_COLUMNS} FROM pets "
                f"WHERE guild_id = ? AND died_at IS NULL "
                f"AND discord_id IN ({placeholders})",
                (gid, *discord_ids),
            ).fetchall()
        return [_row_to_pet(row) for row in rows]

    # --- purchases (jopacoin sinks; ledger source="pet") ---

    def adopt_pet(
        self,
        discord_id: int,
        guild_id: int | None,
        *,
        name: str,
        egg_tier: str,
        fee: int,
        now: int,
        hatch_seconds: int,
    ) -> Pet:
        gid = self.normalize_guild_id(guild_id)
        hatched_at = now + hatch_seconds
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            self._debit_player(
                cursor,
                discord_id,
                gid,
                fee,
                related_type="pet_adopt",
                reason=f"adopted pet {name}",
                metadata={"egg_tier": egg_tier, "fee": fee},
            )
            try:
                cursor.execute(
                    """
                    INSERT INTO pets (
                        discord_id, guild_id, name, species, egg_tier, adopted_at,
                        hatched_at, adopt_fee, last_fed_at, hunger_at_last_fed
                    )
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 100)
                    """,
                    (
                        discord_id,
                        gid,
                        name,
                        UNHATCHED_SPECIES,
                        egg_tier,
                        now,
                        hatched_at,
                        fee,
                        hatched_at,
                    ),
                )
            except sqlite3.IntegrityError as exc:
                raise ValueError("already_has_pet") from exc
            pet_id = cursor.lastrowid
            row = cursor.execute(
                f"SELECT {_PET_COLUMNS} FROM pets WHERE pet_id = ?",
                (pet_id,),
            ).fetchone()
            return _row_to_pet(row)

    def upgrade_egg(
        self,
        discord_id: int,
        guild_id: int | None,
        *,
        name: str,
        premium: int,
        now: int,
    ) -> Pet:
        """Atomically charge the gilded premium and change an unhatched pool."""
        gid = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            row = cursor.execute(
                f"SELECT {_PET_COLUMNS} FROM pets "
                "WHERE discord_id = ? AND guild_id = ? AND died_at IS NULL",
                (discord_id, gid),
            ).fetchone()
            if row is None:
                raise ValueError("no_pet")
            pet = _row_to_pet(row)
            if now >= pet.hatched_at or pet.species != UNHATCHED_SPECIES:
                raise ValueError("pet_hatched")
            if pet.egg_tier == "gilded":
                raise ValueError("already_gilded")
            self._debit_player(
                cursor,
                discord_id,
                gid,
                premium,
                related_type="pet_upgrade",
                reason=f"upgraded egg {pet.name} to gilded",
                metadata={
                    "pet_id": pet.pet_id,
                    "old_name": pet.name,
                    "new_name": name,
                    "premium": premium,
                },
            )
            cursor.execute(
                """
                UPDATE pets
                SET name = ?, egg_tier = 'gilded', adopt_fee = adopt_fee + ?
                WHERE pet_id = ? AND guild_id = ?
                """,
                (name, premium, pet.pet_id, gid),
            )
            upgraded = cursor.execute(
                f"SELECT {_PET_COLUMNS} FROM pets WHERE pet_id = ? AND guild_id = ?",
                (pet.pet_id, gid),
            ).fetchone()
            return _row_to_pet(upgraded)

    def resolve_hatch_species(
        self,
        pet_id: int,
        guild_id: int | None,
        *,
        species: str,
        now: int,
    ) -> Pet:
        """Persist the first species selected at hatch; retries return it."""
        gid = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            row = cursor.execute(
                f"SELECT {_PET_COLUMNS} FROM pets "
                "WHERE pet_id = ? AND guild_id = ? AND died_at IS NULL",
                (pet_id, gid),
            ).fetchone()
            if row is None:
                raise ValueError("no_pet")
            pet = _row_to_pet(row)
            if pet.species != UNHATCHED_SPECIES:
                return pet
            if now < pet.hatched_at:
                raise ValueError("egg_not_ready")
            cursor.execute(
                """
                UPDATE pets
                SET species = ?
                WHERE pet_id = ? AND guild_id = ? AND died_at IS NULL
                  AND species = ?
                """,
                (species, pet_id, gid, UNHATCHED_SPECIES),
            )
            resolved = cursor.execute(
                f"SELECT {_PET_COLUMNS} FROM pets WHERE pet_id = ? AND guild_id = ?",
                (pet_id, gid),
            ).fetchone()
            return _row_to_pet(resolved)

    def buy_supplies(
        self,
        discord_id: int,
        guild_id: int | None,
        *,
        item_id: str,
        qty: int,
        total_cost: int,
        now: int,
        stack_cap: int,
    ) -> int:
        """Debit + stock upsert. Returns the new quantity."""
        gid = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            row = cursor.execute(
                "SELECT qty FROM pet_supplies "
                "WHERE discord_id = ? AND guild_id = ? AND item_id = ?",
                (discord_id, gid, item_id),
            ).fetchone()
            current = row["qty"] if row else 0
            if current + qty > stack_cap:
                raise ValueError("stack_cap")
            self._debit_player(
                cursor,
                discord_id,
                gid,
                total_cost,
                related_type="pet_supplies",
                reason=f"bought {qty}x {item_id}",
                metadata={"item_id": item_id, "qty": qty, "total_cost": total_cost},
            )
            cursor.execute(
                """
                INSERT INTO pet_supplies (discord_id, guild_id, item_id, qty, updated_at)
                VALUES (?, ?, ?, ?, ?)
                ON CONFLICT(discord_id, guild_id, item_id) DO UPDATE SET
                    qty = qty + excluded.qty,
                    updated_at = excluded.updated_at
                """,
                (discord_id, gid, item_id, qty, now),
            )
            return current + qty

    def rename_pet(
        self,
        pet_id: int,
        discord_id: int,
        guild_id: int | None,
        *,
        new_name: str,
        cost: int,
    ) -> None:
        gid = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            row = cursor.execute(
                "SELECT died_at FROM pets WHERE pet_id = ? AND guild_id = ?",
                (pet_id, gid),
            ).fetchone()
            if row is None:
                raise ValueError("no_pet")
            if row["died_at"] is not None:
                raise ValueError("pet_dead")
            self._debit_player(
                cursor,
                discord_id,
                gid,
                cost,
                related_type="pet_rename",
                reason=f"renamed pet to {new_name}",
                metadata={"pet_id": pet_id},
            )
            cursor.execute(
                "UPDATE pets SET name = ? WHERE pet_id = ? AND guild_id = ?",
                (new_name, pet_id, gid),
            )

    def use_salt_lick(
        self,
        pet_id: int,
        discord_id: int,
        guild_id: int | None,
        *,
        cost: int,
        now: int,
        pampered_until: int,
        week_key: str,
    ) -> Pet:
        gid = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            row = cursor.execute(
                "SELECT died_at, pampered_until FROM pets "
                "WHERE pet_id = ? AND guild_id = ?",
                (pet_id, gid),
            ).fetchone()
            if row is None:
                raise ValueError("no_pet")
            if row["died_at"] is not None:
                raise ValueError("pet_dead")
            if row["pampered_until"] is not None and row["pampered_until"] > now:
                raise ValueError("already_pampered")
            self._debit_player(
                cursor,
                discord_id,
                gid,
                cost,
                related_type="pet_salt_lick",
                reason="salt lick",
                metadata={"pet_id": pet_id},
            )
            cursor.execute(
                f"""
                UPDATE pets SET
                    pampered_until = ?,
                    {_WEEK_ACCUMULATE_SQL}
                WHERE pet_id = ? AND guild_id = ?
                """,
                (pampered_until, *_week_params(week_key, cost), pet_id, gid),
            )
            fresh = cursor.execute(
                f"SELECT {_PET_COLUMNS} FROM pets WHERE pet_id = ?",
                (pet_id,),
            ).fetchone()
            return _row_to_pet(fresh)

    # --- trinkets (accessory gacha) ---

    def buy_trinket_atomic(
        self,
        pet_id: int,
        discord_id: int,
        guild_id: int | None,
        *,
        accessory_id: str,
        cost: int,
        refund: int,
        now: int,
    ) -> dict:
        """Debit for a trinket roll; duplicates cost (cost - refund) net and
        grant nothing. A new accessory is recorded and auto-equipped on the
        living pet. Returns {duplicate, net_cost, owned_count}.

        Like every sibling economy op, the pet's liveness is re-verified
        INSIDE the transaction so a death claimed after the caller's check
        can never be charged.
        """
        gid = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            row = cursor.execute(
                "SELECT died_at FROM pets WHERE pet_id = ? AND guild_id = ?",
                (pet_id, gid),
            ).fetchone()
            if row is None:
                raise ValueError("no_pet")
            if row["died_at"] is not None:
                raise ValueError("pet_dead")
            owned = cursor.execute(
                "SELECT 1 FROM pet_accessories "
                "WHERE discord_id = ? AND guild_id = ? AND accessory_id = ?",
                (discord_id, gid, accessory_id),
            ).fetchone()
            duplicate = owned is not None
            net_cost = max(0, cost - refund) if duplicate else cost
            self._debit_player(
                cursor,
                discord_id,
                gid,
                net_cost,
                related_type="pet_trinket",
                reason=f"mystery trinket roll: {accessory_id}",
                metadata={
                    "accessory_id": accessory_id,
                    "duplicate": duplicate,
                    "net_cost": net_cost,
                },
            )
            if not duplicate:
                cursor.execute(
                    "INSERT INTO pet_accessories "
                    "(discord_id, guild_id, accessory_id, obtained_at) "
                    "VALUES (?, ?, ?, ?)",
                    (discord_id, gid, accessory_id, now),
                )
                cursor.execute(
                    "UPDATE pets SET accessory = ? "
                    "WHERE pet_id = ? AND guild_id = ? AND died_at IS NULL",
                    (accessory_id, pet_id, gid),
                )
            count = cursor.execute(
                "SELECT COUNT(*) AS n FROM pet_accessories "
                "WHERE discord_id = ? AND guild_id = ?",
                (discord_id, gid),
            ).fetchone()
            return {
                "duplicate": duplicate,
                "net_cost": net_cost,
                "owned_count": int(count["n"]),
            }

    def get_accessories(self, discord_id: int, guild_id: int | None) -> list[str]:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            rows = conn.execute(
                "SELECT accessory_id FROM pet_accessories "
                "WHERE discord_id = ? AND guild_id = ? ORDER BY obtained_at",
                (discord_id, gid),
            ).fetchall()
        return [row["accessory_id"] for row in rows]

    def equip_accessory(
        self,
        pet_id: int,
        discord_id: int,
        guild_id: int | None,
        accessory_id: str | None,
    ) -> None:
        """Equip an OWNED accessory (or None to unequip) on the living pet.

        Raises distinct codes so callers can report the real cause:
        no_pet / pet_dead / not_owned.
        """
        gid = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            row = cursor.execute(
                "SELECT died_at FROM pets WHERE pet_id = ? AND guild_id = ?",
                (pet_id, gid),
            ).fetchone()
            if row is None:
                raise ValueError("no_pet")
            if row["died_at"] is not None:
                raise ValueError("pet_dead")
            if accessory_id is not None:
                owned = cursor.execute(
                    "SELECT 1 FROM pet_accessories "
                    "WHERE discord_id = ? AND guild_id = ? AND accessory_id = ?",
                    (discord_id, gid, accessory_id),
                ).fetchone()
                if owned is None:
                    raise ValueError("not_owned")
            cursor.execute(
                "UPDATE pets SET accessory = ? WHERE pet_id = ? AND guild_id = ?",
                (accessory_id, pet_id, gid),
            )

    # --- collection log + pity ---

    def species_raised(
        self, discord_id: int, guild_id: int | None, now: int
    ) -> list[str]:
        """Distinct species this player has HATCHED (eggs stay a mystery)."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            rows = conn.execute(
                "SELECT DISTINCT species FROM pets "
                "WHERE discord_id = ? AND guild_id = ? AND hatched_at <= ?",
                (discord_id, gid, now),
            ).fetchall()
        return [row["species"] for row in rows]

    def recent_dead_species(
        self, discord_id: int, guild_id: int | None, limit: int
    ) -> list[str]:
        """Species of the most recent dead pets, newest first (pity input)."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            rows = conn.execute(
                "SELECT species FROM pets "
                "WHERE discord_id = ? AND guild_id = ? AND died_at IS NOT NULL "
                "ORDER BY adopted_at DESC LIMIT ?",
                (discord_id, gid, limit),
            ).fetchall()
        return [row["species"] for row in rows]

    # --- feeding (no balance change: consumes stocked supplies) ---

    def feed_pet(
        self,
        pet_id: int,
        discord_id: int,
        guild_id: int | None,
        *,
        item_id: str,
        expected_last_fed_at: int,
        expected_hunger: int,
        new_last_fed_at: int,
        new_hunger: int,
        feed_date: str,
        feed_cap: int,
        week_key: str,
        consumed_jc: int,
        spat: bool,
    ) -> Pet:
        """Consume one item and move the hunger anchor, all-or-nothing.

        A spat feed (Rama) consumes the item and a feed-cap action but leaves
        the anchors untouched. The anchor guard is optimistic concurrency:
        a concurrent feed changes the anchors and this call raises stale_pet.
        """
        gid = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            row = cursor.execute(
                "SELECT died_at, last_fed_at, hunger_at_last_fed, feeds_today, "
                "feed_date FROM pets WHERE pet_id = ? AND guild_id = ?",
                (pet_id, gid),
            ).fetchone()
            if row is None:
                raise ValueError("no_pet")
            if row["died_at"] is not None:
                raise ValueError("pet_dead")
            if (
                row["last_fed_at"] != expected_last_fed_at
                or row["hunger_at_last_fed"] != expected_hunger
            ):
                raise ValueError("stale_pet")
            feeds_today = row["feeds_today"] if row["feed_date"] == feed_date else 0
            if feeds_today >= feed_cap:
                raise ValueError("feed_cap")
            cursor.execute(
                "UPDATE pet_supplies SET qty = qty - 1, updated_at = ? "
                "WHERE discord_id = ? AND guild_id = ? AND item_id = ? AND qty >= 1",
                (new_last_fed_at, discord_id, gid, item_id),
            )
            if cursor.rowcount == 0:
                raise ValueError("no_supplies")
            cursor.execute(
                f"""
                UPDATE pets SET
                    last_fed_at = CASE WHEN ? THEN last_fed_at ELSE ? END,
                    hunger_at_last_fed = CASE
                        WHEN ? THEN hunger_at_last_fed ELSE ? END,
                    times_fed = times_fed + (CASE WHEN ? THEN 0 ELSE 1 END),
                    feeds_today = ?,
                    feed_date = ?,
                    {_WEEK_ACCUMULATE_SQL}
                WHERE pet_id = ? AND guild_id = ?
                """,
                (
                    spat, new_last_fed_at,
                    spat, new_hunger,
                    spat,
                    feeds_today + 1,
                    feed_date,
                    *_week_params(week_key, consumed_jc),
                    pet_id, gid,
                ),
            )
            fresh = cursor.execute(
                f"SELECT {_PET_COLUMNS} FROM pets WHERE pet_id = ?",
                (pet_id,),
            ).fetchone()
            return _row_to_pet(fresh)

    def apply_hunger_top_up(
        self,
        pet_id: int,
        guild_id: int | None,
        *,
        expected_last_fed_at: int,
        expected_hunger: int,
        new_last_fed_at: int,
        new_hunger: int,
    ) -> bool:
        """Match-win top-up: anchor move with optimistic guard, no supplies.

        Returns False (skipped) on a concurrent change instead of raising —
        a lost race just means the pet was interacted with this instant.
        """
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.execute(
                """
                UPDATE pets SET last_fed_at = ?, hunger_at_last_fed = ?
                WHERE pet_id = ? AND guild_id = ? AND died_at IS NULL
                  AND last_fed_at = ? AND hunger_at_last_fed = ?
                """,
                (
                    new_last_fed_at, new_hunger,
                    pet_id, gid,
                    expected_last_fed_at, expected_hunger,
                ),
            )
            return cursor.rowcount == 1

    # --- death, revival, announcements ---

    def claim_death(
        self,
        pet_id: int,
        guild_id: int | None,
        *,
        died_at: int,
        cause: str = "starvation",
        expected_last_fed_at: int,
        expected_hunger: int,
    ) -> bool:
        """Once-only death claim: the conditional UPDATE is the claim.

        The anchor guard makes the claim optimistic: a feed that committed
        after the caller's snapshot changes the anchors and the claim loses,
        so a freshly fed pet can never be killed from stale starvation math.
        """
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.execute(
                "UPDATE pets SET died_at = ?, death_cause = ? "
                "WHERE pet_id = ? AND guild_id = ? AND died_at IS NULL "
                "AND last_fed_at = ? AND hunger_at_last_fed = ?",
                (died_at, cause, pet_id, gid, expected_last_fed_at, expected_hunger),
            )
            return cursor.rowcount == 1

    def revive_with_aegis(
        self,
        pet_id: int,
        guild_id: int | None,
        *,
        revived_last_fed_at: int,
        revive_hunger: int,
        expected_last_fed_at: int,
        expected_hunger: int,
    ) -> bool:
        """Shellback's one-time save: burn the Aegis instead of dying.

        Anchor-guarded like claim_death so a concurrent feed can't have its
        anchors overwritten (and the once-per-lifetime Aegis wasted) by a
        stale starvation snapshot.
        """
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.execute(
                """
                UPDATE pets SET aegis_used = 1, last_fed_at = ?, hunger_at_last_fed = ?
                WHERE pet_id = ? AND guild_id = ? AND died_at IS NULL AND aegis_used = 0
                  AND last_fed_at = ? AND hunger_at_last_fed = ?
                """,
                (
                    revived_last_fed_at, revive_hunger,
                    pet_id, gid,
                    expected_last_fed_at, expected_hunger,
                ),
            )
            return cursor.rowcount == 1

    def find_starvation_candidates(
        self, now: int, *, decay_per_day: int, max_decay_pct: int
    ) -> list[Pet]:
        """Global superset scan: everything that COULD have starved by now at
        the fastest species decay. The service re-checks each candidate with
        exact per-species math."""
        seconds_per_100_points = 86400 * 100 * 100 // max(1, decay_per_day * max_decay_pct)
        with self.connection() as conn:
            rows = conn.execute(
                f"SELECT {_PET_COLUMNS} FROM pets "
                "WHERE died_at IS NULL "
                "AND last_fed_at + (hunger_at_last_fed * ? / 100) <= ?",
                (seconds_per_100_points, now),
            ).fetchall()
        return [_row_to_pet(row) for row in rows]

    def find_unannounced_hatches(self, now: int) -> list[Pet]:
        with self.connection() as conn:
            rows = conn.execute(
                f"SELECT {_PET_COLUMNS} FROM pets "
                "WHERE died_at IS NULL AND hatch_announced_at IS NULL "
                "AND hatched_at <= ?",
                (now,),
            ).fetchall()
        return [_row_to_pet(row) for row in rows]

    def mark_hatch_announced(
        self, pet_id: int, guild_id: int | None, announced_at: int
    ) -> None:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            conn.execute(
                "UPDATE pets SET hatch_announced_at = ? "
                "WHERE pet_id = ? AND guild_id = ?",
                (announced_at, pet_id, gid),
            )

    def get_unannounced_deaths(self) -> list[Pet]:
        with self.connection() as conn:
            rows = conn.execute(
                f"SELECT {_PET_COLUMNS} FROM pets "
                "WHERE died_at IS NOT NULL AND death_announced_at IS NULL",
            ).fetchall()
        return [_row_to_pet(row) for row in rows]

    def mark_death_announced(
        self, pet_id: int, guild_id: int | None, announced_at: int
    ) -> None:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            conn.execute(
                "UPDATE pets SET death_announced_at = ? "
                "WHERE pet_id = ? AND guild_id = ? AND died_at IS NOT NULL",
                (announced_at, pet_id, gid),
            )

    # --- weekly refunds (nonprofit-funded; ledger source="pet_refund") ---

    def list_pet_guild_ids(self) -> list[int]:
        with self.connection() as conn:
            rows = conn.execute(
                "SELECT DISTINCT guild_id FROM pets WHERE died_at IS NULL"
            ).fetchall()
        return [row["guild_id"] for row in rows]

    def list_week_care(self, guild_id: int | None, week_key: str) -> list[Pet]:
        """Living pets whose owners consumed care JC in the given week.

        Checks both accumulator slots: pets fed since the week rolled over
        carry last week's care in the prev-week slot.
        """
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            rows = conn.execute(
                f"SELECT {_PET_COLUMNS} FROM pets "
                "WHERE guild_id = ? AND died_at IS NULL AND ("
                "  (week_key = ? AND week_consumed_jc > 0) OR "
                "  (prev_week_key = ? AND prev_week_consumed_jc > 0)"
                ")",
                (gid, week_key, week_key),
            ).fetchall()
        return [_row_to_pet(row) for row in rows]

    def is_refund_window_paid(self, guild_id: int | None, week_key: str) -> bool:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            row = conn.execute(
                "SELECT 1 FROM pet_refund_windows WHERE guild_id = ? AND week_key = ?",
                (gid, week_key),
            ).fetchone()
        return row is not None

    def get_nonprofit_balance(self, guild_id: int | None) -> int:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            row = conn.execute(
                "SELECT total_collected FROM nonprofit_fund WHERE guild_id = ?",
                (gid,),
            ).fetchone()
        return int(row["total_collected"]) if row and row["total_collected"] else 0

    def pay_refunds_atomic(
        self,
        guild_id: int | None,
        week_key: str,
        payouts: list[tuple[int, int]],
        now: int,
        *,
        announcement: RefundNotice | None = None,
    ) -> bool:
        """Claim the (guild, week) window and pay refunds from the nonprofit.

        Returns False if the window was already claimed. Raises fund_short if
        the nonprofit balance dropped below the pre-scaled total mid-flight —
        the whole transaction (claim included) rolls back and the next sweep
        recomputes against the fresh balance. When supplied, ``announcement``
        is persisted in the same transaction for at-least-once delivery.
        """
        gid = self.normalize_guild_id(guild_id)
        total = sum(amount for _, amount in payouts)
        announcement_payload = (
            json.dumps(
                {
                    "payouts": [
                        dataclasses.asdict(payout) for payout in announcement.payouts
                    ],
                    "scaled_down": announcement.scaled_down,
                },
                sort_keys=True,
            )
            if announcement is not None
            else None
        )
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "INSERT OR IGNORE INTO pet_refund_windows "
                "(guild_id, week_key, paid_at, total_paid, announcement_payload) "
                "VALUES (?, ?, ?, ?, ?)",
                (gid, week_key, now, total, announcement_payload),
            )
            if cursor.rowcount == 0:
                return False
            if total <= 0:
                return True
            self._set_economy_ledger_context(
                cursor,
                source="pet_refund",
                related_type="pet_refund",
                related_id=week_key,
                reason=f"weekly pet care refund {week_key}",
                metadata={"total": total, "recipients": len(payouts)},
            )
            try:
                cursor.execute(
                    """
                    UPDATE nonprofit_fund
                    SET total_collected = total_collected - ?,
                        updated_at = CURRENT_TIMESTAMP
                    WHERE guild_id = ? AND total_collected >= ?
                    """,
                    (total, gid, total),
                )
                if cursor.rowcount == 0:
                    raise ValueError("fund_short")
                unpaid = 0
                for discord_id, amount in payouts:
                    if amount <= 0:
                        continue
                    cursor.execute(
                        """
                        UPDATE players
                        SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?,
                            updated_at = CURRENT_TIMESTAMP
                        WHERE discord_id = ? AND guild_id = ?
                        """,
                        (amount, discord_id, gid),
                    )
                    if cursor.rowcount == 0:
                        # Missing player row (e.g. purged account): return the
                        # share to the fund rather than destroying the coins.
                        unpaid += amount
                if unpaid:
                    cursor.execute(
                        """
                        UPDATE nonprofit_fund
                        SET total_collected = total_collected + ?,
                            updated_at = CURRENT_TIMESTAMP
                        WHERE guild_id = ?
                        """,
                        (unpaid, gid),
                    )
                    cursor.execute(
                        "UPDATE pet_refund_windows SET total_paid = total_paid - ? "
                        "WHERE guild_id = ? AND week_key = ?",
                        (unpaid, gid, week_key),
                    )
            finally:
                self._clear_economy_ledger_context(cursor)
            return True

    def get_unannounced_refunds(self) -> list[RefundNotice]:
        with self.connection() as conn:
            rows = conn.execute(
                """
                SELECT guild_id, week_key, total_paid, announcement_payload
                FROM pet_refund_windows
                WHERE announcement_payload IS NOT NULL AND announced_at IS NULL
                ORDER BY paid_at, guild_id, week_key
                """
            ).fetchall()
        notices = []
        for row in rows:
            payload = safe_json_loads(
                row["announcement_payload"],
                {},
                context=(
                    "pet_refund_windows "
                    f"guild={row['guild_id']} week={row['week_key']}"
                ),
            )
            notices.append(
                RefundNotice(
                    guild_id=int(row["guild_id"]),
                    week_key=row["week_key"],
                    payouts=tuple(
                        RefundPayout(**payout)
                        for payout in payload.get("payouts", [])
                    ),
                    total_paid=int(row["total_paid"]),
                    scaled_down=bool(payload.get("scaled_down")),
                )
            )
        return notices

    def mark_refund_announced(
        self,
        guild_id: int | None,
        week_key: str,
        announced_at: int,
    ) -> None:
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            conn.execute(
                """
                UPDATE pet_refund_windows
                SET announced_at = ?
                WHERE guild_id = ? AND week_key = ? AND announced_at IS NULL
                """,
                (announced_at, gid, week_key),
            )

    # --- internals ---

    def _debit_player(
        self,
        cursor: sqlite3.Cursor,
        discord_id: int,
        gid: int,
        amount: int,
        *,
        related_type: str,
        reason: str,
        metadata: dict,
    ) -> None:
        """Conditional debit inside the caller's transaction (no overdraft)."""
        if amount <= 0:
            return
        self._set_economy_ledger_context(
            cursor,
            source="pet",
            actor_id=discord_id,
            related_type=related_type,
            related_id=discord_id,
            reason=reason,
            metadata=metadata,
        )
        try:
            cursor.execute(
                """
                UPDATE players
                SET jopacoin_balance = jopacoin_balance - ?,
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                  AND COALESCE(jopacoin_balance, 0) >= ?
                """,
                (amount, discord_id, gid, amount),
            )
            debited = cursor.rowcount
        finally:
            self._clear_economy_ledger_context(cursor)
        if debited == 0:
            raise ValueError("insufficient_funds")
