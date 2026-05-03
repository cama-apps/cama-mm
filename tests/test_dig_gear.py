"""Consolidated tests for the dig gear stack: drop logic, repository CRUD, and
service orchestration.

The drop rolls are RNG-driven; we use ``random.seed`` for determinism so the
hit-rate test isn't flaky. The seed is restored automatically by the
``_isolate_random_state`` autouse fixture in conftest.
"""

import random
import sqlite3

import pytest

from domain.models.dig_gear import GearLoadout, GearPiece, GearSlot
from repositories.dig_repository import DigRepository
from repositories.player_repository import PlayerRepository
from services.dig_constants import (
    ARMOR_TIERS,
    BOOTS_TIERS,
    BOSS_DUEL_STATS,
    GEAR_BOSS_DROP_RATE,
    GEAR_DROP_DEPTH_TIER_MAP,
    GEAR_MAX_DURABILITY,
    GEAR_TIER_TABLES,
    PLAYER_HIT_CEILING,
    WEAPON_TIERS,
    format_relic_label,
)
from services.dig_service import DigService

# =============================================================================
# Shared fixtures
# =============================================================================


@pytest.fixture
def gear_repo(repo_db_path):
    """Bare DigRepository for repository-layer tests."""
    return DigRepository(repo_db_path)


@pytest.fixture
def svc(repo_db_path):
    """Canonical DigService with a registered player, funded balance, and an
    advanced tunnel. This superset setup serves both drop-logic and
    service-orchestration tests; drop tests simply don't read the balance or
    depth fields.
    """
    drepo = DigRepository(repo_db_path)
    prepo = PlayerRepository(repo_db_path)
    s = DigService(drepo, prepo)
    s.player_repo.add(discord_id=111, discord_username="pf", guild_id=0)
    s.player_repo.add_balance(111, 0, 5000)
    s.dig_repo.create_tunnel(111, 0, "Test Tunnel")
    s.dig_repo.update_tunnel(111, 0, depth=100, prestige_level=1)
    return s


@pytest.fixture
def player(svc):
    """Discord id of the canonical test player set up by ``svc``."""
    return 111


# =============================================================================
# === Drop logic ===
# =============================================================================


class TestDigGearDropGate:
    def test_returns_none_for_unmapped_boundary(self, svc):
        """Bosses outside GEAR_DROP_DEPTH_TIER_MAP never drop gear."""
        random.seed(0)
        # Try 100 rolls at boundary 25 (not in the map) — should never drop
        for _ in range(100):
            assert svc._maybe_drop_gear(111, 0, 25) is None

    def test_returns_none_when_roll_misses(self, svc, monkeypatch):
        """If random.random() returns >= GEAR_BOSS_DROP_RATE, no drop."""
        monkeypatch.setattr(random, "random", lambda: GEAR_BOSS_DROP_RATE + 0.01)
        assert svc._maybe_drop_gear(111, 0, 100) is None


class TestDigGearDropTierMatchesDepth:
    def test_each_mapped_boundary_drops_correct_tier(self, svc, monkeypatch):
        """Force a hit and confirm the dropped tier matches the boundary map."""
        # Pin random.random to 0 so the gate always passes; let random.choice
        # use real RNG for slot picking.
        monkeypatch.setattr(random, "random", lambda: 0.0)
        for boundary, expected_tier in GEAR_DROP_DEPTH_TIER_MAP.items():
            drop = svc._maybe_drop_gear(111, 0, boundary)
            assert drop is not None
            assert drop["tier"] == expected_tier
            assert drop["slot"] in {"weapon", "armor", "boots"}
            # Resolve the slot/tier through the GEAR_TIER_TABLES map and
            # confirm the returned name matches the canonical entry.
            slot_enum = GearSlot(drop["slot"])
            expected_name = GEAR_TIER_TABLES[slot_enum][expected_tier].name
            assert drop["name"] == expected_name


class TestDigGearDropPersists:
    def test_drop_creates_dig_gear_row(self, svc, monkeypatch):
        monkeypatch.setattr(random, "random", lambda: 0.0)
        monkeypatch.setattr(random, "choice", lambda choices: "armor")
        drop = svc._maybe_drop_gear(111, 0, 100)
        assert drop is not None
        owned = svc.dig_repo.get_gear(111, 0)
        assert any(
            g["id"] == drop["gear_id"]
            and g["slot"] == "armor"
            and g["source"] == "boss_drop"
            for g in owned
        )


class TestDigGearDropRate:
    """Statistical sanity check on the drop rate via seeded RNG."""

    def test_rate_in_band_over_5000_rolls(self, svc):
        """Over 5000 seeded rolls the empirical rate should sit in
        [GEAR_BOSS_DROP_RATE - 0.02, GEAR_BOSS_DROP_RATE + 0.02].

        With p=0.07 and n=5000 the binomial std-dev is ~0.0036 — a 2pp band
        is well over 5σ in either direction so this is robust to seed drift.
        """
        random.seed(42)
        hits = 0
        n = 5000
        for _ in range(n):
            if svc._maybe_drop_gear(111, 0, 100) is not None:
                hits += 1
        rate = hits / n
        assert abs(rate - GEAR_BOSS_DROP_RATE) < 0.02, (
            f"observed rate {rate:.4f} drifted from target {GEAR_BOSS_DROP_RATE:.4f}"
        )


# =============================================================================
# === Repository CRUD ===
# =============================================================================


class TestDigGearRepositoryCrud:
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


class TestDigGearRepositoryEquipUnequip:
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


class TestDigGearRepositoryDurabilityTick:
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


class TestDigGearRepositoryRepair:
    def test_repair_resets_durability(self, gear_repo):
        a = gear_repo.add_gear(111, 0, "armor", 1, durability=3)
        gear_repo.repair_gear(a, 20)
        assert gear_repo.get_gear_by_id(a)["durability"] == 20

    def test_repair_does_not_auto_equip(self, gear_repo):
        a = gear_repo.add_gear(111, 0, "armor", 1, durability=0)
        gear_repo.repair_gear(a, 20)
        # equipped flag still 0 — caller must equip again explicitly
        assert gear_repo.get_gear_by_id(a)["equipped"] == 0


class TestDigGearRepositoryTickById:
    def test_ticks_specific_ids_only(self, gear_repo):
        a = gear_repo.add_gear(111, 0, "armor", 1)   # equipped soon
        b = gear_repo.add_gear(111, 0, "boots", 2)   # equipped soon
        c = gear_repo.add_gear(111, 0, "weapon", 3)  # NOT in snapshot
        gear_repo.equip_gear(a, 111, 0, "armor")
        gear_repo.equip_gear(b, 111, 0, "boots")
        gear_repo.equip_gear(c, 111, 0, "weapon")
        broken = gear_repo.tick_gear_durability_ids([a, b])
        assert broken == []
        rows = {r["id"]: r for r in gear_repo.get_gear(111, 0)}
        assert rows[a]["durability"] == 19
        assert rows[b]["durability"] == 19
        assert rows[c]["durability"] == 20  # untouched

    def test_ticks_unequipped_pieces_too(self, gear_repo):
        """Snapshot ticks must work on pieces that have been unequipped."""
        a = gear_repo.add_gear(111, 0, "armor", 1)
        gear_repo.equip_gear(a, 111, 0, "armor")
        gear_repo.unequip_gear(a)  # Player swapped during pause
        gear_repo.tick_gear_durability_ids([a])
        assert gear_repo.get_gear_by_id(a)["durability"] == 19

    def test_breaks_at_zero_and_returns_id(self, gear_repo):
        a = gear_repo.add_gear(111, 0, "armor", 1, durability=1)
        gear_repo.equip_gear(a, 111, 0, "armor")
        broken = gear_repo.tick_gear_durability_ids([a])
        assert broken == [a]
        row = gear_repo.get_gear_by_id(a)
        assert row["durability"] == 0
        assert row["equipped"] == 0  # auto-unequipped

    def test_empty_list_is_a_noop(self, gear_repo):
        a = gear_repo.add_gear(111, 0, "armor", 1)
        gear_repo.equip_gear(a, 111, 0, "armor")
        broken = gear_repo.tick_gear_durability_ids([])
        assert broken == []
        assert gear_repo.get_gear_by_id(a)["durability"] == 20


# =============================================================================
# === Service orchestration ===
# =============================================================================


class TestDigGearServiceEquipUnequip:
    def test_equip_owned_armor(self, svc, player):
        r = svc.buy_gear(player, 0, "armor", 1)
        assert r["success"], r
        gid = r["gear_id"]
        eq = svc.equip_gear(player, 0, gid)
        assert eq["success"]
        assert eq["slot"] == "armor"

    def test_equip_rejects_broken(self, svc, player):
        # Manually add a broken piece
        gid = svc.dig_repo.add_gear(player, 0, "boots", 2, durability=0)
        r = svc.equip_gear(player, 0, gid)
        assert not r["success"]
        assert "broken" in r["error"].lower()

    def test_equip_rejects_someone_elses_gear(self, svc, player):
        gid = svc.dig_repo.add_gear(222, 0, "armor", 1)
        r = svc.equip_gear(player, 0, gid)
        assert not r["success"]
        assert "doesn't belong" in r["error"]

    def test_equip_already_equipped_is_a_no_op_error(self, svc, player):
        r = svc.buy_gear(player, 0, "armor", 1)
        gid = r["gear_id"]
        svc.equip_gear(player, 0, gid)
        again = svc.equip_gear(player, 0, gid)
        assert not again["success"]
        assert "already equipped" in again["error"]

    def test_unequip_works_for_owner(self, svc, player):
        r = svc.buy_gear(player, 0, "armor", 1)
        svc.equip_gear(player, 0, r["gear_id"])
        un = svc.unequip_gear(player, 0, r["gear_id"])
        assert un["success"]
        assert svc.dig_repo.get_equipped_gear(player, 0) == {}


class TestDigGearServiceRepair:
    def test_repair_charges_33pct_of_tier_price(self, svc, player):
        # Diamond Plate: tier 3, shop_price 180 -> repair = 59 (180 * 0.33)
        r = svc.buy_gear(player, 0, "armor", 3)
        gid = r["gear_id"]
        # Drop durability to 5 manually
        svc.dig_repo.repair_gear(gid, 5)
        bal_before = svc.player_repo.get_balance(player, 0)
        result = svc.repair_gear(player, 0, gid)
        assert result["success"]
        assert result["cost"] == 59
        assert svc.player_repo.get_balance(player, 0) == bal_before - 59
        assert svc.dig_repo.get_gear_by_id(gid)["durability"] == GEAR_MAX_DURABILITY

    def test_repair_refuses_when_full(self, svc, player):
        r = svc.buy_gear(player, 0, "armor", 1)
        result = svc.repair_gear(player, 0, r["gear_id"])
        assert not result["success"]
        assert "full durability" in result["error"]

    def test_repair_all_sums_costs(self, svc, player):
        a = svc.buy_gear(player, 0, "armor", 1)["gear_id"]   # 20 * 0.33 = 7
        b = svc.buy_gear(player, 0, "boots", 2)["gear_id"]   # 70 * 0.33 = 23
        # Damage both
        svc.dig_repo.repair_gear(a, 5)
        svc.dig_repo.repair_gear(b, 7)
        bal_before = svc.player_repo.get_balance(player, 0)
        result = svc.repair_all_gear(player, 0)
        assert result["success"]
        assert result["repaired"] == 2
        assert result["cost"] == 7 + 23
        assert svc.player_repo.get_balance(player, 0) == bal_before - 30

    def test_repair_all_with_nothing_damaged_errors(self, svc, player):
        svc.buy_gear(player, 0, "armor", 1)  # full durability
        result = svc.repair_all_gear(player, 0)
        assert not result["success"]


class TestDigGearServiceRelicCap:
    def test_cap_at_prestige_zero_is_one_slot(self, svc, player):
        svc.dig_repo.update_tunnel(player, 0, prestige_level=0)
        a = svc.dig_repo.add_artifact(player, 0, "mole_claws", is_relic=True)
        b = svc.dig_repo.add_artifact(player, 0, "magma_heart", is_relic=True)
        r1 = svc.equip_relic_for_player(player, 0, a)
        assert r1["success"]
        r2 = svc.equip_relic_for_player(player, 0, b)
        assert not r2["success"]
        assert "cap (1)" in r2["error"]

    def test_prestige_2_allows_three_relics(self, svc, player):
        svc.dig_repo.update_tunnel(player, 0, prestige_level=2)
        ids = [
            svc.dig_repo.add_artifact(player, 0, x, is_relic=True)
            for x in ("mole_claws", "magma_heart", "crystal_compass", "echo_stone")
        ]
        # First 3 succeed, 4th fails
        for i in range(3):
            r = svc.equip_relic_for_player(player, 0, ids[i])
            assert r["success"], r
        r = svc.equip_relic_for_player(player, 0, ids[3])
        assert not r["success"]
        assert "cap (3)" in r["error"]

    def test_unequip_then_re_equip_works(self, svc, player):
        svc.dig_repo.update_tunnel(player, 0, prestige_level=0)
        a = svc.dig_repo.add_artifact(player, 0, "mole_claws", is_relic=True)
        b = svc.dig_repo.add_artifact(player, 0, "magma_heart", is_relic=True)
        svc.equip_relic_for_player(player, 0, a)
        svc.unequip_relic_for_player(player, 0, a)
        r = svc.equip_relic_for_player(player, 0, b)
        assert r["success"]


class TestDigGearServiceApplyGearToCombat:
    def test_empty_loadout_returns_unchanged_stats(self, svc):
        base = dict(BOSS_DUEL_STATS["bold"])
        loadout = GearLoadout()
        out = svc._apply_gear_to_combat(base, loadout)
        # Identity except for floor/ceiling (which don't trigger here)
        assert out["player_hp"] == base["player_hp"]
        assert out["player_dmg"] == base["player_dmg"]
        assert abs(out["player_hit"] - base["player_hit"]) < 1e-9
        assert abs(out["boss_hit"] - base["boss_hit"]) < 1e-9

    def test_full_loadout_applies_each_axis(self, svc):
        base = dict(BOSS_DUEL_STATS["bold"])
        weapon = GearPiece(
            id=1, slot=GearSlot.WEAPON, tier=7, durability=20,
            equipped=True, acquired_at=0, source="boss_drop",
            tier_def=WEAPON_TIERS[7],
        )
        armor = GearPiece(
            id=2, slot=GearSlot.ARMOR, tier=7, durability=20,
            equipped=True, acquired_at=0, source="boss_drop",
            tier_def=ARMOR_TIERS[7],
        )
        boots = GearPiece(
            id=3, slot=GearSlot.BOOTS, tier=7, durability=20,
            equipped=True, acquired_at=0, source="boss_drop",
            tier_def=BOOTS_TIERS[7],
        )
        loadout = GearLoadout(weapon=weapon, armor=armor, boots=boots)
        out = svc._apply_gear_to_combat(base, loadout)
        # Void-Touched stat sums (per dig_constants.py)
        assert out["player_dmg"] == base["player_dmg"] + 2   # weapon +2
        assert out["player_hp"]  == base["player_hp"] + 4    # armor +4
        assert abs(out["player_hit"] - (base["player_hit"] + 0.07)) < 1e-9
        assert abs(out["boss_hit"]  - (base["boss_hit"]  - 0.13)) < 1e-9

    def test_drop_depth_map_uses_renumbered_tiers(self):
        """Boss-drop depth map points at the new tier numbering after the
        Stormrend insertion: 100 -> 4 (Obsidian), 150 -> 5 (Stormrend),
        200 -> 6 (Frostforged), 275 -> 7 (Void-Touched)."""
        from services.dig_constants import GEAR_DROP_DEPTH_TIER_MAP
        assert GEAR_DROP_DEPTH_TIER_MAP == {100: 4, 150: 5, 200: 6, 275: 7}

    def test_armor_hp_progression_is_monotonic_and_meaningful(self):
        """Armor's HP bonus should increase by tier and produce a real
        soak in mid-tier gear (Diamond+) so the slot is felt in fights.
        Pins the post-buff curve."""
        bonuses = [t.player_hp_bonus for t in ARMOR_TIERS]
        # tiers 0..7: Wooden, Stone, Iron, Diamond, Obsidian, Stormrend, Frostforged, Void-Touched
        assert bonuses == [0, 0, 1, 2, 3, 3, 3, 4]
        # Diamond+ should be at least +2 — anything less is invisible
        # against the base HP of 2-5 in BOSS_DUEL_STATS.
        assert ARMOR_TIERS[3].player_hp_bonus >= 2

    def test_weapon_dmg_progression_pins_smoothed_curve(self):
        """Weapon player_dmg should show real progression at Diamond and
        Frostforged so mid-tier purchases feel meaningful. Pins the curve."""
        dmg = [t.player_dmg for t in WEAPON_TIERS]
        # tiers 0..7: Wooden, Stone, Iron, Diamond, Obsidian, Stormrend, Frostforged, Void-Touched
        assert dmg == [0, 0, 0, 1, 1, 1, 2, 2]
        # Diamond is the first dmg-bearing pickaxe (depth 75 milestone).
        assert WEAPON_TIERS[3].player_dmg >= 1
        # Frostforged (now tier 6 after Stormrend insertion) is the next visible step.
        assert WEAPON_TIERS[6].player_dmg >= 2

    def test_player_hit_clamps_to_ceiling(self, svc):
        base = {"player_hp": 5, "boss_hp": 5, "player_hit": 0.99, "player_dmg": 1,
                "boss_hit": 0.5, "boss_dmg": 1}
        weapon = GearPiece(
            id=1, slot=GearSlot.WEAPON, tier=7, durability=20,
            equipped=True, acquired_at=0, source="boss_drop",
            tier_def=WEAPON_TIERS[7],
        )
        loadout = GearLoadout(weapon=weapon)
        out = svc._apply_gear_to_combat(base, loadout)
        assert out["player_hit"] <= PLAYER_HIT_CEILING

    def test_boss_hit_floors_at_005(self, svc):
        """Even Void-Touched boots can't push boss accuracy below 5%."""
        base = {"player_hp": 5, "boss_hp": 5, "player_hit": 0.5, "player_dmg": 1,
                "boss_hit": 0.05, "boss_dmg": 1}
        boots = GearPiece(
            id=3, slot=GearSlot.BOOTS, tier=7, durability=20,
            equipped=True, acquired_at=0, source="boss_drop",
            tier_def=BOOTS_TIERS[7],
        )
        out = svc._apply_gear_to_combat(base, GearLoadout(boots=boots))
        assert out["boss_hit"] >= 0.05


class TestDigGearServiceActivePickaxeTier:
    def test_falls_back_to_legacy_column(self, svc, player):
        svc.dig_repo.update_tunnel(player, 0, pickaxe_tier=4)
        tunnel = dict(svc.dig_repo.get_tunnel(player, 0))
        # No equipped weapon yet
        assert svc._get_active_pickaxe_tier(player, 0, tunnel) == 4

    def test_equipped_weapon_takes_priority(self, svc, player):
        svc.dig_repo.update_tunnel(player, 0, pickaxe_tier=2)
        gid = svc.dig_repo.add_gear(player, 0, "weapon", 5)
        svc.dig_repo.equip_gear(gid, player, 0, "weapon")
        tunnel = dict(svc.dig_repo.get_tunnel(player, 0))
        assert svc._get_active_pickaxe_tier(player, 0, tunnel) == 5


class TestDigGearServiceBuyGear:
    def test_buy_succeeds_with_depth_and_funds(self, svc, player):
        bal_before = svc.player_repo.get_balance(player, 0)
        r = svc.buy_gear(player, 0, "armor", 1)
        assert r["success"]
        assert r["cost"] == 20
        assert svc.player_repo.get_balance(player, 0) == bal_before - 20
        owned = svc.dig_repo.get_gear(player, 0)
        assert any(g["slot"] == "armor" and g["tier"] == 1 for g in owned)

    def test_buy_refuses_drop_only_tier(self, svc, player):
        r = svc.buy_gear(player, 0, "armor", 5)
        assert not r["success"]
        assert "boss kills" in r["error"]

    def test_buy_refuses_when_underdepth(self, svc, player):
        svc.dig_repo.update_tunnel(player, 0, depth=10)  # below tier 2 req of 50
        r = svc.buy_gear(player, 0, "armor", 2)
        assert not r["success"]
        assert "depth" in r["error"].lower()

    def test_buy_refuses_when_broke(self, svc, player):
        svc.player_repo.add_balance(player, 0, -5000)  # zero out
        r = svc.buy_gear(player, 0, "armor", 1)
        assert not r["success"]
        assert "JC" in r["error"]


class TestDigGearServiceAtomicDebit:
    """Confirm the repair flows can't drive balance negative under a race."""

    def test_repair_succeeds_when_just_funded(self, svc, player):
        """``try_debit`` is a single conditional UPDATE — if it succeeds the
        balance is debited atomically by exactly ``cost`` JC."""
        # Buy Diamond Plate (180 JC) while flush, then drain balance to 60
        # and damage the piece. Diamond repair = 59 JC, so 60 is enough.
        gid = svc.buy_gear(player, 0, "armor", 3)["gear_id"]
        svc.player_repo.add_balance(player, 0, -(svc.player_repo.get_balance(player, 0) - 60))
        svc.dig_repo.repair_gear(gid, 5)
        assert svc.player_repo.get_balance(player, 0) == 60
        r = svc.repair_gear(player, 0, gid)
        assert r["success"]
        assert svc.player_repo.get_balance(player, 0) == 1

    def test_repair_does_not_charge_on_insufficient_balance(self, svc, player):
        gid = svc.buy_gear(player, 0, "armor", 3)["gear_id"]
        # Drain to 5 JC (Diamond repair would cost 59)
        svc.player_repo.add_balance(player, 0, -(svc.player_repo.get_balance(player, 0) - 5))
        svc.dig_repo.repair_gear(gid, 5)
        bal_before = svc.player_repo.get_balance(player, 0)
        r = svc.repair_gear(player, 0, gid)
        assert not r["success"]
        # Balance unchanged — try_debit was a no-op when the WHERE clause failed.
        assert svc.player_repo.get_balance(player, 0) == bal_before


class TestDigGearServiceTryDebit:
    """Direct coverage on PlayerRepository.try_debit."""

    def test_succeeds_when_funded(self, svc, player):
        starting = svc.player_repo.get_balance(player, 0)
        ok = svc.player_repo.try_debit(player, 0, 100)
        assert ok is True
        assert svc.player_repo.get_balance(player, 0) == starting - 100

    def test_fails_when_short_and_does_not_charge(self, svc, player):
        starting = svc.player_repo.get_balance(player, 0)
        ok = svc.player_repo.try_debit(player, 0, starting + 1)
        assert ok is False
        assert svc.player_repo.get_balance(player, 0) == starting

    def test_zero_amount_is_a_noop_success(self, svc, player):
        starting = svc.player_repo.get_balance(player, 0)
        assert svc.player_repo.try_debit(player, 0, 0) is True
        assert svc.player_repo.get_balance(player, 0) == starting


# =============================================================================
# Relic label formatter
# =============================================================================


class TestRelicLabelFormatter:
    """format_relic_label resolves plain + pinnacle artifact_ids to display
    strings. Pure function over the constants module — no fixtures needed."""

    def test_plain_known_relic(self):
        assert format_relic_label("frozen_clock") == "Frozen Clock"

    def test_plain_known_relic_with_stats_flag_unchanged(self):
        # with_stats only adds parens for pinnacles; plain relics ignore it
        assert format_relic_label("frozen_clock", with_stats=True) == "Frozen Clock"

    def test_pinnacle_without_stats(self):
        pid = "pinnacle:Pickaxe:Patience:boss_hit_minus:hp_plus_1"
        assert format_relic_label(pid) == "Pickaxe of Patience"

    def test_pinnacle_with_stats(self):
        pid = "pinnacle:Pickaxe:Patience:boss_hit_minus:hp_plus_1"
        assert (
            format_relic_label(pid, with_stats=True)
            == "Pickaxe of Patience (Bosses miss more often, Tougher skin)"
        )

    def test_unknown_plain_id_returns_raw(self):
        assert format_relic_label("not_a_real_relic") == "not_a_real_relic"

    def test_pinnacle_skips_unknown_stat_id(self):
        pid = "pinnacle:Pickaxe:Patience:bogus_stat:hp_plus_1"
        assert (
            format_relic_label(pid, with_stats=True)
            == "Pickaxe of Patience (Tougher skin)"
        )
