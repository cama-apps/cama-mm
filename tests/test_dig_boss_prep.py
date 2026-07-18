"""Boss-preparation consumables arm once and persist for one full attempt."""

import json
from types import SimpleNamespace

import pytest

from repositories.dig_repository import DigRepository
from services.dig_service import DigService


@pytest.fixture
def service(repo_db_path, player_repository, guild_id):
    player_repository.add(
        discord_id=91001,
        discord_username="prep_player",
        guild_id=guild_id,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    repo = DigRepository(repo_db_path)
    repo.create_tunnel(91001, guild_id, "Prep Tunnel")
    repo.update_tunnel(
        91001,
        guild_id,
        depth=24,
        boss_progress=json.dumps({
            "25": {"boss_id": "grothak", "status": "active"},
        }),
    )
    return DigService(repo, player_repository)


def test_arming_boss_prep_consumes_exact_queued_row_atomically(
    service,
    guild_id,
):
    item_id = service.dig_repo.add_inventory_item(
        91001,
        guild_id,
        "tempered_whetstone",
    )
    service.dig_repo.queue_item(item_id)
    tunnel = dict(service.dig_repo.get_tunnel(91001, guild_id))
    progress = service._get_boss_progress_entries(tunnel)

    prep = service._activate_boss_prep(
        91001,
        guild_id,
        progress,
        25,
    )

    assert prep == {"item_type": "tempered_whetstone", "used": False}
    assert service.dig_repo.get_inventory(91001, guild_id) == []
    stored = service._get_boss_progress_entries(
        dict(service.dig_repo.get_tunnel(91001, guild_id)),
    )
    assert stored["25"]["active_prep"] == prep


def test_existing_active_prep_is_reused_across_phases(service, guild_id):
    progress = {
        "25": {
            "boss_id": "grothak",
            "status": "phase1_defeated",
            "active_prep": {"item_type": "rescue_line", "used": False},
        },
    }

    prep = service._activate_boss_prep(
        91001,
        guild_id,
        progress,
        25,
    )

    assert prep["item_type"] == "rescue_line"


def test_boss_prep_effect_helpers(service):
    whetstone = {"item_type": "tempered_whetstone", "used": False}
    rescue = {"item_type": "rescue_line", "used": False}

    assert service._apply_boss_prep_damage(3, whetstone) == 4
    assert service._apply_boss_prep_damage(3, rescue) == 3
    assert service._apply_boss_prep_loss(7, rescue) == (4, True)
    assert service._apply_boss_prep_loss(7, whetstone) == (7, False)


def test_whetstone_adds_exactly_one_after_damage_multipliers(
    service,
    guild_id,
    monkeypatch,
):
    item_id = service.dig_repo.add_inventory_item(
        91001,
        guild_id,
        "tempered_whetstone",
    )
    service.dig_repo.queue_item(item_id)
    service.mana_effects_service = SimpleNamespace(
        get_effects=lambda *_args: SimpleNamespace(
            color="White",
            boss_hp_mult=1.0,
            boss_damage_variance_modifier=0.0,
            boss_damage_mult=1.2,
            boss_no_crit_against=False,
        ),
    )

    def fixed_player_stats(base_stats, _loadout):
        stats = dict(base_stats)
        stats["player_dmg"] = 4
        return stats

    captured = {}

    def capture_win_chance(**stats):
        captured.update(stats)
        return 0.5

    monkeypatch.setattr(service, "_apply_gear_to_combat", fixed_player_stats)
    monkeypatch.setattr(
        "services.dig_service._approx_duel_win_prob",
        capture_win_chance,
    )
    monkeypatch.setattr("services.dig.combat_mixin.random.random", lambda: 0.0)

    result = service.fight_boss(91001, guild_id, "cautious")

    assert result["success"]
    # White turns the underlying 4 damage into 4; the prep then adds exactly 1.
    assert captured["player_dmg"] == 5


def test_warding_salts_block_only_first_mechanic(service):
    prep = {"item_type": "warding_salts", "used": False}

    assert service._consume_warding_salts(prep) is True
    assert prep["used"] is True
    assert service._consume_warding_salts(prep) is False
