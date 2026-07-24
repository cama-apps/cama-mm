"""Relic/trophy combat effects must apply in pinnacle rounds without a pause.

The pinnacle's first-pass round loop previously honored only a small subset
of combat effects, while the shared round engine used after a mid-fight
prompt pause applied all of them — so the same fight behaved differently
depending on whether a prompt fired. These tests pin two of the previously
missing effects on the no-pause path; both fail against the old inline loop.
"""

from __future__ import annotations

import json
import random
import time

import pytest

import domain.models.boss_mechanics as boss_mechanics
from repositories.dig_repository import DigRepository
from services.dig_constants import (
    BOSS_BOUNDARIES,
    BOSS_PAYOUTS,
    PINNACLE_DEPTH,
)
from services.dig_service import DigService
from tests.conftest import TEST_GUILD_ID

DISCORD_ID = 10001


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def dig_service(dig_repo, player_repository, monkeypatch):
    svc = DigService(dig_repo, player_repository)
    monkeypatch.setattr(svc, "_get_weather_effects", lambda guild_id, layer_name: {})
    return svc


def _at_pinnacle(dig_service, dig_repo, player_repository, monkeypatch):
    """Park the player at the pinnacle boundary with no mechanic prompt."""
    player_repository.add(
        discord_id=DISCORD_ID,
        discord_username="Pinnacle Tester",
        guild_id=TEST_GUILD_ID,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    player_repository.update_balance(DISCORD_ID, TEST_GUILD_ID, 2000)
    monkeypatch.setattr(time, "time", lambda: 1_000_000)
    monkeypatch.setattr(random, "random", lambda: 0.99)
    dig_service.dig(DISCORD_ID, TEST_GUILD_ID)

    bp: dict = {str(b): "defeated" for b in BOSS_BOUNDARIES}
    bp[str(PINNACLE_DEPTH)] = {
        "status": "active",
        "boss_id": "forgotten_king",
        "first_meet_seen": True,
    }
    dig_repo.update_tunnel(
        DISCORD_ID, TEST_GUILD_ID,
        depth=PINNACLE_DEPTH - 1,
        boss_progress=json.dumps(bp),
        prestige_level=0,
        pinnacle_boss_id="forgotten_king",
        pinnacle_phase=1,
        luminosity=100,
    )
    # No mid-fight prompt — the whole fight resolves in one pass, which is
    # exactly the path that used to skip most relic effects.
    monkeypatch.setattr(boss_mechanics, "get_mechanic", lambda mid: None)


def _fix_combat_stats(
    dig_service,
    monkeypatch,
    *,
    player_hp,
    boss_hp,
    boss_dmg,
    player_hit=0.5,
    player_dmg=1,
):
    """Pin the combat numbers so the round outcomes are fully scripted."""
    monkeypatch.setattr(
        dig_service,
        "_apply_gear_to_combat",
        lambda base, loadout: {
            "player_hp": player_hp,
            "player_hit": player_hit,
            "player_dmg": player_dmg,
            "crit_chance": 0.0,
            "crit_bonus": 0,
        },
    )
    monkeypatch.setattr(
        dig_service,
        "_scale_boss_stats",
        lambda stats, **kwargs: {
            "boss_hp": boss_hp,
            "boss_hit": 1.0,
            "boss_dmg": boss_dmg,
        },
    )


def test_scout_pinnacle_uses_the_locked_phase_boss(
    dig_service, dig_repo, player_repository, monkeypatch,
):
    _at_pinnacle(dig_service, dig_repo, player_repository, monkeypatch)
    dig_repo.add_inventory_item(
        DISCORD_ID, TEST_GUILD_ID, "lantern",
    )

    result = dig_service.scout_boss(
        DISCORD_ID, TEST_GUILD_ID,
    )

    assert result["success"] is True
    assert result["boundary"] == PINNACLE_DEPTH
    assert result["boss_id"] == "forgotten_king"
    assert result["boss_name"] == "The Forgotten King"
    assert set(result["odds"]) == {"cautious", "bold", "reckless"}
    assert not any(
        item["item_type"] == "lantern"
        for item in dig_repo.get_inventory(DISCORD_ID, TEST_GUILD_ID)
    )


def test_pinnacle_scout_multiplier_matches_live_payout(
    dig_service, dig_repo, player_repository, monkeypatch,
):
    _at_pinnacle(dig_service, dig_repo, player_repository, monkeypatch)
    dig_repo.update_tunnel(
        DISCORD_ID, TEST_GUILD_ID, prestige_level=4,
    )
    dig_repo.add_inventory_item(
        DISCORD_ID, TEST_GUILD_ID, "lantern",
    )
    monkeypatch.setattr(
        "services.dig_service._approx_duel_win_prob",
        lambda **_stats: 0.5,
    )

    result = dig_service.scout_boss(
        DISCORD_ID, TEST_GUILD_ID,
    )

    assert result["odds"]["cautious"]["multiplier"] == BOSS_PAYOUTS[
        PINNACLE_DEPTH
    ][0]


def test_deaths_door_saves_a_killing_blow_without_a_pause(
    dig_service, dig_repo, player_repository, monkeypatch,
):
    _at_pinnacle(dig_service, dig_repo, player_repository, monkeypatch)
    monkeypatch.setattr(
        dig_service, "_has_relic", lambda d, g, rid: rid == "deaths_door",
    )
    _fix_combat_stats(dig_service, monkeypatch, player_hp=2, boss_hp=30, boss_dmg=1)

    # Per round: player-swing roll then boss-swing roll. 0.99 = player miss,
    # 0.5 = boss hit. Round 2's boss hit drops the player to 0; the next
    # roll (0.2 < 0.40) is the one-shot save. Round 3 (fallback 0.99 rolls)
    # kills for real because the save was consumed.
    rolls = iter([0.99, 0.5, 0.99, 0.5, 0.2])
    monkeypatch.setattr(random, "random", lambda: next(rolls, 0.99))

    result = dig_service.fight_boss(DISCORD_ID, TEST_GUILD_ID, "bold", wager=0)

    assert result["success"]
    assert result["won"] is False
    assert any(e.get("deaths_door") for e in result["round_log"])


def test_lifesteal_heals_on_first_landed_hit_without_a_pause(
    dig_service, dig_repo, player_repository, monkeypatch,
):
    _at_pinnacle(dig_service, dig_repo, player_repository, monkeypatch)
    monkeypatch.setattr(
        dig_service, "_has_relic", lambda d, g, rid: rid == "runebitten_shard",
    )
    _fix_combat_stats(dig_service, monkeypatch, player_hp=5, boss_hp=3, boss_dmg=1)

    # Every roll low: the player lands every swing (boss dies in 3 rounds)
    # and the boss hits back each round without ever killing.
    monkeypatch.setattr(random, "random", lambda: 0.0)

    result = dig_service.fight_boss(DISCORD_ID, TEST_GUILD_ID, "bold", wager=0)

    assert result["success"]
    assert result["won"] is True
    assert any(e.get("lifesteal") for e in result["round_log"])


def test_wagered_pinnacle_fight_keeps_ten_percent_hit_floor(
    dig_service, dig_repo, player_repository, monkeypatch,
):
    _at_pinnacle(dig_service, dig_repo, player_repository, monkeypatch)
    _fix_combat_stats(
        dig_service,
        monkeypatch,
        player_hp=1,
        boss_hp=1,
        boss_dmg=1,
        player_hit=0.0,
        player_dmg=100,
    )
    monkeypatch.setattr(random, "random", lambda: 0.09)

    result = dig_service.fight_boss(
        DISCORD_ID, TEST_GUILD_ID, "reckless", wager=10,
    )

    assert result["success"]
    assert result["won"] is True


def test_pinnacle_loss_extra_wear_only_hits_armor(
    dig_service, dig_repo, player_repository, monkeypatch,
):
    _at_pinnacle(dig_service, dig_repo, player_repository, monkeypatch)
    gear_ids = {
        slot: dig_repo.add_gear(DISCORD_ID, TEST_GUILD_ID, slot, 1)
        for slot in ("weapon", "armor", "boots", "amulet")
    }
    for slot, gear_id in gear_ids.items():
        dig_repo.equip_gear(gear_id, DISCORD_ID, TEST_GUILD_ID, slot)
    durability_before = {
        slot: dig_repo.get_gear_by_id(gear_id)["durability"]
        for slot, gear_id in gear_ids.items()
    }
    _fix_combat_stats(
        dig_service,
        monkeypatch,
        player_hp=1,
        boss_hp=30,
        boss_dmg=1,
        player_hit=0.0,
    )
    monkeypatch.setattr(random, "random", lambda: 0.99)

    result = dig_service.fight_boss(
        DISCORD_ID, TEST_GUILD_ID, "reckless", wager=10,
    )

    assert result["success"]
    assert result["won"] is False
    assert dig_repo.get_gear_by_id(gear_ids["armor"])["durability"] == (
        durability_before["armor"] - 2
    )
    for slot in ("weapon", "boots", "amulet"):
        assert dig_repo.get_gear_by_id(gear_ids[slot])["durability"] == (
            durability_before[slot] - 1
        )


def test_resumed_pinnacle_loss_ticks_snapshot_armor_after_swap(
    dig_service, dig_repo, player_repository, monkeypatch,
):
    _at_pinnacle(dig_service, dig_repo, player_repository, monkeypatch)
    mechanic = boss_mechanics.MECHANIC_REGISTRY["king_decree"]
    monkeypatch.setattr(
        boss_mechanics,
        "get_mechanic",
        lambda mechanic_id: mechanic if mechanic_id == mechanic.id else None,
    )
    monkeypatch.setattr(
        dig_service,
        "_apply_option_outcome_to_state",
        lambda **kwargs: (
            "The choice changes nothing.",
            kwargs["player_hp"],
            kwargs["boss_hp"],
            kwargs["status_effects"],
        ),
    )

    weapon_id = dig_repo.add_gear(DISCORD_ID, TEST_GUILD_ID, "weapon", 1)
    armor_id = dig_repo.add_gear(DISCORD_ID, TEST_GUILD_ID, "armor", 1)
    boots_id = dig_repo.add_gear(DISCORD_ID, TEST_GUILD_ID, "boots", 1)
    replacement_armor_id = dig_repo.add_gear(
        DISCORD_ID, TEST_GUILD_ID, "armor", 1,
    )
    for slot, gear_id in (
        ("weapon", weapon_id),
        ("armor", armor_id),
        ("boots", boots_id),
    ):
        dig_repo.equip_gear(gear_id, DISCORD_ID, TEST_GUILD_ID, slot)
    snapshot_ids = [weapon_id, armor_id, boots_id]
    durability_before = {
        gear_id: dig_repo.get_gear_by_id(gear_id)["durability"]
        for gear_id in (*snapshot_ids, replacement_armor_id)
    }
    dig_repo.save_active_duel(
        DISCORD_ID,
        TEST_GUILD_ID,
        {
            "boss_id": "forgotten_king",
            "tier": PINNACLE_DEPTH,
            "mechanic_id": mechanic.id,
            "risk_tier": "reckless",
            "wager": 10,
            "player_hp": 1,
            "boss_hp": 30,
            "round_num": mechanic.trigger_round,
            "round_log": "[]",
            "pending_prompt": json.dumps(
                dig_service._serialize_prompt(mechanic)
            ),
            "rng_state": "",
            "status_effects": json.dumps({
                "attempts_this_fight": 1,
                "initial_win_chance": 0.1,
                "pinnacle_state": {
                    "phase": 1,
                    "boss_hp_max": 30,
                    "phase_key": f"{PINNACLE_DEPTH}:1",
                },
                "gear_snapshot_ids": snapshot_ids,
                "armor_snapshot_id": armor_id,
            }),
            "echo_applied": 0,
            "echo_killer_id": None,
            "player_hit": 0.0,
            "player_dmg": 1,
            "boss_hit": 1.0,
            "boss_dmg": 1,
            "crit_chance": 0.0,
            "crit_bonus": 0,
        },
    )
    dig_repo.equip_gear(
        replacement_armor_id, DISCORD_ID, TEST_GUILD_ID, "armor",
    )
    monkeypatch.setattr(random, "random", lambda: 0.99)

    result = dig_service.resume_boss_duel(
        DISCORD_ID, TEST_GUILD_ID, option_idx=0,
    )

    assert result["success"]
    assert result["won"] is False
    assert dig_repo.get_gear_by_id(armor_id)["durability"] == (
        durability_before[armor_id] - 2
    )
    for gear_id in (weapon_id, boots_id):
        assert dig_repo.get_gear_by_id(gear_id)["durability"] == (
            durability_before[gear_id] - 1
        )
    assert dig_repo.get_gear_by_id(replacement_armor_id)["durability"] == (
        durability_before[replacement_armor_id]
    )
