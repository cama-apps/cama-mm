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
from services.dig_constants import BOSS_BOUNDARIES, PINNACLE_DEPTH
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


def _fix_combat_stats(dig_service, monkeypatch, *, player_hp, boss_hp, boss_dmg):
    """Pin the combat numbers so the round outcomes are fully scripted."""
    monkeypatch.setattr(
        dig_service,
        "_apply_gear_to_combat",
        lambda base, loadout: {
            "player_hp": player_hp,
            "player_hit": 0.5,
            "player_dmg": 1,
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
