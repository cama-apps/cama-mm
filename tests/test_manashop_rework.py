"""Tests for the manashop rework: tap-mana, daily-uses, buffs, and new effects."""

from __future__ import annotations

import time

import pytest

from repositories.buff_repository import BuffRepository
from repositories.mana_repository import ManaRepository
from repositories.slow_drip_repository import SlowDripRepository
from services.buff_service import (
    BUFF_COUNTERSPELL,
    BUFF_DARK_BARGAIN,
    BUFF_OVERGROWTH,
    BuffService,
)
from tests.conftest import TEST_GUILD_ID

USER = 111
ALLY = 222
TARGET = 333


# ──────────────────────────────────────────────────────────────────
# ManaRepository: consumed_today + manashop_daily_uses
# ──────────────────────────────────────────────────────────────────


@pytest.fixture
def mana_repo(repo_db_path):
    return ManaRepository(repo_db_path)


@pytest.fixture
def buff_repo(repo_db_path):
    return BuffRepository(repo_db_path)


@pytest.fixture
def buff_service(buff_repo):
    return BuffService(buff_repo)


@pytest.fixture
def slow_drip_repo(repo_db_path):
    return SlowDripRepository(repo_db_path)


def test_mark_mana_consumed_atomic_first_call_succeeds(mana_repo):
    mana_repo.claim_mana_atomic(USER, TEST_GUILD_ID, "Mountain", "2026-05-09")
    assert mana_repo.is_mana_consumed(USER, TEST_GUILD_ID) is False

    flipped = mana_repo.mark_mana_consumed_atomic(USER, TEST_GUILD_ID)

    assert flipped is True
    assert mana_repo.is_mana_consumed(USER, TEST_GUILD_ID) is True


def test_mark_mana_consumed_atomic_idempotent_within_day(mana_repo):
    mana_repo.claim_mana_atomic(USER, TEST_GUILD_ID, "Mountain", "2026-05-09")
    assert mana_repo.mark_mana_consumed_atomic(USER, TEST_GUILD_ID) is True
    # Second call returns False — already consumed.
    assert mana_repo.mark_mana_consumed_atomic(USER, TEST_GUILD_ID) is False


def test_claim_mana_resets_consumed_flag_next_day(mana_repo):
    mana_repo.claim_mana_atomic(USER, TEST_GUILD_ID, "Mountain", "2026-05-09")
    mana_repo.mark_mana_consumed_atomic(USER, TEST_GUILD_ID)
    assert mana_repo.is_mana_consumed(USER, TEST_GUILD_ID) is True

    # New day rolls fresh mana — consumed flag should reset.
    mana_repo.claim_mana_atomic(USER, TEST_GUILD_ID, "Forest", "2026-05-10")
    assert mana_repo.is_mana_consumed(USER, TEST_GUILD_ID) is False


def test_was_item_used_today_starts_false_then_true(mana_repo):
    today = "2026-05-09"
    assert mana_repo.was_item_used_today(USER, TEST_GUILD_ID, "aegis", today) is False
    assert mana_repo.mark_item_used_atomic(USER, TEST_GUILD_ID, "aegis", today) is True
    assert mana_repo.was_item_used_today(USER, TEST_GUILD_ID, "aegis", today) is True


def test_mark_item_used_atomic_blocks_double_use_same_day(mana_repo):
    today = "2026-05-09"
    assert mana_repo.mark_item_used_atomic(USER, TEST_GUILD_ID, "regrowth", today) is True
    # Same player, same item, same day — second call is a no-op.
    assert mana_repo.mark_item_used_atomic(USER, TEST_GUILD_ID, "regrowth", today) is False
    # Different item is still allowed.
    assert mana_repo.mark_item_used_atomic(USER, TEST_GUILD_ID, "blood_pact", today) is True
    # Different day is allowed.
    assert mana_repo.mark_item_used_atomic(USER, TEST_GUILD_ID, "regrowth", "2026-05-10") is True


# ──────────────────────────────────────────────────────────────────
# BuffRepository / BuffService
# ──────────────────────────────────────────────────────────────────


def test_buff_service_grant_and_active_for(buff_service):
    buff_service.grant_counterspell(USER, TEST_GUILD_ID)
    active = buff_service.buff_repo.active_for(USER, TEST_GUILD_ID, BUFF_COUNTERSPELL)
    assert len(active) == 1
    assert active[0]["buff_type"] == BUFF_COUNTERSPELL
    assert active[0]["expires_at"] > int(time.time())


def test_pvp_immunity_via_counterspell_or_sanctuary(buff_service):
    assert buff_service.has_pvp_immunity(USER, TEST_GUILD_ID) is False

    buff_service.grant_counterspell(USER, TEST_GUILD_ID)
    assert buff_service.has_pvp_immunity(USER, TEST_GUILD_ID) is True


def test_sanctuary_protects_both_caster_and_ally(buff_service):
    buff_service.grant_sanctuary(USER, TEST_GUILD_ID, ALLY)
    assert buff_service.has_pvp_immunity(USER, TEST_GUILD_ID) is True
    # Ally is the target_id of the sanctuary buff cast by USER.
    assert buff_service.has_pvp_immunity(ALLY, TEST_GUILD_ID) is True


def test_consume_aegis_charge_returns_true_then_false(buff_service):
    buff_service.grant_aegis(USER, TEST_GUILD_ID)
    assert buff_service.consume_aegis_charge(USER, TEST_GUILD_ID) is True
    # Second call: nothing to consume.
    assert buff_service.consume_aegis_charge(USER, TEST_GUILD_ID) is False


def test_blood_pact_skim_state_persists(buff_service):
    buff_service.grant_blood_pact(USER, TEST_GUILD_ID, TARGET)
    pact = buff_service.get_blood_pact_skimmer(TARGET, TEST_GUILD_ID)
    assert pact is not None
    assert pact["discord_id"] == USER
    assert pact["data"]["cap"] == 150
    assert pact["data"]["skim_rate"] == 0.25
    # Update the skim total — should round-trip via repo.
    buff_service.record_blood_pact_skim(pact["id"], pact["data"], 25)
    refreshed = buff_service.get_blood_pact_skimmer(TARGET, TEST_GUILD_ID)
    assert refreshed["data"]["skimmed_total"] == 25


def test_blood_pact_skim_calculation_respects_rate_and_cap(buff_service):
    buff_service.grant_blood_pact(USER, TEST_GUILD_ID, TARGET)

    first = buff_service.claim_blood_pact_skim(TARGET, TEST_GUILD_ID, 400)
    second = buff_service.claim_blood_pact_skim(TARGET, TEST_GUILD_ID, 400)

    assert first["skimmer_id"] == USER
    assert first["amount"] == 100
    assert first["new_total"] == 100
    assert second["buff_id"] == first["buff_id"]
    assert second["skimmer_id"] == USER
    assert second["amount"] == 50
    assert second["new_total"] == 150
    refreshed = buff_service.get_blood_pact_skimmer(TARGET, TEST_GUILD_ID)
    assert refreshed["data"]["skimmed_total"] == 150
    assert buff_service.claim_blood_pact_skim(TARGET, TEST_GUILD_ID, 400) is None


def test_blood_pact_skim_reverts_when_transfer_fails(
    repo_db_path, buff_service, buff_repo, monkeypatch,
):
    from repositories.player_repository import PlayerRepository

    player_repo = PlayerRepository(repo_db_path)
    player_repo.add(discord_id=USER, discord_username="Skimmer", guild_id=TEST_GUILD_ID)
    player_repo.add(discord_id=TARGET, discord_username="Target", guild_id=TEST_GUILD_ID)
    player_repo.update_balance(TARGET, TEST_GUILD_ID, 500)
    balance_before = player_repo.get_balance(TARGET, TEST_GUILD_ID)
    buff_service.grant_blood_pact(USER, TEST_GUILD_ID, TARGET)

    def _fail_transfer(*_args, **_kwargs):
        raise RuntimeError("transfer failed")

    monkeypatch.setattr(player_repo, "add_balance_many", _fail_transfer)

    skimmed = buff_service.apply_blood_pact_skim(
        TARGET, TEST_GUILD_ID, 400, player_repo
    )
    assert skimmed == 0
    pact = buff_service.get_blood_pact_skimmer(TARGET, TEST_GUILD_ID)
    assert pact["data"]["skimmed_total"] == 0
    assert player_repo.get_balance(TARGET, TEST_GUILD_ID) == balance_before


def test_blood_pact_apply_transfers_balances(repo_db_path, buff_service):
    from repositories.player_repository import PlayerRepository

    player_repo = PlayerRepository(repo_db_path)
    player_repo.add(discord_id=USER, discord_username="Skimmer", guild_id=TEST_GUILD_ID)
    player_repo.add(discord_id=TARGET, discord_username="Target", guild_id=TEST_GUILD_ID)
    player_repo.update_balance(TARGET, TEST_GUILD_ID, 500)
    skimmer_before = player_repo.get_balance(USER, TEST_GUILD_ID)
    buff_service.grant_blood_pact(USER, TEST_GUILD_ID, TARGET)

    skimmed = buff_service.apply_blood_pact_skim(
        TARGET, TEST_GUILD_ID, 400, player_repo
    )

    assert skimmed == 100
    assert player_repo.get_balance(TARGET, TEST_GUILD_ID) == 400
    assert player_repo.get_balance(USER, TEST_GUILD_ID) == skimmer_before + 100


def test_record_blood_pact_skim_preserves_stored_cap_and_rate(buff_repo, buff_service):
    """Recording a skim must not clobber the pact's stored cap / skim_rate —
    only skimmed_total should change."""
    from services.buff_service import BUFF_BLOOD_PACT

    buff_repo.grant(
        USER, TEST_GUILD_ID, BUFF_BLOOD_PACT, int(time.time()) + 3600,
        target_id=TARGET,
        data={"skimmed_total": 5, "cap": 80, "skim_rate": 0.25},
    )
    pact = buff_service.get_blood_pact_skimmer(TARGET, TEST_GUILD_ID)
    buff_service.record_blood_pact_skim(pact["id"], pact["data"], 30)

    refreshed = buff_service.get_blood_pact_skimmer(TARGET, TEST_GUILD_ID)
    assert refreshed["data"]["skimmed_total"] == 30
    assert refreshed["data"]["cap"] == 80
    assert refreshed["data"]["skim_rate"] == 0.25


def test_dark_bargain_debt_fetches(buff_service):
    buff_service.grant_dark_bargain_debt(USER, TEST_GUILD_ID, amount_due=700)
    debt = buff_service.has_dark_bargain_debt(USER, TEST_GUILD_ID)
    assert debt is not None
    assert debt["data"]["amount_due"] == 700
    assert debt["data"]["default_penalty"] == 1600
    assert debt["data"]["default_penalty_games"] == 5


def test_settle_due_dark_bargain_paid_once(repo_db_path, buff_repo, buff_service):
    from repositories.bankruptcy_repository import BankruptcyRepository
    from repositories.player_repository import PlayerRepository

    player_repo = PlayerRepository(repo_db_path)
    bankruptcy_repo = BankruptcyRepository(repo_db_path)
    player_repo.add(discord_id=USER, discord_username="Buyer", guild_id=TEST_GUILD_ID)
    player_repo.add_balance(USER, TEST_GUILD_ID, 1100)
    buff_repo.grant(
        USER,
        TEST_GUILD_ID,
        BUFF_DARK_BARGAIN,
        int(time.time()) - 1,
        data={"amount_due": 700, "default_penalty": 1600, "default_penalty_games": 5},
    )

    first = buff_service.settle_due_dark_bargains(
        player_repo=player_repo,
        bankruptcy_repo=bankruptcy_repo,
    )
    second = buff_service.settle_due_dark_bargains(
        player_repo=player_repo,
        bankruptcy_repo=bankruptcy_repo,
    )

    assert first == [{"discord_id": USER, "guild_id": TEST_GUILD_ID, "status": "paid", "amount": 700}]
    assert second == []
    assert player_repo.get_balance(USER, TEST_GUILD_ID) == 403


def test_settle_due_dark_bargain_default_adds_penalty(repo_db_path, buff_repo, buff_service):
    from repositories.bankruptcy_repository import BankruptcyRepository
    from repositories.player_repository import PlayerRepository

    player_repo = PlayerRepository(repo_db_path)
    bankruptcy_repo = BankruptcyRepository(repo_db_path)
    player_repo.add(discord_id=USER, discord_username="Buyer", guild_id=TEST_GUILD_ID)
    player_repo.add_balance(USER, TEST_GUILD_ID, 300)
    buff_repo.grant(
        USER,
        TEST_GUILD_ID,
        BUFF_DARK_BARGAIN,
        int(time.time()) - 1,
        data={"amount_due": 700, "default_penalty": 1600, "default_penalty_games": 5},
    )

    settled = buff_service.settle_due_dark_bargains(
        player_repo=player_repo,
        bankruptcy_repo=bankruptcy_repo,
    )

    assert settled == [{"discord_id": USER, "guild_id": TEST_GUILD_ID, "status": "defaulted", "amount": 1600}]
    assert player_repo.get_balance(USER, TEST_GUILD_ID) == -1297
    assert bankruptcy_repo.get_penalty_games(USER, TEST_GUILD_ID) == 5


def test_settle_due_dark_bargain_missing_player_keeps_debt_active(
    repo_db_path, buff_repo, buff_service,
):
    import sqlite3

    from repositories.bankruptcy_repository import BankruptcyRepository
    from repositories.player_repository import PlayerRepository

    player_repo = PlayerRepository(repo_db_path)
    bankruptcy_repo = BankruptcyRepository(repo_db_path)
    missing_user = 999999
    buff_repo.grant(
        missing_user,
        TEST_GUILD_ID,
        BUFF_DARK_BARGAIN,
        int(time.time()) - 1,
        data={"amount_due": 700, "default_penalty": 1600, "default_penalty_games": 5},
    )

    settled = buff_service.settle_due_dark_bargains(
        player_repo=player_repo,
        bankruptcy_repo=bankruptcy_repo,
    )

    assert settled == [{
        "discord_id": missing_user,
        "guild_id": TEST_GUILD_ID,
        "status": "missing_player",
        "amount": 0,
    }]
    with sqlite3.connect(repo_db_path) as conn:
        triggered = conn.execute(
            """
            SELECT triggered FROM manashop_buffs
            WHERE discord_id = ? AND guild_id = ? AND buff_type = ?
            """,
            (missing_user, TEST_GUILD_ID, BUFF_DARK_BARGAIN),
        ).fetchone()[0]
    assert triggered == 0


def test_overgrowth_migration_backfills_missing_charges(repo_db_path, buff_repo):
    import sqlite3
    import time

    from infrastructure.schema_manager import SchemaManager
    from services.buff_service import BUFF_OVERGROWTH

    future = int(time.time()) + 3600
    buff_repo.grant(USER, TEST_GUILD_ID, BUFF_OVERGROWTH, future, data={})

    mgr = SchemaManager(repo_db_path)
    with sqlite3.connect(repo_db_path) as conn:
        cursor = conn.cursor()
        mgr._migration_backfill_overgrowth_charges_remaining(cursor)
        conn.commit()
        data_json = cursor.execute(
            "SELECT data FROM manashop_buffs WHERE discord_id = ? AND buff_type = ?",
            (USER, BUFF_OVERGROWTH),
        ).fetchone()[0]

    import json
    assert json.loads(data_json)["charges_remaining"] == 10


def test_overgrowth_active_for_user_only(buff_service):
    buff_service.grant_overgrowth(USER, TEST_GUILD_ID)
    assert buff_service.has_overgrowth(USER, TEST_GUILD_ID) is True
    assert buff_service.has_overgrowth(ALLY, TEST_GUILD_ID) is False


def test_overgrowth_grants_ten_dig_charges(buff_service, buff_repo):
    buff_service.grant_overgrowth(USER, TEST_GUILD_ID)

    active = buff_repo.active_for(USER, TEST_GUILD_ID, BUFF_OVERGROWTH)
    assert active[0]["data"]["charges_remaining"] == 10

    for _ in range(10):
        assert buff_service.consume_overgrowth_charge(USER, TEST_GUILD_ID) is True

    assert buff_service.consume_overgrowth_charge(USER, TEST_GUILD_ID) is False
    assert buff_service.has_overgrowth(USER, TEST_GUILD_ID) is False


def test_overgrowth_regrant_refreshes_not_extends(buff_service, buff_repo):
    """Re-granting overgrowth while active must leave exactly one row alive
    (the new one), so the timer resets to 12h rather than stacking."""
    buff_service.grant_overgrowth(USER, TEST_GUILD_ID)
    first_active = buff_repo.active_for(USER, TEST_GUILD_ID, BUFF_OVERGROWTH)
    assert len(first_active) == 1
    first_id = first_active[0]["id"]

    buff_service.grant_overgrowth(USER, TEST_GUILD_ID)
    second_active = buff_repo.active_for(USER, TEST_GUILD_ID, BUFF_OVERGROWTH)
    assert len(second_active) == 1
    assert second_active[0]["id"] != first_id


def test_overgrowth_regrant_collapses_pre_existing_duplicates(buff_service, buff_repo):
    """Models the post-race state: two active overgrowth rows already exist
    (e.g. from a concurrent re-purchase under an older non-atomic
    implementation). A subsequent grant must collapse them down to one
    rather than letting the older rows stay alive."""
    future = int(time.time()) + 12 * 3600
    leaked_a = buff_repo.grant(USER, TEST_GUILD_ID, BUFF_OVERGROWTH, future)
    leaked_b = buff_repo.grant(USER, TEST_GUILD_ID, BUFF_OVERGROWTH, future)
    assert len(buff_repo.active_for(USER, TEST_GUILD_ID, BUFF_OVERGROWTH)) == 2

    new_id = buff_service.grant_overgrowth(USER, TEST_GUILD_ID)
    surviving = buff_repo.active_for(USER, TEST_GUILD_ID, BUFF_OVERGROWTH)
    assert len(surviving) == 1
    assert surviving[0]["id"] == new_id
    assert new_id not in (leaked_a, leaked_b)


def test_cleanup_expired_prunes_old_rows(buff_service, buff_repo):
    # Manually grant a buff with a past expiry
    buff_repo.grant(
        USER, TEST_GUILD_ID, BUFF_COUNTERSPELL,
        expires_at=int(time.time()) - 10,
    )
    pruned = buff_service.cleanup_expired()
    assert pruned >= 1
    assert buff_service.has_pvp_immunity(USER, TEST_GUILD_ID) is False


# ──────────────────────────────────────────────────────────────────
# SlowDripRepository
# ──────────────────────────────────────────────────────────────────


def test_slow_drip_get_today_defaults(slow_drip_repo):
    state = slow_drip_repo.get_today(USER, TEST_GUILD_ID, "2026-05-09")
    assert state == {"claimed_today": 0, "last_claim_at": 0}


def test_slow_drip_add_claim_accumulates(slow_drip_repo):
    today = "2026-05-09"
    slow_drip_repo.add_claim(USER, TEST_GUILD_ID, today, 30)
    slow_drip_repo.add_claim(USER, TEST_GUILD_ID, today, 50)
    state = slow_drip_repo.get_today(USER, TEST_GUILD_ID, today)
    assert state["claimed_today"] == 80
    assert state["last_claim_at"] > 0


def test_slow_drip_per_day_isolation(slow_drip_repo):
    slow_drip_repo.add_claim(USER, TEST_GUILD_ID, "2026-05-09", 100)
    state_yesterday = slow_drip_repo.get_today(USER, TEST_GUILD_ID, "2026-05-09")
    state_today = slow_drip_repo.get_today(USER, TEST_GUILD_ID, "2026-05-10")
    assert state_yesterday["claimed_today"] == 100
    assert state_today["claimed_today"] == 0
