"""Tests for the dig_gear repository methods (CRUD + atomic equip + durability)."""

import sqlite3

import pytest

from repositories.dig_repository import DigRepository


@pytest.fixture
def gear_repo(repo_db_path):
    return DigRepository(repo_db_path)


class TestGearCrud:
    def test_add_then_get(self, gear_repo):
        gid = gear_repo.add_gear(111, 0, "armor", 2, source="shop")
        all_owned = gear_repo.get_gear(111, 0)
        assert len(all_owned) == 1
        assert all_owned[0]["id"] == gid
        assert all_owned[0]["slot"] == "armor"
        assert all_owned[0]["tier"] == 2
        assert all_owned[0]["equipped"] == 0
        assert all_owned[0]["source"] == "shop"

    def test_add_uses_max_durability_by_default(self, gear_repo):
        gid = gear_repo.add_gear(111, 0, "boots", 1)
        row = gear_repo.get_gear_by_id(gid)
        assert row["durability"] == 20  # GEAR_MAX_DURABILITY

    def test_add_respects_explicit_durability(self, gear_repo):
        gid = gear_repo.add_gear(111, 0, "boots", 1, durability=7)
        row = gear_repo.get_gear_by_id(gid)
        assert row["durability"] == 7

    def test_get_gear_orders_by_slot_then_tier_desc(self, gear_repo):
        gear_repo.add_gear(111, 0, "armor", 1)
        gear_repo.add_gear(111, 0, "armor", 3)
        gear_repo.add_gear(111, 0, "boots", 2)
        rows = gear_repo.get_gear(111, 0)
        # armor sorts before boots; within armor, tier 3 before tier 1
        slots = [r["slot"] for r in rows]
        tiers = [r["tier"] for r in rows]
        assert slots == ["armor", "armor", "boots"]
        assert tiers == [3, 1, 2]

    def test_get_gear_isolated_by_player_and_guild(self, gear_repo):
        gear_repo.add_gear(111, 0, "armor", 1)
        gear_repo.add_gear(222, 0, "armor", 1)
        gear_repo.add_gear(111, 999, "armor", 1)
        assert len(gear_repo.get_gear(111, 0)) == 1
        assert len(gear_repo.get_gear(222, 0)) == 1
        assert len(gear_repo.get_gear(111, 999)) == 1


class TestEquipUnequip:
    def test_equip_marks_one_piece_equipped(self, gear_repo):
        gid = gear_repo.add_gear(111, 0, "armor", 1)
        gear_repo.equip_gear(gid, 111, 0, "armor")
        equipped = gear_repo.get_equipped_gear(111, 0)
        assert "armor" in equipped
        assert equipped["armor"]["id"] == gid

    def test_equipping_second_piece_swaps_first_atomically(self, gear_repo):
        a = gear_repo.add_gear(111, 0, "armor", 1)
        b = gear_repo.add_gear(111, 0, "armor", 3)
        gear_repo.equip_gear(a, 111, 0, "armor")
        gear_repo.equip_gear(b, 111, 0, "armor")
        equipped = gear_repo.get_equipped_gear(111, 0)
        assert equipped["armor"]["id"] == b
        # First piece is now unequipped (no longer in dict but still owned)
        all_armor = [r for r in gear_repo.get_gear(111, 0) if r["slot"] == "armor"]
        assert len(all_armor) == 2
        equipped_ids = [r["id"] for r in all_armor if r["equipped"]]
        assert equipped_ids == [b]

    def test_partial_unique_index_blocks_manual_double_equip(self, repo_db_path):
        """The partial unique index must reject two equipped rows in one slot.

        Bypasses the service so we exercise the DB-level constraint directly.
        """
        gear_repo = DigRepository(repo_db_path)
        a = gear_repo.add_gear(111, 0, "boots", 1)
        b = gear_repo.add_gear(111, 0, "boots", 2)
        gear_repo.equip_gear(a, 111, 0, "boots")
        # Direct write of equipped=1 on the second row must violate the constraint
        with sqlite3.connect(repo_db_path) as conn:
            conn.row_factory = sqlite3.Row
            with pytest.raises(sqlite3.IntegrityError):
                conn.execute("UPDATE dig_gear SET equipped = 1 WHERE id = ?", (b,))
                conn.commit()

    def test_unequip_clears_equipped_flag(self, gear_repo):
        gid = gear_repo.add_gear(111, 0, "armor", 1)
        gear_repo.equip_gear(gid, 111, 0, "armor")
        gear_repo.unequip_gear(gid)
        equipped = gear_repo.get_equipped_gear(111, 0)
        assert equipped == {}

    def test_get_equipped_gear_keys_by_slot(self, gear_repo):
        a = gear_repo.add_gear(111, 0, "armor", 1)
        b = gear_repo.add_gear(111, 0, "boots", 2)
        c = gear_repo.add_gear(111, 0, "weapon", 3)
        gear_repo.equip_gear(a, 111, 0, "armor")
        gear_repo.equip_gear(b, 111, 0, "boots")
        gear_repo.equip_gear(c, 111, 0, "weapon")
        equipped = gear_repo.get_equipped_gear(111, 0)
        assert set(equipped.keys()) == {"armor", "boots", "weapon"}


class TestDurabilityTick:
    def test_tick_decrements_only_equipped(self, gear_repo):
        a = gear_repo.add_gear(111, 0, "armor", 1)
        gear_repo.add_gear(111, 0, "armor", 3)  # owned but unequipped
        gear_repo.equip_gear(a, 111, 0, "armor")

        broken = gear_repo.tick_gear_durability(111, 0)
        assert broken == []  # not zero yet

        rows = {r["id"]: r for r in gear_repo.get_gear(111, 0)}
        assert rows[a]["durability"] == 19  # equipped -> ticked
        # Unequipped piece untouched
        unequipped = [r for r in rows.values() if r["id"] != a]
        assert all(r["durability"] == 20 for r in unequipped)

    def test_tick_to_zero_returns_broken_ids_and_unequips(self, gear_repo):
        a = gear_repo.add_gear(111, 0, "boots", 2, durability=1)
        gear_repo.equip_gear(a, 111, 0, "boots")
        broken = gear_repo.tick_gear_durability(111, 0)
        assert broken == [a]
        # Auto-unequipped at zero
        equipped = gear_repo.get_equipped_gear(111, 0)
        assert equipped == {}
        # Durability now 0
        row = gear_repo.get_gear_by_id(a)
        assert row["durability"] == 0
        assert row["equipped"] == 0

    def test_tick_floors_at_zero(self, gear_repo):
        """A second tick on a zero-durability piece must not go negative."""
        a = gear_repo.add_gear(111, 0, "boots", 2, durability=1)
        gear_repo.equip_gear(a, 111, 0, "boots")
        gear_repo.tick_gear_durability(111, 0)  # hits 0
        # Re-equip via direct update bypassing service so we can re-tick
        with sqlite3.connect(gear_repo.db_path) as conn:
            conn.execute("UPDATE dig_gear SET durability = 0, equipped = 1 WHERE id = ?", (a,))
            conn.commit()
        gear_repo.tick_gear_durability(111, 0)
        row = gear_repo.get_gear_by_id(a)
        assert row["durability"] == 0  # didn't go negative


class TestRepair:
    def test_repair_resets_durability(self, gear_repo):
        a = gear_repo.add_gear(111, 0, "armor", 1, durability=3)
        gear_repo.repair_gear(a, 20)
        assert gear_repo.get_gear_by_id(a)["durability"] == 20

    def test_repair_does_not_auto_equip(self, gear_repo):
        a = gear_repo.add_gear(111, 0, "armor", 1, durability=0)
        gear_repo.repair_gear(a, 20)
        # equipped flag still 0 — caller must equip again explicitly
        assert gear_repo.get_gear_by_id(a)["equipped"] == 0
