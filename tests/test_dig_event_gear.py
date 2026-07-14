"""Event-only unique gear registry, persistence, and atomic grant tests."""

from __future__ import annotations

import sqlite3
from dataclasses import replace

import pytest

from infrastructure.schema_manager import SchemaManager
from repositories.dig_repository import DigRepository
from repositories.player_repository import PlayerRepository
from services.dig_data import items
from services.dig_service import DigService

APPROVED_UNIQUE_GEAR = {
    "glassbreaker_pick": ("weapon", 8),
    "needle_pick": ("weapon", 16),
    "briarplate": ("armor", 14),
    "nullweave_mantle": ("armor", 12),
    "springheel_boots": ("boots", 14),
    "anchor_boots": ("boots", 16),
    "loaded_die": ("amulet", 12),
    "blood_locket": ("amulet", 14),
}


def _service(repo_db_path: str) -> DigService:
    dig_repo = DigRepository(repo_db_path)
    player_repo = PlayerRepository(repo_db_path)
    player_repo.add(discord_id=101, discord_username="Unique", guild_id=7)
    player_repo.update_balance(101, 7, 100)
    dig_repo.create_tunnel(101, 7, "Sidegrade Shaft")
    return DigService(dig_repo, player_repo)


def test_unique_gear_registry_contains_the_approved_sidegrades():
    registry = items.UNIQUE_GEAR

    assert set(registry) == set(APPROVED_UNIQUE_GEAR)
    for item_id, (slot, durability) in APPROVED_UNIQUE_GEAR.items():
        definition = registry[item_id]
        assert definition.item_id == item_id
        assert definition.slot.value == slot
        assert definition.reference_tier == 3
        assert definition.repair_value == 200
        assert definition.max_durability == durability
        assert definition.effect_summary

    assert registry["glassbreaker_pick"].player_dmg == 2
    assert registry["glassbreaker_pick"].player_hit == pytest.approx(-0.08)
    assert registry["needle_pick"].player_hit == pytest.approx(0.08)
    assert registry["needle_pick"].crit_chance == pytest.approx(0.03)
    assert registry["briarplate"].effect_id == "reflect_first_hit"
    assert registry["nullweave_mantle"].effect_id == "block_first_status"
    assert registry["springheel_boots"].effect_id == "springheel_counter"
    assert registry["anchor_boots"].effect_id == "block_first_skip"
    assert registry["loaded_die"].crit_chance == pytest.approx(0.10)
    assert registry["loaded_die"].crit_bonus == 1
    assert registry["loaded_die"].player_hit == pytest.approx(-0.05)
    assert registry["blood_locket"].crit_chance == pytest.approx(0.05)
    assert registry["blood_locket"].player_hp_bonus == -1
    assert registry["blood_locket"].effect_id == "heal_first_crit"


def test_fresh_schema_adds_nullable_item_id_to_dig_gear(tmp_path):
    db_path = str(tmp_path / "unique-gear.db")
    SchemaManager(db_path).initialize()

    with sqlite3.connect(db_path) as conn:
        columns = {
            row[1]: row
            for row in conn.execute("PRAGMA table_info(dig_gear)").fetchall()
        }

    assert "item_id" in columns
    assert columns["item_id"][3] == 0  # nullable for legacy tier gear


def test_repository_persists_unique_item_identity_and_custom_durability(repo_db_path):
    repo = DigRepository(repo_db_path)

    gear_id = repo.add_gear(
        101,
        7,
        "weapon",
        3,
        source="event:collapsed_armory",
        durability=8,
        item_id="glassbreaker_pick",
    )

    row = repo.get_gear_by_id(gear_id)
    assert row is not None
    assert row["item_id"] == "glassbreaker_pick"
    assert row["durability"] == 8


def test_service_hydrates_and_serializes_unique_gear(repo_db_path):
    service = _service(repo_db_path)
    gear_id = service.dig_repo.add_gear(
        101,
        7,
        "armor",
        3,
        source="event:dead_prospectors_pack",
        durability=14,
        item_id="briarplate",
    )

    inventory = service.get_inventory_gear(101, 7)
    unique = next(piece for piece in inventory if piece["id"] == gear_id)

    assert unique["item_id"] == "briarplate"
    assert unique["name"] == "Briarplate"
    assert unique["max_durability"] == 14
    assert unique["durability"] == 14
    assert unique["effect"] == (
        "+1 HP; reflect 1 damage on the first boss hit."
    )

    service.dig_repo.equip_gear(gear_id, 101, 7, "armor")
    equipped = service.get_loadout(101, 7)["armor"]
    assert equipped["effect"] == unique["effect"]


def test_unique_weapon_uses_authored_dig_modifiers(repo_db_path, monkeypatch):
    service = _service(repo_db_path)
    gear_id = service.dig_repo.add_gear(
        101,
        7,
        "weapon",
        3,
        source="event:collapsed_armory",
        durability=8,
        item_id="glassbreaker_pick",
    )
    service.dig_repo.equip_gear(gear_id, 101, 7, "weapon")
    monkeypatch.setitem(
        items.UNIQUE_GEAR,
        "glassbreaker_pick",
        replace(
            items.UNIQUE_GEAR["glassbreaker_pick"],
            advance_bonus=7,
            cave_in_reduction=0.12,
            loot_bonus=9,
        ),
    )

    tunnel = dict(service.dig_repo.get_tunnel(101, 7))
    modifiers = service._get_active_pickaxe_data(101, 7, tunnel)

    assert modifiers["advance_bonus"] == 7
    assert modifiers["cave_in_reduction"] == pytest.approx(0.12)
    assert modifiers["loot_bonus"] == 9


def test_unknown_unique_item_id_fails_closed_during_hydration(repo_db_path):
    service = _service(repo_db_path)
    service.dig_repo.add_gear(
        101,
        7,
        "armor",
        3,
        source="event:broken",
        item_id="not_registered",
    )

    assert all(
        piece.get("item_id") != "not_registered"
        for piece in service.get_inventory_gear(101, 7)
    )


def test_atomic_tunnel_update_can_grant_unique_gear(repo_db_path):
    service = _service(repo_db_path)

    gear_id = service.dig_repo.atomic_tunnel_balance_update(
        101,
        7,
        balance_delta=-6,
        tunnel_updates={"depth": 12},
        add_gear={
            "slot": "weapon",
            "tier": 3,
            "durability": 8,
            "source": "event:collapsed_armory",
            "item_id": "glassbreaker_pick",
        },
        log_action_type="event",
        log_detail={"event_id": "collapsed_armory", "gear": "glassbreaker_pick"},
    )

    assert isinstance(gear_id, int)
    assert service.player_repo.get_balance(101, 7) == 94
    assert service.dig_repo.get_tunnel(101, 7)["depth"] == 12
    assert service.dig_repo.get_gear_by_id(gear_id)["item_id"] == "glassbreaker_pick"


def test_atomic_unique_gear_grant_rolls_back_with_event_failure(repo_db_path):
    service = _service(repo_db_path)
    before_balance = service.player_repo.get_balance(101, 7)
    before_depth = service.dig_repo.get_tunnel(101, 7)["depth"]

    with pytest.raises(TypeError):
        service.dig_repo.atomic_tunnel_balance_update(
            101,
            7,
            balance_delta=-6,
            tunnel_updates={"depth": 12},
            add_gear={
                "slot": "weapon",
                "tier": 3,
                "durability": 8,
                "source": "event:collapsed_armory",
                "item_id": "glassbreaker_pick",
            },
            log_action_type="event",
            log_detail={"not_json_serializable": {1, 2, 3}},
        )

    assert service.player_repo.get_balance(101, 7) == before_balance
    assert service.dig_repo.get_tunnel(101, 7)["depth"] == before_depth
    assert not any(
        row.get("item_id") == "glassbreaker_pick"
        for row in service.dig_repo.get_gear(101, 7)
    )


def test_resolve_event_grants_and_returns_unique_gear_atomically(
    repo_db_path, monkeypatch,
):
    service = _service(repo_db_path)
    event = {
        "id": "gear_integration",
        "name": "Gear Integration",
        "rarity": "uncommon",
        "risky_option": {
            "success_chance": 1.0,
            "success": {
                "jc": -6,
                "advance": 0,
                "description": "Recovered.",
                "gear_reward_pool": ["glassbreaker_pick", "needle_pick"],
            },
        },
    }
    monkeypatch.setattr("services.dig_service.EVENT_POOL", [event])
    monkeypatch.setattr("services.dig.events_mixin.random.random", lambda: 0.0)
    monkeypatch.setattr("services.dig.events_mixin.random.uniform", lambda a, b: 1.0)
    monkeypatch.setattr(
        "services.dig.events_mixin.random.choice", lambda pool: pool[0],
    )

    result = service.resolve_event(101, 7, "gear_integration", "risky")

    assert result["success"] is True
    assert result["gear_drop"]["item_id"] == "glassbreaker_pick"
    assert result["gear_drop"]["name"] == "Glassbreaker Pick"
    assert result["gear_drop"]["max_durability"] == 8
    assert result["gear_drop"]["effect"] == (
        "Diamond dig bonuses; +2 boss damage; -8% hit chance."
    )
    assert service.dig_repo.get_gear_by_id(result["gear_drop"]["gear_id"])[
        "item_id"
    ] == "glassbreaker_pick"


def test_resolve_event_gear_grant_rolls_back_with_event_log_failure(
    repo_db_path, monkeypatch,
):
    service = _service(repo_db_path)
    event = {
        "id": "gear_rollback",
        "name": "Gear Rollback",
        "rarity": "uncommon",
        "risky_option": {
            "success_chance": 1.0,
            "success": {
                "jc": -6,
                "advance": 0,
                "description": "Recovered.",
                "gear_reward_pool": ["glassbreaker_pick"],
            },
        },
    }
    monkeypatch.setattr("services.dig_service.EVENT_POOL", [event])
    monkeypatch.setattr("services.dig.events_mixin.random.random", lambda: 0.0)
    monkeypatch.setattr("services.dig.events_mixin.random.uniform", lambda a, b: 1.0)
    monkeypatch.setattr(
        "services.dig.events_mixin.random.choice", lambda pool: pool[0],
    )
    monkeypatch.setattr(
        "repositories.dig_repository.json.dumps",
        lambda value: (_ for _ in ()).throw(TypeError("event log failed")),
    )
    before_balance = service.player_repo.get_balance(101, 7)

    with pytest.raises(TypeError, match="event log failed"):
        service.resolve_event(101, 7, "gear_rollback", "risky")

    assert service.player_repo.get_balance(101, 7) == before_balance
    assert not any(
        row.get("item_id") == "glassbreaker_pick"
        for row in service.dig_repo.get_gear(101, 7)
    )
