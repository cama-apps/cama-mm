"""Focused coverage for atomic hostile-loss protection."""

from __future__ import annotations

import sqlite3
import time

import pytest

from infrastructure.schema_manager import SchemaManager
from repositories.buff_repository import BuffRepository
from repositories.loan_repository import LoanRepository
from repositories.mana_repository import ManaRepository
from repositories.player_repository import PlayerRepository
from repositories.protection_repository import ProtectionRepository
from services.buff_service import BuffService
from services.mana_service import get_today_pst
from services.protection_service import ProtectionService
from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY


@pytest.fixture
def protection_stack(repo_db_path):
    player_repo = PlayerRepository(repo_db_path)
    mana_repo = ManaRepository(repo_db_path)
    buff_repo = BuffRepository(repo_db_path)
    protection_repo = ProtectionRepository(repo_db_path)
    return {
        "service": ProtectionService(protection_repo),
        "repo": protection_repo,
        "players": player_repo,
        "mana": mana_repo,
        "buffs": BuffService(buff_repo),
        "buff_repo": buff_repo,
        "loans": LoanRepository(repo_db_path),
        "db_path": repo_db_path,
    }


def _player(repo: PlayerRepository, discord_id: int, balance: int, guild_id=TEST_GUILD_ID):
    repo.add(
        discord_id=discord_id,
        discord_username=f"player-{discord_id}",
        guild_id=guild_id,
    )
    repo.update_balance(discord_id, guild_id, balance)


def test_plains_claim_resets_guardian_capacity_only_for_plains(protection_stack):
    mana = protection_stack["mana"]
    today = get_today_pst()

    assert mana.claim_mana_atomic(1, TEST_GUILD_ID, "Plains", today)
    assert mana.get_white_shield_remaining(1, TEST_GUILD_ID) == 25

    with sqlite3.connect(protection_stack["db_path"]) as conn:
        conn.execute(
            "UPDATE player_mana SET white_shield_remaining = 7 "
            "WHERE discord_id = ? AND guild_id = ?",
            (1, TEST_GUILD_ID),
        )
    assert not mana.claim_mana_atomic(1, TEST_GUILD_ID, "Plains", today)
    assert mana.get_white_shield_remaining(1, TEST_GUILD_ID) == 7

    assert mana.claim_mana_atomic(1, TEST_GUILD_ID, "Forest", "2099-01-02")
    assert mana.get_white_shield_remaining(1, TEST_GUILD_ID) == 0
    assert mana.claim_mana_atomic(1, TEST_GUILD_ID, "Plains", "2099-01-03")
    assert mana.get_white_shield_remaining(1, TEST_GUILD_ID) == 25


def test_white_shield_migration_backfills_existing_unconsumed_plains(
    protection_stack,
):
    mana = protection_stack["mana"]
    mana.set_mana(2, TEST_GUILD_ID, "Plains", get_today_pst())
    assert mana.get_white_shield_remaining(2, TEST_GUILD_ID) == 0

    with sqlite3.connect(protection_stack["db_path"]) as conn:
        conn.execute(
            "DELETE FROM schema_migrations "
            "WHERE name = 'add_white_shield_remaining_to_player_mana'"
        )
    SchemaManager(protection_stack["db_path"]).initialize()

    assert mana.get_white_shield_remaining(2, TEST_GUILD_ID) == 25


def test_guardian_halves_current_loss_and_spends_absorbed_capacity(protection_stack):
    service = protection_stack["service"]
    players = protection_stack["players"]
    mana = protection_stack["mana"]
    _player(players, 10, 100)
    mana.claim_mana_atomic(10, TEST_GUILD_ID, "Plains", get_today_pst())

    result = service.apply_hostile_loss(
        10,
        TEST_GUILD_ID,
        20,
        "pyroclasm",
        actor_id=99,
        event_key="pyro:one",
    )

    assert (result.requested, result.attempted, result.absorbed, result.applied) == (
        20,
        20,
        10,
        10,
    )
    assert result.applied_loss == 10
    assert result.absorbed_amount == 10
    assert players.get_balance(10, TEST_GUILD_ID) == 90
    assert mana.get_white_shield_remaining(10, TEST_GUILD_ID) == 15
    assert result.details[0].source == "guardian"


def test_aegis_capacity_reduces_both_sides_of_player_transfer(protection_stack):
    service = protection_stack["service"]
    players = protection_stack["players"]
    buffs = protection_stack["buffs"]
    _player(players, 10, 100)
    _player(players, 99, 0)
    buffs.grant_aegis(10, TEST_GUILD_ID)

    result = service.apply_hostile_loss(
        10,
        TEST_GUILD_ID,
        100,
        "red_shell",
        actor_id=99,
        event_key="red-shell:one",
        destination="player",
        recipient_id=99,
    )

    assert result.absorbed == 75
    assert result.applied == 25
    assert players.get_balance(10, TEST_GUILD_ID) == 75
    assert players.get_balance(99, TEST_GUILD_ID) == 25
    assert result.destination_balance_before == 0
    assert result.destination_balance_after == 25

    duplicate = service.apply_hostile_loss(
        10,
        TEST_GUILD_ID,
        100,
        "red_shell",
        actor_id=99,
        event_key="red-shell:one",
        destination="player",
        recipient_id=99,
    )
    assert duplicate.duplicate is True
    assert players.get_balance(10, TEST_GUILD_ID) == 75
    assert players.get_balance(99, TEST_GUILD_ID) == 25


def test_shared_sanctuary_pool_is_consumed_by_caster_and_ally(protection_stack):
    service = protection_stack["service"]
    players = protection_stack["players"]
    buffs = protection_stack["buffs"]
    buff_repo = protection_stack["buff_repo"]
    _player(players, 10, 200)
    _player(players, 11, 200)
    sanctuary_id = buffs.grant_sanctuary(10, TEST_GUILD_ID, 11)

    caster = service.apply_hostile_loss(
        10,
        TEST_GUILD_ID,
        100,
        "lightning_bolt",
        actor_id=99,
        event_key="bolt:caster",
        destination="reserve",
    )
    ally = service.apply_hostile_loss(
        11,
        TEST_GUILD_ID,
        80,
        "lightning_bolt",
        actor_id=99,
        event_key="bolt:ally",
        destination="reserve",
    )

    assert (caster.absorbed, caster.applied) == (100, 0)
    assert (ally.absorbed, ally.applied) == (50, 30)
    assert protection_stack["loans"].get_nonprofit_fund(TEST_GUILD_ID) == 30
    assert buff_repo.active_for(10, TEST_GUILD_ID, "sanctuary") == []
    with sqlite3.connect(protection_stack["db_path"]) as conn:
        data = conn.execute(
            "SELECT triggered, data FROM manashop_buffs WHERE id = ?",
            (sanctuary_id,),
        ).fetchone()
    assert data is not None and data[0] == 1


def test_reprieve_then_guardian_compound_in_documented_order(protection_stack):
    service = protection_stack["service"]
    players = protection_stack["players"]
    mana = protection_stack["mana"]
    buffs = protection_stack["buffs"]
    buff_repo = protection_stack["buff_repo"]
    _player(players, 10, 100)
    reprieve_id = buffs.grant_reprieve(10, TEST_GUILD_ID)
    mana.claim_mana_atomic(10, TEST_GUILD_ID, "Plains", get_today_pst())

    result = service.apply_hostile_loss(
        10,
        TEST_GUILD_ID,
        20,
        "soul_harvest",
        actor_id=99,
        event_key="harvest:one",
    )

    assert (result.absorbed, result.applied) == (15, 5)
    assert [detail.source for detail in result.details] == ["reprieve", "guardian"]
    assert players.get_balance(10, TEST_GUILD_ID) == 95
    assert mana.get_white_shield_remaining(10, TEST_GUILD_ID) == 20
    active = buff_repo.active_for(10, TEST_GUILD_ID, "reprieve")
    assert active[0]["id"] == reprieve_id
    assert active[0]["data"]["capacity_remaining"] == 15


def test_self_caused_loss_bypasses_shields_and_retro(protection_stack):
    service = protection_stack["service"]
    players = protection_stack["players"]
    mana = protection_stack["mana"]
    buffs = protection_stack["buffs"]
    _player(players, 10, 100)
    buffs.grant_aegis(10, TEST_GUILD_ID)
    mana.claim_mana_atomic(10, TEST_GUILD_ID, "Plains", get_today_pst())

    now = int(time.time())
    result = service.apply_hostile_loss(
        10,
        TEST_GUILD_ID,
        20,
        "blue_shell",
        actor_id=10,
        event_key="blue-shell:self",
        destination="reserve",
        occurred_at=now,
    )

    assert result.shieldable is False
    assert (result.absorbed, result.applied) == (0, 20)
    assert players.get_balance(10, TEST_GUILD_ID) == 80
    assert mana.get_white_shield_remaining(10, TEST_GUILD_ID) == 25
    assert service.reconcile_guardian(10, TEST_GUILD_ID, now - 60) == 0


def test_guardian_retro_reimburses_once(protection_stack):
    service = protection_stack["service"]
    repo = protection_stack["repo"]
    players = protection_stack["players"]
    mana = protection_stack["mana"]
    _player(players, 10, 100)
    now = int(time.time())

    service.apply_hostile_loss(
        10,
        TEST_GUILD_ID,
        20,
        "wildfire",
        actor_id=99,
        event_key="wildfire:before-mana",
        occurred_at=now - 10,
    )
    mana.claim_mana_atomic(10, TEST_GUILD_ID, "Plains", get_today_pst())

    assert service.reconcile_guardian(10, TEST_GUILD_ID, now - 60) == 10
    assert service.reconcile_guardian(10, TEST_GUILD_ID, now - 60) == 0
    assert players.get_balance(10, TEST_GUILD_ID) == 90
    assert mana.get_white_shield_remaining(10, TEST_GUILD_ID) == 15
    assert repo.get_event(10, TEST_GUILD_ID, "wildfire:before-mana")[
        "retro_covered"
    ] == 10


def test_reprieve_retro_uses_rolling_pool_and_is_idempotent(protection_stack):
    service = protection_stack["service"]
    players = protection_stack["players"]
    buffs = protection_stack["buffs"]
    buff_repo = protection_stack["buff_repo"]
    _player(players, 10, 100)
    now = int(time.time())

    service.apply_hostile_loss(
        10,
        TEST_GUILD_ID,
        20,
        "pyroclasm",
        actor_id=99,
        event_key="pyro:retro",
        occurred_at=now - 100,
    )
    buff_id = buffs.grant_reprieve(10, TEST_GUILD_ID)

    assert service.reconcile_purchased_pool(
        10, TEST_GUILD_ID, buff_id, 24 * 3600, now=now
    ) == 10
    assert service.reconcile_purchased_pool(
        10, TEST_GUILD_ID, buff_id, 24 * 3600, now=now
    ) == 0
    assert players.get_balance(10, TEST_GUILD_ID) == 90
    active = buff_repo.active_for(10, TEST_GUILD_ID, "reprieve")
    assert active[0]["data"]["capacity_remaining"] == 15


def test_min_balance_short_circuits_before_spending_pool(protection_stack):
    service = protection_stack["service"]
    players = protection_stack["players"]
    buffs = protection_stack["buffs"]
    buff_repo = protection_stack["buff_repo"]
    _player(players, 10, 49)
    buffs.grant_aegis(10, TEST_GUILD_ID)

    result = service.apply_hostile_loss(
        10,
        TEST_GUILD_ID,
        20,
        "bomb_omb",
        actor_id=99,
        event_key="bomb:threshold",
        min_balance=50,
        clamp_to_balance=True,
    )

    assert (result.requested, result.attempted, result.absorbed, result.applied) == (
        20,
        0,
        0,
        0,
    )
    assert players.get_balance(10, TEST_GUILD_ID) == 49
    active = buff_repo.active_for(10, TEST_GUILD_ID, "aegis")
    assert active[0]["data"]["capacity_remaining"] == 75


def test_non_jc_attack_consumes_aegis_once(protection_stack):
    service = protection_stack["service"]
    players = protection_stack["players"]
    buffs = protection_stack["buffs"]
    buff_repo = protection_stack["buff_repo"]
    _player(players, 10, 100)
    buffs.grant_aegis(10, TEST_GUILD_ID)

    first = service.block_non_jc_attack(
        10, TEST_GUILD_ID, actor_id=99, event_key="sabotage:one"
    )
    duplicate = service.block_non_jc_attack(
        10, TEST_GUILD_ID, actor_id=99, event_key="sabotage:one"
    )

    assert first.blocked is True and first.source == "aegis"
    assert duplicate.blocked is True and duplicate.duplicate is True
    assert buff_repo.active_for(10, TEST_GUILD_ID, "aegis") == []


def test_protection_is_guild_isolated(protection_stack):
    service = protection_stack["service"]
    players = protection_stack["players"]
    buffs = protection_stack["buffs"]
    _player(players, 10, 100, TEST_GUILD_ID)
    _player(players, 10, 100, TEST_GUILD_ID_SECONDARY)
    buffs.grant_aegis(10, TEST_GUILD_ID)

    result = service.apply_hostile_loss(
        10,
        TEST_GUILD_ID_SECONDARY,
        20,
        "pyroclasm",
        actor_id=99,
        event_key="pyro:other-guild",
    )

    assert (result.absorbed, result.applied) == (0, 20)
    assert players.get_balance(10, TEST_GUILD_ID) == 100
    assert players.get_balance(10, TEST_GUILD_ID_SECONDARY) == 80
