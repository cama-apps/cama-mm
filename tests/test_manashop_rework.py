"""Tests for the manashop rework: tap-mana, daily-uses, buffs, and new effects."""

from __future__ import annotations

import time

import pytest

from repositories.buff_repository import BuffRepository
from repositories.mana_repository import ManaRepository
from repositories.slow_drip_repository import SlowDripRepository
from services.buff_service import (
    BUFF_COUNTERSPELL,
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
    # Update the skim total — should round-trip via repo.
    buff_service.record_blood_pact_skim(pact["id"], pact["data"], 25)
    refreshed = buff_service.get_blood_pact_skimmer(TARGET, TEST_GUILD_ID)
    assert refreshed["data"]["skimmed_total"] == 25


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
    buff_service.grant_dark_bargain_debt(USER, TEST_GUILD_ID, amount_due=800)
    debt = buff_service.has_dark_bargain_debt(USER, TEST_GUILD_ID)
    assert debt is not None
    assert debt["data"]["amount_due"] == 800


def test_overgrowth_active_for_user_only(buff_service):
    buff_service.grant_overgrowth(USER, TEST_GUILD_ID)
    assert buff_service.has_overgrowth(USER, TEST_GUILD_ID) is True
    assert buff_service.has_overgrowth(ALLY, TEST_GUILD_ID) is False


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
