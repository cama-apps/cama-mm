"""Tests for the dig gear service-layer methods."""

import pytest

from domain.models.dig_gear import GearLoadout, GearPiece, GearSlot
from repositories.dig_repository import DigRepository
from repositories.player_repository import PlayerRepository
from services.dig_constants import (
    ARMOR_TIERS,
    BOOTS_TIERS,
    BOSS_DUEL_STATS,
    GEAR_MAX_DURABILITY,
    WEAPON_TIERS,
)
from services.dig_service import DigService


@pytest.fixture
def svc(repo_db_path):
    drepo = DigRepository(repo_db_path)
    prepo = PlayerRepository(repo_db_path)
    return DigService(drepo, prepo)


@pytest.fixture
def player(svc):
    """Register a player with a tunnel + balance, return discord_id."""
    svc.player_repo.add(discord_id=111, discord_username="pf", guild_id=0)
    svc.player_repo.add_balance(111, 0, 5000)
    svc.dig_repo.create_tunnel(111, 0, "Test Tunnel")
    svc.dig_repo.update_tunnel(111, 0, depth=100, prestige_level=1)
    return 111


class TestEquipUnequip:
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


class TestRepair:
    def test_repair_charges_50pct_of_tier_price(self, svc, player):
        # Diamond Plate: tier 3, shop_price 180 -> repair = 90
        r = svc.buy_gear(player, 0, "armor", 3)
        gid = r["gear_id"]
        # Drop durability to 5 manually
        svc.dig_repo.repair_gear(gid, 5)
        bal_before = svc.player_repo.get_balance(player, 0)
        result = svc.repair_gear(player, 0, gid)
        assert result["success"]
        assert result["cost"] == 90
        assert svc.player_repo.get_balance(player, 0) == bal_before - 90
        assert svc.dig_repo.get_gear_by_id(gid)["durability"] == GEAR_MAX_DURABILITY

    def test_repair_refuses_when_full(self, svc, player):
        r = svc.buy_gear(player, 0, "armor", 1)
        result = svc.repair_gear(player, 0, r["gear_id"])
        assert not result["success"]
        assert "full durability" in result["error"]

    def test_repair_all_sums_costs(self, svc, player):
        a = svc.buy_gear(player, 0, "armor", 1)["gear_id"]   # 20 -> 10
        b = svc.buy_gear(player, 0, "boots", 2)["gear_id"]   # 70 -> 35
        # Damage both
        svc.dig_repo.repair_gear(a, 5)
        svc.dig_repo.repair_gear(b, 7)
        bal_before = svc.player_repo.get_balance(player, 0)
        result = svc.repair_all_gear(player, 0)
        assert result["success"]
        assert result["repaired"] == 2
        assert result["cost"] == 10 + 35
        assert svc.player_repo.get_balance(player, 0) == bal_before - 45

    def test_repair_all_with_nothing_damaged_errors(self, svc, player):
        svc.buy_gear(player, 0, "armor", 1)  # full durability
        result = svc.repair_all_gear(player, 0)
        assert not result["success"]


class TestRelicCap:
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


class TestApplyGearToCombat:
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
            id=1, slot=GearSlot.WEAPON, tier=6, durability=20,
            equipped=True, acquired_at=0, source="boss_drop",
            tier_def=WEAPON_TIERS[6],
        )
        armor = GearPiece(
            id=2, slot=GearSlot.ARMOR, tier=6, durability=20,
            equipped=True, acquired_at=0, source="boss_drop",
            tier_def=ARMOR_TIERS[6],
        )
        boots = GearPiece(
            id=3, slot=GearSlot.BOOTS, tier=6, durability=20,
            equipped=True, acquired_at=0, source="boss_drop",
            tier_def=BOOTS_TIERS[6],
        )
        loadout = GearLoadout(weapon=weapon, armor=armor, boots=boots)
        out = svc._apply_gear_to_combat(base, loadout)
        # Void-Touched stat sums (per dig_constants.py)
        assert out["player_dmg"] == base["player_dmg"] + 2   # weapon +2
        assert out["player_hp"]  == base["player_hp"] + 3    # armor +3
        assert abs(out["player_hit"] - (base["player_hit"] + 0.07)) < 1e-9
        assert abs(out["boss_hit"]  - (base["boss_hit"]  - 0.13)) < 1e-9

    def test_player_hit_clamps_to_ceiling(self, svc):
        base = {"player_hp": 5, "boss_hp": 5, "player_hit": 0.99, "player_dmg": 1,
                "boss_hit": 0.5, "boss_dmg": 1}
        weapon = GearPiece(
            id=1, slot=GearSlot.WEAPON, tier=6, durability=20,
            equipped=True, acquired_at=0, source="boss_drop",
            tier_def=WEAPON_TIERS[6],
        )
        loadout = GearLoadout(weapon=weapon)
        out = svc._apply_gear_to_combat(base, loadout)
        assert out["player_hit"] <= 0.95  # PLAYER_HIT_CEILING

    def test_boss_hit_floors_at_005(self, svc):
        """Even Void-Touched boots can't push boss accuracy below 5%."""
        base = {"player_hp": 5, "boss_hp": 5, "player_hit": 0.5, "player_dmg": 1,
                "boss_hit": 0.05, "boss_dmg": 1}
        boots = GearPiece(
            id=3, slot=GearSlot.BOOTS, tier=6, durability=20,
            equipped=True, acquired_at=0, source="boss_drop",
            tier_def=BOOTS_TIERS[6],
        )
        out = svc._apply_gear_to_combat(base, GearLoadout(boots=boots))
        assert out["boss_hit"] >= 0.05


class TestActivePickaxeTier:
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


class TestBuyGear:
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


class TestAtomicDebit:
    """Confirm the repair flows can't drive balance negative under a race."""

    def test_repair_succeeds_when_just_funded(self, svc, player):
        """``try_debit`` is a single conditional UPDATE — if it succeeds the
        balance is debited atomically by exactly ``cost`` JC."""
        # Buy Diamond Plate (180 JC) while flush, then drain balance to 100
        # and damage the piece. Diamond repair = 90 JC, so 100 is enough.
        gid = svc.buy_gear(player, 0, "armor", 3)["gear_id"]
        svc.player_repo.add_balance(player, 0, -(svc.player_repo.get_balance(player, 0) - 100))
        svc.dig_repo.repair_gear(gid, 5)
        assert svc.player_repo.get_balance(player, 0) == 100
        r = svc.repair_gear(player, 0, gid)
        assert r["success"]
        assert svc.player_repo.get_balance(player, 0) == 10

    def test_repair_does_not_charge_on_insufficient_balance(self, svc, player):
        gid = svc.buy_gear(player, 0, "armor", 3)["gear_id"]
        # Drain to 5 JC (Diamond repair would cost 90)
        svc.player_repo.add_balance(player, 0, -(svc.player_repo.get_balance(player, 0) - 5))
        svc.dig_repo.repair_gear(gid, 5)
        bal_before = svc.player_repo.get_balance(player, 0)
        r = svc.repair_gear(player, 0, gid)
        assert not r["success"]
        # Balance unchanged — try_debit was a no-op when the WHERE clause failed.
        assert svc.player_repo.get_balance(player, 0) == bal_before


class TestTryDebit:
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
