"""Behavioral coverage for the three event-only Dig rings."""

from __future__ import annotations

import pytest

import services.dig_service as dig_service_module
from repositories.dig_repository import DigRepository
from repositories.player_repository import PlayerRepository
from services.dig_data.items import UNIQUE_GEAR
from services.dig_service import DigService

PLAYER_ID = 731
GUILD_ID = 44


@pytest.fixture
def ring_service(repo_db_path):
    dig_repo = DigRepository(repo_db_path)
    player_repo = PlayerRepository(repo_db_path)
    player_repo.add(
        discord_id=PLAYER_ID,
        discord_username="Ringbearer",
        guild_id=GUILD_ID,
    )
    player_repo.update_balance(PLAYER_ID, GUILD_ID, 10_000)
    dig_repo.create_tunnel(PLAYER_ID, GUILD_ID, "Loop Mine")
    dig_repo.update_tunnel(
        PLAYER_ID,
        GUILD_ID,
        depth=10,
        max_depth=10,
        luminosity=100,
    )
    return DigService(dig_repo, player_repo)


def _equip_ring(service: DigService, item_id: str) -> int:
    definition = UNIQUE_GEAR[item_id]
    gear_id = service.dig_repo.add_gear(
        PLAYER_ID,
        GUILD_ID,
        definition.slot.value,
        definition.reference_tier,
        source="event:test",
        durability=definition.max_durability,
        item_id=item_id,
    )
    result = service.equip_gear(PLAYER_ID, GUILD_ID, gear_id)
    assert result["success"] is True
    return gear_id


def _choice_event(*, event_id: str, success_chance: float, failure_jc: int = 0):
    return {
        "id": event_id,
        "name": "Ring Probe",
        "rarity": "uncommon",
        "safe_option": {
            "success_chance": 1.0,
            "success": {
                "advance": 1,
                "jc": 0,
                "description": "A measured step.",
            },
        },
        "risky_option": {
            "success_chance": success_chance,
            "success": {
                "advance": 1,
                "jc": 0,
                "description": "The wager holds.",
            },
            "failure": {
                "advance": 0,
                "jc": failure_jc,
                "description": "The wager breaks.",
            },
        },
    }


def _pin_event_rng(monkeypatch, *, roll: float) -> None:
    monkeypatch.setattr("services.dig.events_mixin.random.random", lambda: roll)
    monkeypatch.setattr("services.dig.events_mixin.random.uniform", lambda _a, _b: 1.0)
    monkeypatch.setattr("services.dig.events_mixin.random.randint", lambda _a, _b: 0)


def test_surveyors_loop_adds_one_block_to_safe_success(
    ring_service,
    monkeypatch,
):
    _equip_ring(ring_service, "surveyors_loop")
    event = _choice_event(event_id="survey_stride", success_chance=0.50)
    monkeypatch.setattr(dig_service_module, "EVENT_POOL", [event])
    _pin_event_rng(monkeypatch, roll=0.0)

    result = ring_service.resolve_event(
        PLAYER_ID,
        GUILD_ID,
        "survey_stride",
        "safe",
    )

    assert result["success"] is True
    assert result["succeeded"] is True
    assert result["depth_delta"] == 2


def test_surveyors_loop_penalizes_risky_choices_only_while_functional(
    ring_service,
    monkeypatch,
):
    ring_id = _equip_ring(ring_service, "surveyors_loop")
    event = _choice_event(event_id="survey_risk", success_chance=0.50)
    monkeypatch.setattr(dig_service_module, "EVENT_POOL", [event])
    _pin_event_rng(monkeypatch, roll=0.47)

    active_result = ring_service.resolve_event(
        PLAYER_ID,
        GUILD_ID,
        "survey_risk",
        "risky",
    )
    ring_service.dig_repo.repair_gear(ring_id, 0)
    broken_result = ring_service.resolve_event(
        PLAYER_ID,
        GUILD_ID,
        "survey_risk",
        "risky",
    )

    assert active_result["succeeded"] is False
    assert broken_result["succeeded"] is True


def test_ruinwager_signet_improves_risky_success_chance(
    ring_service,
    monkeypatch,
):
    _equip_ring(ring_service, "ruinwager_signet")
    event = _choice_event(event_id="ruin_edge", success_chance=0.50)
    monkeypatch.setattr(dig_service_module, "EVENT_POOL", [event])
    _pin_event_rng(monkeypatch, roll=0.53)

    result = ring_service.resolve_event(
        PLAYER_ID,
        GUILD_ID,
        "ruin_edge",
        "risky",
    )

    assert result["succeeded"] is True


def test_ruinwager_signet_makes_failed_jc_losses_one_quarter_harsher(
    ring_service,
    monkeypatch,
):
    _equip_ring(ring_service, "ruinwager_signet")
    event = _choice_event(
        event_id="ruin_loss",
        success_chance=0.50,
        failure_jc=-8,
    )
    monkeypatch.setattr(dig_service_module, "EVENT_POOL", [event])
    _pin_event_rng(monkeypatch, roll=0.99)

    result = ring_service.resolve_event(
        PLAYER_ID,
        GUILD_ID,
        "ruin_loss",
        "risky",
    )

    # -8 authored -> -10 base event tuning -> -14 Dig pressure ->
    # -18 Ruinwager drawback -> -20 shared deflationary scaling.
    assert result["succeeded"] is False
    assert result["jc_delta"] == -20


def test_red_thread_band_rerolls_only_the_first_missed_attack(
    ring_service,
    monkeypatch,
):
    _equip_ring(ring_service, "red_thread_band")
    status = ring_service._trophy_status_seed(
        PLAYER_ID,
        GUILD_ID,
        player_start_hp=4,
    )
    rolls = iter((0.90, 0.20, 0.90, 0.90, 0.90))
    monkeypatch.setattr(
        "services.dig.combat_mixin.random.random",
        lambda: next(rolls),
    )

    first, _, boss_hp, _ = ring_service._run_one_round(
        round_num=1,
        player_hp=4,
        boss_hp=5,
        player_hit=0.50,
        player_dmg=1,
        boss_hit=0.50,
        boss_dmg=1,
        status_effects=status,
    )
    second, _, boss_hp, _ = ring_service._run_one_round(
        round_num=2,
        player_hp=4,
        boss_hp=boss_hp,
        player_hit=0.50,
        player_dmg=1,
        boss_hit=0.50,
        boss_dmg=1,
        status_effects=status,
    )

    assert first["player_hit"] is True
    assert first["red_thread_reroll"] is True
    assert boss_hp == 4
    assert second["player_hit"] is False
    assert "red_thread_reroll" not in second
    assert status["gear_reroll_first_miss"] is False


def test_red_thread_band_preserves_reroll_during_silenced_round(
    ring_service,
    monkeypatch,
):
    _equip_ring(ring_service, "red_thread_band")
    status = ring_service._trophy_status_seed(
        PLAYER_ID,
        GUILD_ID,
        player_start_hp=4,
    )
    status["silenced_next_round"] = True
    monkeypatch.setattr(
        "services.dig.combat_mixin.random.random",
        lambda: 0.90,
    )

    entry, _, _, _ = ring_service._run_one_round(
        round_num=1,
        player_hp=4,
        boss_hp=5,
        player_hit=0.50,
        player_dmg=1,
        boss_hit=0.50,
        boss_dmg=1,
        status_effects=status,
    )

    assert entry["player_hit"] is False
    assert "red_thread_reroll" not in entry
    assert status["gear_reroll_first_miss"] is True
