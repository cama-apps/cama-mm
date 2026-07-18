"""Mechanical coverage for the new ordinary relic set."""

from __future__ import annotations

import json

import pytest

from domain.models.boss_mechanics import MechanicOption, OutcomeRoll
from repositories.dig_repository import DigRepository
from repositories.player_repository import PlayerRepository
from services.dig_data.balance import scale_positive_dig_jc
from services.dig_service import DigService


@pytest.fixture
def relic_effect_service(repo_db_path):
    dig_repo = DigRepository(repo_db_path)
    player_repo = PlayerRepository(repo_db_path)
    player_repo.add(discord_id=701, discord_username="Relic Tester", guild_id=17)
    player_repo.update_balance(701, 17, 1_000)
    dig_repo.create_tunnel(701, 17, "Relic Effects")
    dig_repo.update_tunnel(701, 17, total_digs=1, depth=10, luminosity=60)
    return DigService(dig_repo, player_repo)


def _equip(service: DigService, artifact_id: str) -> None:
    row_id = service.dig_repo.add_artifact(701, 17, artifact_id, is_relic=True)
    service.dig_repo.equip_relic(row_id, 701, 17, True)
    service._invalidate_relic_cache(701, 17)


def test_chipped_compass_adds_one_block_to_safe_success(
    relic_effect_service, monkeypatch,
):
    _equip(relic_effect_service, "chipped_compass")
    event = {
        "id": "compass_test",
        "name": "Compass Test",
        "rarity": "common",
        "safe_option": {
            "success_chance": 1.0,
            "success": {"advance": 2, "jc": 0, "description": "Safe."},
        },
    }
    monkeypatch.setattr("services.dig_service.EVENT_POOL", [event])
    monkeypatch.setattr("services.dig.events_mixin.random.random", lambda: 0.0)
    monkeypatch.setattr("services.dig.events_mixin.random.randint", lambda a, b: 0)

    result = relic_effect_service.resolve_event(701, 17, "compass_test", "safe")

    assert result["succeeded"] is True
    assert result["depth_delta"] == 3


def test_black_wax_seal_boosts_cursed_risky_success_and_spends_duration(
    relic_effect_service, monkeypatch,
):
    _equip(relic_effect_service, "black_wax_seal")
    relic_effect_service.dig_repo.update_tunnel(
        701,
        17,
        temp_curses=json.dumps({
            "id": "test_hex",
            "name": "Test Hex",
            "digs_remaining": 3,
            "effect": {"advance_bonus": -1},
        }),
    )
    event = {
        "id": "seal_test",
        "name": "Seal Test",
        "rarity": "common",
        "risky_option": {
            "success_chance": 0.50,
            "success": {"advance": 0, "jc": 0, "description": "Won."},
            "failure": {"advance": 0, "jc": 0, "description": "Lost."},
        },
    }
    monkeypatch.setattr("services.dig_service.EVENT_POOL", [event])
    monkeypatch.setattr("services.dig.events_mixin.random.random", lambda: 0.54)
    relic_effect_service.dig_repo.update_tunnel(701, 17, luminosity=100)

    result = relic_effect_service.resolve_event(701, 17, "seal_test", "risky")

    assert result["succeeded"] is True
    curse = json.loads(relic_effect_service.dig_repo.get_tunnel(701, 17)["temp_curses"])
    assert curse["digs_remaining"] == 2


def test_burning_ledger_increases_event_gains_and_strengthened_losses(
    relic_effect_service, monkeypatch,
):
    _equip(relic_effect_service, "burning_ledger")
    gain = {
        "id": "ledger_gain",
        "name": "Ledger Gain",
        "rarity": "common",
        "safe_option": {
            "success_chance": 1.0,
            "success": {"advance": 0, "jc": 100, "description": "Gain."},
        },
    }
    loss = {
        "id": "ledger_loss",
        "name": "Ledger Loss",
        "rarity": "common",
        "risky_option": {
            "success_chance": 0.0,
            "success": {"advance": 0, "jc": 0, "description": "No."},
            "failure": {"advance": 0, "jc": -100, "description": "Loss."},
        },
    }
    monkeypatch.setattr("services.dig_service.EVENT_POOL", [gain, loss])
    monkeypatch.setattr("services.dig.events_mixin.random.random", lambda: 0.5)
    monkeypatch.setattr("services.dig.events_mixin.random.uniform", lambda a, b: 1.0)
    monkeypatch.setattr(
        "services.dig.events_mixin.scale_minigame_jc_delta", lambda value: value,
    )
    monkeypatch.setattr(
        "services.dig.events_mixin.scale_deflationary_minigame_jc_delta",
        lambda value: value,
    )

    gain_result = relic_effect_service.resolve_event(701, 17, "ledger_gain", "safe")
    loss_result = relic_effect_service.resolve_event(701, 17, "ledger_loss", "risky")

    assert gain_result["jc_delta"] == scale_positive_dig_jc(115)
    assert loss_result["jc_delta"] == -224


def test_lantern_stub_restores_five_on_first_daily_dig(relic_effect_service):
    _equip(relic_effect_service, "lantern_stub")
    tunnel = dict(relic_effect_service.dig_repo.get_tunnel(701, 17))
    tunnel["last_dig_at"] = None
    lum_info = {"luminosity_after": 60, "drained": 3}

    luminosity = relic_effect_service._apply_lantern_stub_restore(
        701, 17, tunnel, lum_info, "2026-07-13",
    )

    assert luminosity == 65
    assert lum_info["luminosity_after"] == 65
    assert lum_info["lantern_stub_restored"] == 5


def test_bone_abacus_strengthens_stamina_discount_and_taxes_paid_yield(
    relic_effect_service,
):
    _equip(relic_effect_service, "bone_abacus")
    tunnel = dict(relic_effect_service.dig_repo.get_tunnel(701, 17))
    tunnel["stat_stamina"] = 5

    assert relic_effect_service._apply_stamina_to_paid_cost(100, tunnel) == 75
    assert relic_effect_service._relic_jc_yield_multiplier(
        701,
        17,
        include_random=False,
        is_paid_dig=True,
    ) == pytest.approx(0.90)


def test_paper_crane_blocks_first_boss_status(relic_effect_service, monkeypatch):
    _equip(relic_effect_service, "paper_crane")
    status = relic_effect_service._trophy_status_seed(701, 17, player_start_hp=5)
    option = MechanicOption(
        label="Take it",
        flavor="Test",
        outcome_rolls=(OutcomeRoll(1.0, 0, 0, None, "burn", "Burned."),),
    )
    monkeypatch.setattr("services.dig.combat_mixin.random.random", lambda: 0.0)

    _, _, _, updated = relic_effect_service._apply_option_outcome_to_state(
        option=option,
        player_hp=5,
        boss_hp=5,
        status_effects=status,
    )

    assert updated["relic_paper_crane"] is False
    assert updated["paper_crane_blocked"] == "burn"
    assert "burn_rounds_remaining" not in updated


def test_bottled_quake_adds_damage_to_first_landed_hit(relic_effect_service):
    status = {"relic_bottled_quake": True}

    entry, _, boss_hp, _ = relic_effect_service._run_one_round(
        round_num=1,
        player_hp=5,
        boss_hp=4,
        player_hit=1.0,
        player_dmg=1,
        boss_hit=0.0,
        boss_dmg=1,
        status_effects=status,
    )

    assert boss_hp == 2
    assert entry["bottled_quake"] is True
    assert status["relic_bottled_quake"] is False


def test_shifting_idol_rolls_a_fresh_attempt_bonus(
    relic_effect_service, monkeypatch,
):
    _equip(relic_effect_service, "shifting_idol")
    choices = iter(("hp", "crit"))
    monkeypatch.setattr(
        "services.dig.combat_mixin.random.choice", lambda pool: next(choices),
    )

    first = relic_effect_service._apply_shifting_idol_stats(701, 17, 5, 0.60, 0.10)
    second = relic_effect_service._apply_shifting_idol_stats(701, 17, 5, 0.60, 0.10)

    assert first == (6, 0.60, 0.10, "hp")
    assert second == (5, 0.60, pytest.approx(0.15), "crit")
