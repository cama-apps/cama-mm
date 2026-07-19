"""Regression tests for the 2026-07c dig sweep fixes.

Covers: the prestige conditional claim, the daily economy event applying to
the main dig payout, abandon clearing pinnacle state, the fallback/DM dig
results reporting ``dig_consumed``, ``_ensure_boss_locked`` merging instead
of replacing the boss_progress entry, the repeat-blocked sabotage attempt
being charged and logged, and the lantern-stub daily restore being a no-op
on a same-day retry.
"""

from __future__ import annotations

import json
import random
import time
from types import SimpleNamespace

import pytest

from repositories.dig_repository import DigRepository
from services.dig_constants import (
    BOSS_BOUNDARIES,
    PINNACLE_DEPTH,
    PRESTIGE_PERKS,
)
from services.dig_data.balance import scale_positive_dig_jc
from services.dig_service import DigService
from tests.conftest import TEST_GUILD_ID


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def dig_service(dig_repo, player_repository, monkeypatch):
    svc = DigService(dig_repo, player_repository)
    monkeypatch.setattr(svc, "_get_weather_effects", lambda guild_id, layer_name: {})
    return svc


def _register(player_repo, discord_id, balance=2000):
    player_repo.add(
        discord_id=discord_id,
        discord_username=f"User{discord_id}",
        guild_id=TEST_GUILD_ID,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    player_repo.update_balance(discord_id, TEST_GUILD_ID, balance)


def _first_dig(dig_service, monkeypatch, discord_id):
    monkeypatch.setattr(time, "time", lambda: 1_000_000)
    monkeypatch.setattr(random, "random", lambda: 0.99)
    dig_service.dig(discord_id, TEST_GUILD_ID)


class TestPrestigeAtomicClaim:
    """Two prestige calls racing past can_prestige must credit exactly once."""

    def test_double_prestige_credits_once(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _register(player_repository, 10001)
        _first_dig(dig_service, monkeypatch, 10001)
        bp = {str(b): "defeated" for b in BOSS_BOUNDARIES}
        bp[str(PINNACLE_DEPTH)] = {"status": "defeated", "boss_id": "forgotten_king"}
        dig_repo.update_tunnel(
            10001, TEST_GUILD_ID,
            depth=PINNACLE_DEPTH,
            boss_progress=json.dumps(bp),
            prestige_level=0,
        )
        real_get_tunnel = dig_repo.get_tunnel
        pre_tunnel = dict(real_get_tunnel(10001, TEST_GUILD_ID))
        balance_before = player_repository.get_balance(10001, TEST_GUILD_ID)
        perk = PRESTIGE_PERKS[0]

        first = dig_service.prestige(10001, TEST_GUILD_ID, perk)
        assert first["success"] is True

        relic_count_after_first = len(
            dig_repo.get_artifacts(10001, TEST_GUILD_ID) or []
        )

        # Drive the race: the second caller re-reads the PRE-reset tunnel
        # (exactly what a concurrent call that passed can_prestige before the
        # first reset committed would see) while the DB already holds the
        # reset. The conditional claim must reject it and roll back cleanly.
        monkeypatch.setattr(
            dig_repo, "get_tunnel", lambda d, g: dict(pre_tunnel),
        )
        second = dig_service.prestige(10001, TEST_GUILD_ID, perk)

        assert second["success"] is False
        balance_after = player_repository.get_balance(10001, TEST_GUILD_ID)
        # Exactly one grant: scale_positive_dig_jc(1000) == 650 JC.
        assert balance_after - balance_before == scale_positive_dig_jc(1000) == 650
        # Exactly one granted relic — the loser's roll rolled back.
        assert len(
            dig_repo.get_artifacts(10001, TEST_GUILD_ID) or []
        ) == relic_count_after_first
        assert real_get_tunnel(10001, TEST_GUILD_ID)["prestige_level"] == 1


class TestDailyEconomyEventOnDig:
    """The daily economy event applies to the main dig payout exactly once.

    The event is applied inside ``_apply_mana_yield_taxes`` (which every dig
    payout path runs); this pins both that it fires and that no second
    application sneaks in elsewhere in the flow.
    """

    def test_regular_dig_payout_reflects_the_multiplier_once(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _register(player_repository, 10001)
        calls: list[tuple] = []

        class _Recording:
            def adjust_reward(self, guild_id, amount):
                calls.append((guild_id, amount))
                return int(amount * 2)

        dig_service.economy_event_service = _Recording()
        monkeypatch.setattr(random, "randint", lambda a, b: b)
        _first_dig(dig_service, monkeypatch, 10001)
        calls.clear()

        monkeypatch.setattr(time, "time", lambda: 1_100_000)  # past cooldown
        balance_before = player_repository.get_balance(10001, TEST_GUILD_ID)
        result = dig_service.dig(10001, TEST_GUILD_ID)

        assert result["success"]
        assert result["cave_in"] is False
        assert len(calls) == 1
        base_amount = calls[0][1]
        assert base_amount > 0
        # The event sees the scaled payout and its result is what gets paid.
        assert base_amount == scale_positive_dig_jc(result["gross_jc"])
        assert result["jc_earned"] == 2 * base_amount
        assert (
            player_repository.get_balance(10001, TEST_GUILD_ID) - balance_before
            == result["jc_earned"]
        )


class TestAbandonResetsPinnacleState:
    """Abandoning mid-pinnacle must not leave a resumable stored phase."""

    def test_abandon_clears_pinnacle_state(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _register(player_repository, 10001)
        _first_dig(dig_service, monkeypatch, 10001)
        dig_repo.update_tunnel(
            10001, TEST_GUILD_ID,
            depth=PINNACLE_DEPTH - 1,
            pinnacle_boss_id="forgotten_king",
            pinnacle_phase=2,
        )
        balance_before = player_repository.get_balance(10001, TEST_GUILD_ID)

        result = dig_service.abandon_tunnel(10001, TEST_GUILD_ID)

        assert result["success"]
        # Refund = scaled 10% of depth, credited exactly once.
        expected_refund = scale_positive_dig_jc(int((PINNACLE_DEPTH - 1) * 0.1))
        assert result["refund"] == expected_refund
        assert (
            player_repository.get_balance(10001, TEST_GUILD_ID) - balance_before
            == expected_refund
        )
        tunnel = dig_repo.get_tunnel(10001, TEST_GUILD_ID)
        assert tunnel["pinnacle_phase"] == 0
        assert tunnel["pinnacle_boss_id"] is None


class TestFallbackDigConsumedFlag:
    """Fallback/DM dig results must report dig_consumed like the main path."""

    def test_deterministic_fallback_dig_reports_consumed(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _register(player_repository, 10001)
        _first_dig(dig_service, monkeypatch, 10001)
        monkeypatch.setattr(time, "time", lambda: 1_100_000)

        terminal, pre = dig_service.dig_with_preconditions(10001, TEST_GUILD_ID)
        assert terminal is None
        result = dig_service._execute_deterministic_outcome(pre)

        assert result["success"]
        assert result["dig_consumed"] is True

    def test_dm_outcome_dig_reports_consumed(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _register(player_repository, 10002)
        _first_dig(dig_service, monkeypatch, 10002)
        monkeypatch.setattr(time, "time", lambda: 1_100_000)

        terminal, pre = dig_service.dig_with_preconditions(10002, TEST_GUILD_ID)
        assert terminal is None
        result = dig_service.apply_dig_outcome(
            pre, {"advance": 2, "jc_earned": 3, "cave_in": False},
        )

        assert result["success"]
        assert result["dig_consumed"] is True

    def test_dm_cave_in_dig_reports_consumed(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _register(player_repository, 10003)
        _first_dig(dig_service, monkeypatch, 10003)
        monkeypatch.setattr(time, "time", lambda: 1_100_000)

        terminal, pre = dig_service.dig_with_preconditions(10003, TEST_GUILD_ID)
        assert terminal is None
        result = dig_service.apply_dig_outcome(
            pre,
            {
                "advance": 0,
                "jc_earned": 0,
                "cave_in": True,
                "cave_in_block_loss": 2,
                "cave_in_type": "minor",
                "cave_in_jc_lost": 0,
            },
        )

        assert result["success"]
        assert result["cave_in"] is True
        assert result["dig_consumed"] is True


class TestEnsureBossLockedMerge:
    """Locking a boss must merge into the entry, not replace it."""

    def test_lock_preserves_active_prep(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _register(player_repository, 10001)
        _first_dig(dig_service, monkeypatch, 10001)
        bp = {str(b): "active" for b in BOSS_BOUNDARIES}
        bp["25"] = {
            "status": "active",
            "active_prep": {"item_type": "tempered_whetstone", "used": False},
        }
        dig_repo.update_tunnel(
            10001, TEST_GUILD_ID, depth=24, boss_progress=json.dumps(bp),
        )
        tunnel = dict(dig_repo.get_tunnel(10001, TEST_GUILD_ID))

        boss = dig_service._ensure_boss_locked(10001, TEST_GUILD_ID, tunnel, 25)

        assert boss is not None
        persisted = json.loads(
            dig_repo.get_tunnel(10001, TEST_GUILD_ID)["boss_progress"]
        )
        entry = persisted["25"]
        assert entry["boss_id"] == boss.boss_id
        assert entry["status"] == "active"
        # The just-persisted prep must survive the lock write.
        assert entry["active_prep"] == {
            "item_type": "tempered_whetstone", "used": False,
        }


class TestSabotageDuplicateBlockCharge:
    """A repeat blocked attempt inside the shield window costs and is logged."""

    def test_repeat_blocked_attempt_costs_and_logs(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _register(player_repository, 10001, balance=500)  # actor
        _register(player_repository, 10002, balance=500)  # target
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10002, TEST_GUILD_ID)
        dig_repo.update_tunnel(10002, TEST_GUILD_ID, depth=100)

        dig_service.protection_service = SimpleNamespace(
            block_non_jc_attack=lambda victim_id, guild_id, *, actor_id, event_key: (
                SimpleNamespace(blocked=True, duplicate=True, source="counterspell")
            ),
        )
        actor_before = player_repository.get_balance(10001, TEST_GUILD_ID)
        target_before = player_repository.get_balance(10002, TEST_GUILD_ID)

        result = dig_service.sabotage_tunnel(10001, 10002, TEST_GUILD_ID)

        assert result["success"]
        assert result["cost"] == 20  # max(5, depth 100 // 5)
        assert result["victim_tip"] == 0
        assert (
            actor_before - player_repository.get_balance(10001, TEST_GUILD_ID)
            == 20
        )
        # No tip on the duplicate — the victim balance is untouched.
        assert player_repository.get_balance(10002, TEST_GUILD_ID) == target_before
        logs = dig_repo.get_recent_actions(
            10001, TEST_GUILD_ID, action_type="sabotage", hours=1,
        )
        assert logs
        detail = json.loads(logs[0].get("detail") or "{}")
        assert detail.get("duplicate_block") is True
        assert detail.get("cost") == 20


class TestLanternStubDailyRestoreIdempotent:
    """The daily +5 restore must not re-apply on a same-day retry."""

    def test_restore_is_a_noop_on_same_day_retry(
        self, dig_service, dig_repo, player_repository,
    ):
        _register(player_repository, 10001)
        dig_repo.create_tunnel(10001, TEST_GUILD_ID, "Stub Tunnel")
        dig_repo.update_tunnel(10001, TEST_GUILD_ID, luminosity=50)
        row_id = dig_repo.add_artifact(
            10001, TEST_GUILD_ID, "lantern_stub", is_relic=True,
        )
        dig_repo.equip_relic(row_id, 10001, TEST_GUILD_ID, True)
        dig_service._invalidate_relic_cache(10001, TEST_GUILD_ID)

        tunnel = dict(dig_repo.get_tunnel(10001, TEST_GUILD_ID))
        tunnel["last_dig_at"] = None
        lum_info = {"luminosity_after": 50, "drained": 0}
        first = dig_service._apply_lantern_stub_restore(
            10001, TEST_GUILD_ID, tunnel, lum_info, "2026-07-19",
        )
        assert first == 55
        row = dig_repo.get_tunnel(10001, TEST_GUILD_ID)
        assert row["lantern_stub_date"] == "2026-07-19"
        assert row["luminosity"] == 55

        # Retry after a failed dig: last_dig_at is still stale, but the
        # stamp closes the gate — no second +5.
        tunnel2 = dict(dig_repo.get_tunnel(10001, TEST_GUILD_ID))
        tunnel2["last_dig_at"] = None
        lum_info2 = {"luminosity_after": 55, "drained": 0}
        second = dig_service._apply_lantern_stub_restore(
            10001, TEST_GUILD_ID, tunnel2, lum_info2, "2026-07-19",
        )
        assert second == 55
        assert "lantern_stub_restored" not in lum_info2
        assert dig_repo.get_tunnel(10001, TEST_GUILD_ID)["luminosity"] == 55
