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


class TestSabotageBlockTipIsFundedNotMinted:
    """Every shield-block tip must come out of the attacker, never thin air.

    The three block branches (protection shield, manashop ward, aegis charge)
    each credited the victim while charging the attacker nothing. The ward
    branch is the worst: has_pvp_immunity is a pure read with no dedup, it
    returns before the 12h cooldown check, and /dig sabotage has no command
    cooldown — so an attacker could mint JC to a warded ally on every call.

    The tip is scaled by the dig reward multiplier before being capped at
    the attempt cost, so a block may burn the remainder. Burning matches
    the existing duplicate-block branch; minting was the defect.
    """

    @staticmethod
    def _setup(dig_service, dig_repo, player_repository, monkeypatch):
        _register(player_repository, 10001, balance=500)  # actor
        _register(player_repository, 10002, balance=500)  # target
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10002, TEST_GUILD_ID)
        dig_repo.update_tunnel(10002, TEST_GUILD_ID, depth=100)
        dig_service.protection_service = None

    def test_protection_shield_block_conserves_jc(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        self._setup(dig_service, dig_repo, player_repository, monkeypatch)
        dig_service.protection_service = SimpleNamespace(
            block_non_jc_attack=lambda victim_id, guild_id, *, actor_id, event_key: (
                SimpleNamespace(blocked=True, duplicate=False, source="counterspell")
            ),
        )
        actor_before = player_repository.get_balance(10001, TEST_GUILD_ID)
        target_before = player_repository.get_balance(10002, TEST_GUILD_ID)

        result = dig_service.sabotage_tunnel(10001, 10002, TEST_GUILD_ID)

        actor_delta = player_repository.get_balance(10001, TEST_GUILD_ID) - actor_before
        target_delta = player_repository.get_balance(10002, TEST_GUILD_ID) - target_before
        assert result["success"]
        assert actor_delta < 0, "the attacker must pay for the attempt"
        assert target_delta > 0, "the defender should still be tipped"
        assert target_delta <= -actor_delta, (
            f"tip not funded by the attacker: attacker {actor_delta}, "
            f"victim +{target_delta}"
        )
        assert actor_delta + target_delta <= 0, (
            f"JC minted: attacker {actor_delta}, victim +{target_delta}"
        )

    def test_manashop_ward_block_conserves_jc_on_every_call(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        self._setup(dig_service, dig_repo, player_repository, monkeypatch)
        dig_service.buff_service = SimpleNamespace(
            has_pvp_immunity=lambda discord_id, guild_id: True,
            consume_aegis_charge=lambda discord_id, guild_id: False,
        )
        actor_before = player_repository.get_balance(10001, TEST_GUILD_ID)
        target_before = player_repository.get_balance(10002, TEST_GUILD_ID)

        # Repeat it: this branch has no dedup and no cooldown in front of it.
        for _ in range(5):
            dig_service.sabotage_tunnel(10001, 10002, TEST_GUILD_ID)

        actor_delta = player_repository.get_balance(10001, TEST_GUILD_ID) - actor_before
        target_delta = player_repository.get_balance(10002, TEST_GUILD_ID) - target_before
        assert target_delta > 0, "the warded defender should still be tipped"
        assert target_delta <= -actor_delta, (
            f"tip not funded by the attacker: attacker {actor_delta}, "
            f"victim +{target_delta}"
        )
        assert actor_delta + target_delta <= 0, (
            f"JC minted over 5 spammed attempts: attacker {actor_delta}, "
            f"victim +{target_delta}"
        )

    def test_aegis_charge_block_conserves_jc(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        self._setup(dig_service, dig_repo, player_repository, monkeypatch)
        dig_service.buff_service = SimpleNamespace(
            has_pvp_immunity=lambda discord_id, guild_id: False,
            consume_aegis_charge=lambda discord_id, guild_id: True,
        )
        actor_before = player_repository.get_balance(10001, TEST_GUILD_ID)
        target_before = player_repository.get_balance(10002, TEST_GUILD_ID)

        result = dig_service.sabotage_tunnel(10001, 10002, TEST_GUILD_ID)

        actor_delta = player_repository.get_balance(10001, TEST_GUILD_ID) - actor_before
        target_delta = player_repository.get_balance(10002, TEST_GUILD_ID) - target_before
        assert result["success"]
        assert target_delta > 0, "the defender should still be tipped"
        assert target_delta <= -actor_delta, (
            f"tip not funded by the attacker: attacker {actor_delta}, "
            f"victim +{target_delta}"
        )
        assert actor_delta + target_delta <= 0, (
            f"JC minted: attacker {actor_delta}, victim +{target_delta}"
        )


class TestDigConsumablesCommitAtomically:
    """A dig that fails at its commit must not keep effects nor destroy items."""

    @staticmethod
    def _ready_digger(dig_service, dig_repo, player_repository, monkeypatch):
        _register(player_repository, 10001, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        dig_repo.update_tunnel(10001, TEST_GUILD_ID, depth=20)

    def test_failed_dig_keeps_neither_the_item_nor_its_charges(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """Charge grants used to be written before the commit that burns the item.

        Any later failure kept the charges and rolled the row deletion back, so
        the player held both the hard hat and its +3 charges, repeatably.
        """
        self._ready_digger(dig_service, dig_repo, player_repository, monkeypatch)
        assert dig_service.buy_item(10001, TEST_GUILD_ID, "hard_hat")["success"]
        item_row = dig_repo.get_inventory(10001, TEST_GUILD_ID)[0]
        dig_repo.queue_item(item_row["id"])

        charges_before = dig_repo.get_tunnel(10001, TEST_GUILD_ID)["hard_hat_charges"] or 0

        # Fail the dig at its final commit, after the grants were resolved.
        real_commit = dig_repo.atomic_tunnel_balance_update

        def boom(*args, **kwargs):
            raise RuntimeError("commit failed")

        dig_repo.atomic_tunnel_balance_update = boom
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 100_000)
        try:
            with pytest.raises(RuntimeError):
                dig_service.dig(10001, TEST_GUILD_ID)
        finally:
            dig_repo.atomic_tunnel_balance_update = real_commit

        tunnel = dig_repo.get_tunnel(10001, TEST_GUILD_ID)
        assert (tunnel["hard_hat_charges"] or 0) == charges_before, (
            "charges were granted despite the dig failing"
        )
        still_held = [
            i for i in dig_repo.get_inventory(10001, TEST_GUILD_ID)
            if i["item_type"] == "hard_hat"
        ]
        assert still_held, "the item was destroyed by a dig that never committed"

    def test_sonar_pulse_bought_while_one_is_pending_is_not_destroyed(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """Queuing a pulse while a skip is pending burned two charges for one skip."""
        self._ready_digger(dig_service, dig_repo, player_repository, monkeypatch)
        # A skip is already primed from an earlier pulse.
        dig_repo.update_tunnel(10001, TEST_GUILD_ID, sonar_skip_pending=1)

        assert dig_service.buy_item(10001, TEST_GUILD_ID, "sonar_pulse")["success"]
        pulse = [
            i for i in dig_repo.get_inventory(10001, TEST_GUILD_ID)
            if i["item_type"] == "sonar_pulse"
        ][0]
        dig_repo.queue_item(pulse["id"])

        # 0.15 sits above the shallow cave-in chance (Dirt 0.05 / Stone 0.10)
        # and below the ~0.22 event chance, so this dig rolls an event without
        # collapsing — a cave-in returns early and never reaches the skip.
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 100_000)
        monkeypatch.setattr(random, "random", lambda: 0.15)
        result = dig_service.dig(10001, TEST_GUILD_ID)

        assert result.get("sonar_skipped") is True, (
            "test is vacuous unless the pending skip was actually consumed"
        )
        tunnel = dig_repo.get_tunnel(10001, TEST_GUILD_ID)
        assert int(tunnel["sonar_skip_pending"] or 0) == 1, (
            "the pulse queued this dig was cleared by the skip it did not cause"
        )


class TestAbandonedDuelForfeit:
    """An abandoned paused duel forfeits its stake exactly once, up front.

    The wager is only charged when a duel resolves, so an unsettled row used to
    forgive it. Settling it introduced three sharper problems, all covered here:
    forfeiting the same stake twice, letting the player stake money the forfeit
    was about to take, and charging a pinnacle carry twice.
    """

    @staticmethod
    def _at_boss(dig_service, dig_repo, player_repository, monkeypatch, balance=1000):
        _register(player_repository, 10001, balance=balance)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        dig_repo.update_tunnel(10001, TEST_GUILD_ID, depth=BOSS_BOUNDARIES[0])

    @staticmethod
    def _park_stale_duel(dig_repo, wager, boss_id="grothak"):
        dig_repo.save_active_duel(10001, TEST_GUILD_ID, {
            "boss_id": boss_id, "tier": 25, "mechanic_id": "x",
            "risk_tier": "bold", "wager": wager,
            "player_hp": 10, "boss_hp": 10, "round_num": 1,
            "rng_state": "{}", "player_hit": 0.5, "player_dmg": 2,
            "boss_hit": 0.5, "boss_dmg": 2,
        })

    def test_stale_duel_is_claimed_atomically_not_read_then_cleared(self):
        """The abandoned row must be read-and-deleted in one step.

        Reading it, debiting the stake, then deleting it as a separate step
        lets two concurrent starts — or a resume racing a start — forfeit the
        same stake twice, and a process death in between re-forfeits it on the
        next /dig boss. That death is exactly the scenario this settles.

        Asserted structurally: the race needs real concurrency to observe, but
        using the atomic claim is what makes it impossible.
        """
        import inspect

        from services.dig.combat_mixin import BossCombatMixin

        source = inspect.getsource(BossCombatMixin.start_boss_duel)
        assert "claim_active_duel" in source, (
            "start_boss_duel must claim the paused duel atomically"
        )
        assert "get_active_duel" not in source, (
            "start_boss_duel still reads the paused duel non-atomically; the "
            "stake can be forfeited twice"
        )

    def test_forfeit_is_charged_before_the_new_wager_is_validated(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """Otherwise the follow-up fight is a free bet.

        Validating first let the player stake their whole balance while a
        forfeit was pending: the forfeit then drained them to zero, a loss
        clamps at zero, and a win still pays the full multiplier.
        """
        self._at_boss(dig_service, dig_repo, player_repository, monkeypatch, balance=1000)
        self._park_stale_duel(dig_repo, wager=1000)

        result = dig_service.start_boss_duel(10001, TEST_GUILD_ID, "bold", 1000)

        # Either the wager is refused for want of funds, or it was resized to
        # what remains after the forfeit — never the full pre-forfeit balance.
        wager = result.get("wager", 0) if isinstance(result, dict) else 0
        assert not result.get("success") or wager < 1000, (
            f"staked {wager} JC that the pending forfeit was about to consume"
        )

    def test_pinnacle_carry_is_not_charged_twice(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """Forfeiting the stake must drop the carry markers riding on it."""
        self._at_boss(dig_service, dig_repo, player_repository, monkeypatch, balance=5000)
        progress = {
            str(BOSS_BOUNDARIES[0]): {
                "boss_id": "grothak", "status": "phase1_defeated",
                "carried_wager": 500, "carried_risk_tier": "bold",
            }
        }
        dig_repo.update_tunnel(
            10001, TEST_GUILD_ID, boss_progress=json.dumps(progress),
        )
        self._park_stale_duel(dig_repo, wager=500)

        dig_service.start_boss_duel(10001, TEST_GUILD_ID, "bold", 0)

        entry = json.loads(
            dig_repo.get_tunnel(10001, TEST_GUILD_ID)["boss_progress"] or "{}"
        ).get(str(BOSS_BOUNDARIES[0]), {})
        assert not entry.get("carried_wager"), (
            "the forfeited stake is still carried and will be charged again"
        )


class TestConsumableChargeParityAcrossDigPaths:
    """The deterministic path must spend and grant charges like live dig().

    Charge grants are staged into the dig's atomic commit. The deterministic
    mirror never merged them (queued items were deleted with no effect) and
    still wrote its own spends immediately, so the staged pre-spend grant
    overwrote them and the absorb was free.
    """

    CHARGE_COLUMNS = (
        "hard_hat_charges", "grappling_hook_charges", "sonar_skip_pending",
        "reinforced_until", "void_bait_digs",
    )

    @staticmethod
    def _dig_with_item(dig_service, dig_repo, player_repository, monkeypatch,
                       discord_id, item, roll, deterministic):
        _register(player_repository, discord_id, balance=5000)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(discord_id, TEST_GUILD_ID)
        dig_repo.update_tunnel(discord_id, TEST_GUILD_ID, depth=20)
        assert dig_service.buy_item(discord_id, TEST_GUILD_ID, item)["success"]
        row = [
            i for i in dig_repo.get_inventory(discord_id, TEST_GUILD_ID)
            if i["item_type"] == item
        ][0]
        dig_repo.queue_item(row["id"])

        monkeypatch.setattr(time, "time", lambda: 1_200_000)
        monkeypatch.setattr(random, "random", lambda: roll)
        if deterministic:
            terminal, pre = dig_service.dig_with_preconditions(
                discord_id, TEST_GUILD_ID,
            )
            assert terminal is None, terminal
            dig_service._execute_deterministic_outcome(pre)
        else:
            dig_service.dig(discord_id, TEST_GUILD_ID)

        tunnel = dig_repo.get_tunnel(discord_id, TEST_GUILD_ID)
        return {c: tunnel[c] for c in TestConsumableChargeParityAcrossDigPaths.CHARGE_COLUMNS}

    @pytest.mark.parametrize(
        "item",
        ["reinforcement", "sonar_pulse", "grappling_hook", "void_bait", "hard_hat"],
    )
    @pytest.mark.parametrize(("roll", "label"), [(0.99, "no_cave_in"), (0.001, "cave_in")])
    def test_charge_columns_match_the_live_path(
        self, dig_service, dig_repo, player_repository, monkeypatch, item, roll, label,
    ):
        live = self._dig_with_item(
            dig_service, dig_repo, player_repository, monkeypatch,
            40001, item, roll, deterministic=False,
        )
        determ = self._dig_with_item(
            dig_service, dig_repo, player_repository, monkeypatch,
            40002, item, roll, deterministic=True,
        )
        assert live == determ, (
            f"{item} ({label}): deterministic path diverged from live dig() — "
            f"live={live} deterministic={determ}"
        )


class TestWardBlockDoesNotBurnTheSabotageCooldown:
    """A ward-blocked attempt must not lock the attacker out of the target.

    The ward branch returns before the 12h per-target cooldown check. Logging
    it as a plain "sabotage" meant every blocked attempt renewed the attacker's
    own lockout, so a cheap ward became permanent sabotage immunity — and it
    inflated the 7-day same-target count that drives the reveal.
    """

    def test_ward_block_leaves_the_target_still_sabotageable(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _register(player_repository, 10001, balance=500)
        _register(player_repository, 10002, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10002, TEST_GUILD_ID)
        dig_repo.update_tunnel(10002, TEST_GUILD_ID, depth=100)
        dig_service.protection_service = None

        warded = {"on": True}
        dig_service.buff_service = SimpleNamespace(
            has_pvp_immunity=lambda discord_id, guild_id: warded["on"],
            consume_aegis_charge=lambda discord_id, guild_id: False,
        )

        blocked = dig_service.sabotage_tunnel(10001, 10002, TEST_GUILD_ID)
        assert blocked["success"] is False

        # The ward drops; the attacker must not be locked out by their own
        # blocked attempt.
        warded["on"] = False
        follow_up = dig_service.sabotage_tunnel(10001, 10002, TEST_GUILD_ID)

        assert "last 12 hours" not in (follow_up.get("error") or ""), (
            "a ward-blocked attempt burned the attacker's 12h cooldown"
        )

    def test_ward_block_is_still_audited(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """Excluding it from the cooldown scan must not make it invisible."""
        _register(player_repository, 10001, balance=500)
        _register(player_repository, 10002, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10002, TEST_GUILD_ID)
        dig_repo.update_tunnel(10002, TEST_GUILD_ID, depth=100)
        dig_service.protection_service = None
        dig_service.buff_service = SimpleNamespace(
            has_pvp_immunity=lambda discord_id, guild_id: True,
            consume_aegis_charge=lambda discord_id, guild_id: False,
        )

        dig_service.sabotage_tunnel(10001, 10002, TEST_GUILD_ID)

        logged = dig_repo.get_recent_actions(
            10001, TEST_GUILD_ID, action_type="sabotage_ward_blocked", hours=1,
        )
        assert logged, "the blocked attempt must still be recorded"
        detail = json.loads(logged[0].get("detail") or "{}")
        assert detail.get("target_id") == 10002
        assert detail.get("protection_source") == "ward"
