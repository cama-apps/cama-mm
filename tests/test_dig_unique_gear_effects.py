"""Boss-combat effects for event-only horizontal gear."""

from __future__ import annotations

import pytest

from domain.models.boss_mechanics import MechanicOption, OutcomeRoll
from repositories.dig_repository import DigRepository
from repositories.player_repository import PlayerRepository
from services.dig_data.items import UNIQUE_GEAR
from services.dig_service import DigService


@pytest.fixture
def unique_service(repo_db_path):
    dig_repo = DigRepository(repo_db_path)
    player_repo = PlayerRepository(repo_db_path)
    player_repo.add(discord_id=901, discord_username="Sidegrade", guild_id=19)
    dig_repo.create_tunnel(901, 19, "Horizontal Shaft")
    return DigService(dig_repo, player_repo)


def _equip_unique(service: DigService, item_id: str) -> int:
    definition = UNIQUE_GEAR[item_id]
    gear_id = service.dig_repo.add_gear(
        901,
        19,
        definition.slot.value,
        definition.reference_tier,
        source="event:test",
        durability=definition.max_durability,
        item_id=item_id,
    )
    service.dig_repo.equip_gear(
        gear_id, 901, 19, definition.slot.value,
    )
    return gear_id


def test_broken_unique_gear_does_not_seed_its_combat_effect(unique_service):
    gear_id = _equip_unique(unique_service, "briarplate")
    unique_service.dig_repo.repair_gear(gear_id, 0)

    status = unique_service._trophy_status_seed(901, 19, player_start_hp=5)

    assert "gear_reflect_first_hit" not in status


def test_nullweave_blocks_first_status_and_anchor_blocks_first_player_skip(
    unique_service, monkeypatch,
):
    _equip_unique(unique_service, "nullweave_mantle")
    _equip_unique(unique_service, "anchor_boots")
    status = unique_service._trophy_status_seed(901, 19, player_start_hp=5)
    monkeypatch.setattr("services.dig.combat_mixin.random.random", lambda: 0.0)
    status_option = MechanicOption(
        "Status", "Status", (OutcomeRoll(1.0, 0, 0, None, "burn", "Burn."),),
    )
    skip_option = MechanicOption(
        "Skip", "Skip", (OutcomeRoll(1.0, 0, 0, "player", None, "Skip."),),
    )

    _, _, _, status = unique_service._apply_option_outcome_to_state(
        option=status_option, player_hp=5, boss_hp=5, status_effects=status,
    )
    _, _, _, status = unique_service._apply_option_outcome_to_state(
        option=skip_option, player_hp=5, boss_hp=5, status_effects=status,
    )

    assert status["gear_block_first_status"] is False
    assert status["nullweave_blocked"] == "burn"
    assert "burn_rounds_remaining" not in status
    assert status["gear_block_first_skip"] is False
    assert status["anchor_boots_blocked"] is True
    assert "skip_next_round_for" not in status


def test_briarplate_reflects_first_landed_boss_hit(unique_service):
    _equip_unique(unique_service, "briarplate")
    status = unique_service._trophy_status_seed(901, 19, player_start_hp=5)

    entry, player_hp, boss_hp, _ = unique_service._run_one_round(
        round_num=1,
        player_hp=5,
        boss_hp=5,
        player_hit=0.0,
        player_dmg=1,
        boss_hit=1.0,
        boss_dmg=1,
        status_effects=status,
    )

    assert player_hp == 4
    assert boss_hp == 4
    assert entry["briarplate_reflect"] is True
    assert status["gear_reflect_first_hit"] is False


def test_springheel_boots_counter_first_dodge(unique_service):
    _equip_unique(unique_service, "springheel_boots")
    status = unique_service._trophy_status_seed(901, 19, player_start_hp=5)

    entry, _, boss_hp, _ = unique_service._run_one_round(
        round_num=1,
        player_hp=5,
        boss_hp=5,
        player_hit=0.0,
        player_dmg=1,
        boss_hit=0.0,
        boss_dmg=1,
        status_effects=status,
    )

    assert boss_hp == 4
    assert entry["springheel_counter"] is True
    assert status["gear_springheel_counter"] is False


def test_blood_locket_heals_on_first_crit_without_overhealing(unique_service):
    _equip_unique(unique_service, "blood_locket")
    status = unique_service._trophy_status_seed(901, 19, player_start_hp=5)

    entry, player_hp, _, _ = unique_service._run_one_round(
        round_num=1,
        player_hp=4,
        boss_hp=5,
        player_hit=1.0,
        player_dmg=1,
        boss_hit=0.0,
        boss_dmg=1,
        status_effects=status,
        crit_chance=1.0,
        crit_bonus=1,
    )

    assert player_hp == 5
    assert entry["blood_locket_heal"] is True
    assert status["gear_heal_first_crit"] is False
