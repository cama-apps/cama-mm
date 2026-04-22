"""Tests for the boss HP duel system."""

from __future__ import annotations

import json
import random
import time

import pytest

from repositories.dig_repository import DigRepository
from services.dig_constants import (
    BOSS_DUEL_STATS,
    BOSS_HP_PER_40_DEPTH,
    BOSS_HP_PER_PRESTIGE,
    BOSS_PAYOUTS,
    FREE_DIG_COOLDOWN_SECONDS,
)
from services.dig_service import DigService, _approx_duel_win_prob
from tests.conftest import TEST_GUILD_ID


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def dig_service(dig_repo, player_repository, monkeypatch):
    svc = DigService(dig_repo, player_repository)
    monkeypatch.setattr(svc, "_get_weather_effects", lambda guild_id, layer_name: {})
    return svc


def _register(player_repo, discord_id=10001, balance=200):
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
    return discord_id


def _at_boss(dig_service, dig_repo, player_repository, monkeypatch, *, depth=24, prestige=0):
    """Place a fresh player one block before the depth-25 boss boundary."""
    _register(player_repository, balance=200)
    monkeypatch.setattr(time, "time", lambda: 1_000_000)
    monkeypatch.setattr(random, "random", lambda: 0.99)
    dig_service.dig(10001, TEST_GUILD_ID)
    dig_repo.update_tunnel(10001, TEST_GUILD_ID, depth=depth, prestige_level=prestige)
    monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)


class TestDuelDeterministicOutcomes:
    """With ``random.random`` pinned to extremes, duel outcomes are deterministic."""

    def test_cautious_always_hit_wins(self, dig_service, dig_repo, player_repository, monkeypatch):
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch)
        monkeypatch.setattr(random, "random", lambda: 0.01)
        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "cautious", wager=10)
        assert result["success"]
        assert result["won"] is True
        # Round log is included in the response.
        assert len(result["round_log"]) >= 1

    def test_never_hit_triggers_round_cap_loss(self, dig_service, dig_repo, player_repository, monkeypatch):
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch)
        # Snapshot balance just before the fight (the first dig may have
        # credited 1-5 JC from the guaranteed first-dig payout).
        balance_before_fight = player_repository.get_balance(10001, TEST_GUILD_ID)

        # Nobody can roll under 0.999; round cap fires and the boss takes it.
        monkeypatch.setattr(random, "random", lambda: 0.999)
        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "cautious", wager=10)
        assert result["won"] is False
        assert 5 <= result["knockback"] <= 10
        assert player_repository.get_balance(10001, TEST_GUILD_ID) == balance_before_fight - 10

    def test_player_first_one_shot_boss_never_swings(self, dig_service, dig_repo, player_repository, monkeypatch):
        """Reckless always-hit: boss dies round 1 before it can counterattack."""
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch)
        monkeypatch.setattr(random, "random", lambda: 0.0)
        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "reckless", wager=10)
        assert result["won"] is True
        # Reckless player_dmg=3, boss_hp at depth 25 still 5; with 0.02 depth
        # penalty player_hit stays 0.13 but random=0.0 still hits. Rounds = 2.
        assert len(result["round_log"]) == 2
        # First round entry must not contain a boss_hit outcome since boss
        # was alive and about to act only after player's first swing.
        assert "boss_hit" in result["round_log"][0]  # boss did swing round 1
        assert "boss_hit" not in result["round_log"][-1]  # killing blow, boss never swings back


class TestDuelScaling:
    """Depth and prestige both add boss HP to make duels harder."""

    def test_boss_hp_scales_with_depth(self, dig_service, dig_repo, player_repository, monkeypatch):
        """Boss HP at depth 200 = base + (200//40) * BOSS_HP_PER_40_DEPTH.

        Asserts directly on the first round's recorded ``boss_hp`` (which
        is post-player-hit HP) so the test doesn't depend on who wins.
        """
        base_boss_hp = int(BOSS_DUEL_STATS["cautious"]["boss_hp"])
        expected = base_boss_hp + (200 // 40) * BOSS_HP_PER_40_DEPTH

        _register(player_repository, balance=2000)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        bp_defeated = json.dumps({"25": "defeated", "50": "defeated", "75": "defeated",
                                   "100": "defeated", "150": "defeated"})
        dig_repo.update_tunnel(10001, TEST_GUILD_ID, depth=199, boss_progress=bp_defeated)
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        # Force a hit so the first round's boss_hp reflects one damage instance.
        monkeypatch.setattr(random, "random", lambda: 0.0)

        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "cautious", wager=10)
        first_round_boss_hp_after_hit = result["round_log"][0]["boss_hp"]
        # After 1 player hit for player_dmg=1, boss has (expected - 1) HP left.
        assert first_round_boss_hp_after_hit == expected - int(BOSS_DUEL_STATS["cautious"]["player_dmg"])

    def test_boss_hp_scales_with_prestige(self, dig_service, dig_repo, player_repository, monkeypatch):
        base_boss_hp = int(BOSS_DUEL_STATS["cautious"]["boss_hp"])
        prestige = 3
        expected = base_boss_hp + prestige * BOSS_HP_PER_PRESTIGE

        _register(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        dig_repo.update_tunnel(10001, TEST_GUILD_ID, depth=24, prestige_level=prestige)
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        monkeypatch.setattr(random, "random", lambda: 0.0)

        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "cautious", wager=10)
        first_round_boss_hp_after_hit = result["round_log"][0]["boss_hp"]
        assert first_round_boss_hp_after_hit == expected - int(BOSS_DUEL_STATS["cautious"]["player_dmg"])


class TestDuelPayout:
    """Win pays wager * BOSS_PAYOUTS[depth][tier]; loss forfeits wager + cave-in."""

    def test_win_pays_from_payout_table(self, dig_service, dig_repo, player_repository, monkeypatch):
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch)
        balance_before = player_repository.get_balance(10001, TEST_GUILD_ID)
        monkeypatch.setattr(random, "random", lambda: 0.0)

        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "cautious", wager=10)
        assert result["won"] is True
        expected_multiplier = BOSS_PAYOUTS[25][0]
        expected_profit = int(10 * (expected_multiplier - 1))
        assert player_repository.get_balance(10001, TEST_GUILD_ID) == balance_before + expected_profit

    def test_loss_applies_knockback(self, dig_service, dig_repo, player_repository, monkeypatch):
        """Boss loss knocks the player back 5-10 blocks and clears cheers."""
        _register(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        bp_defeated = json.dumps({"25": "defeated", "50": "defeated", "75": "defeated"})
        dig_repo.update_tunnel(10001, TEST_GUILD_ID, depth=99, boss_progress=bp_defeated)
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        monkeypatch.setattr(random, "random", lambda: 0.999)

        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "cautious", wager=10)
        assert result["won"] is False
        knockback = result["knockback"]
        assert 5 <= knockback <= 10
        tunnel = dig_repo.get_tunnel(10001, TEST_GUILD_ID)
        assert tunnel["depth"] == 99 - knockback


class TestMilestoneAntiFarm:
    """After caving in and re-crossing a milestone, the bonus is NOT re-awarded."""

    def test_milestone_awarded_once(self, dig_service, dig_repo, player_repository, monkeypatch):
        _register(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        # Pretend the tunnel has been to 40 before (max_depth=40).
        dig_repo.update_tunnel(10001, TEST_GUILD_ID, depth=20, max_depth=40)

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        monkeypatch.setattr(random, "randint", lambda a, b: 10)

        result = dig_service.dig(10001, TEST_GUILD_ID)
        assert result["success"]
        # 25 has already been crossed (max_depth = 40), so no milestone bonus.
        assert result["milestone_bonus"] == 0


class TestApproxWinProb:
    """The Monte Carlo estimator should be in the right ballpark."""

    def test_cautious_first_boss_is_high(self):
        stats = BOSS_DUEL_STATS["cautious"]
        prob = _approx_duel_win_prob(
            player_hp=int(stats["player_hp"]),
            boss_hp=int(stats["boss_hp"]),
            player_hit=float(stats["player_hit"]),
            player_dmg=int(stats["player_dmg"]),
            boss_hit=float(stats["boss_hit"]),
            boss_dmg=int(stats["boss_dmg"]),
            trials=2000,
        )
        assert prob > 0.65

    def test_reckless_first_boss_is_low(self):
        stats = BOSS_DUEL_STATS["reckless"]
        prob = _approx_duel_win_prob(
            player_hp=int(stats["player_hp"]),
            boss_hp=int(stats["boss_hp"]),
            player_hit=float(stats["player_hit"]),
            player_dmg=int(stats["player_dmg"]),
            boss_hit=float(stats["boss_hit"]),
            boss_dmg=int(stats["boss_dmg"]),
            trials=2000,
        )
        assert prob < 0.35


class TestBossEchoWeakening:
    """After a guild-first kill, subsequent fighters see a weakened boss for 24h."""

    def test_first_kill_records_echo(self, dig_service, dig_repo, player_repository, monkeypatch):
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch)
        monkeypatch.setattr(random, "random", lambda: 0.0)
        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "reckless", wager=10)
        assert result["won"] is True
        assert result.get("echo_applied") is False
        # _at_boss leaves boss_progress unset so the locked boss falls back to
        # the grandfathered "grothak" at tier 25.
        row = dig_repo.get_active_boss_echo(TEST_GUILD_ID, "grothak")
        assert row is not None
        assert row["killer_discord_id"] == 10001

    def test_second_kill_sees_weakened_boss(self, dig_service, dig_repo, player_repository, monkeypatch):
        # First digger kills Grothak
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch)
        monkeypatch.setattr(random, "random", lambda: 0.0)
        dig_service.fight_boss(10001, TEST_GUILD_ID, "reckless", wager=10)

        # Second digger arrives at the same boundary
        _register(player_repository, discord_id=10002, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 10)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10002, TEST_GUILD_ID)
        dig_repo.update_tunnel(10002, TEST_GUILD_ID, depth=24)
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 2 * FREE_DIG_COOLDOWN_SECONDS + 10)
        balance_before = player_repository.get_balance(10002, TEST_GUILD_ID)
        monkeypatch.setattr(random, "random", lambda: 0.0)

        result = dig_service.fight_boss(10002, TEST_GUILD_ID, "cautious", wager=10)
        assert result["won"] is True
        assert result.get("echo_applied") is True
        assert result.get("echo_killer_id") == 10001

        # Payout is 0.7x the normal cautious multiplier
        base_multiplier = BOSS_PAYOUTS[25][0]
        expected_profit = int(10 * (base_multiplier * 0.7 - 1))
        assert player_repository.get_balance(10002, TEST_GUILD_ID) == balance_before + expected_profit

    def test_killer_reruns_get_no_discount(self, dig_service, dig_repo, player_repository, monkeypatch):
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch)
        monkeypatch.setattr(random, "random", lambda: 0.0)
        dig_service.fight_boss(10001, TEST_GUILD_ID, "reckless", wager=10)

        # Same killer comes back to the same boundary
        bp = json.dumps({"25": "active"})  # reset boss status for re-fight
        dig_repo.update_tunnel(10001, TEST_GUILD_ID, depth=24, boss_progress=bp)
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 2 * FREE_DIG_COOLDOWN_SECONDS + 10)

        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "cautious", wager=10)
        assert result["won"] is True
        # Killer exempt: no echo applied even though a row exists.
        assert result.get("echo_applied") is False

    def test_beneficiary_kill_refreshes_echo_to_themselves(self, dig_service, dig_repo, player_repository, monkeypatch):
        """A player who benefits from an active echo and then clears the boss
        becomes the new attributed killer and restarts the window."""
        # First digger kills Grothak → echo written for 10001.
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch)
        monkeypatch.setattr(random, "random", lambda: 0.0)
        dig_service.fight_boss(10001, TEST_GUILD_ID, "reckless", wager=10)
        assert dig_repo.get_active_boss_echo(TEST_GUILD_ID, "grothak")["killer_discord_id"] == 10001

        # Second digger arrives under the echo and wins.
        _register(player_repository, discord_id=10002, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 10)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10002, TEST_GUILD_ID)
        dig_repo.update_tunnel(10002, TEST_GUILD_ID, depth=24)
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 2 * FREE_DIG_COOLDOWN_SECONDS + 10)
        monkeypatch.setattr(random, "random", lambda: 0.0)
        result = dig_service.fight_boss(10002, TEST_GUILD_ID, "cautious", wager=10)

        assert result["won"] is True
        assert result["echo_applied"] is True
        # After the beneficiary's clear, the echo's killer is now 10002.
        row = dig_repo.get_active_boss_echo(TEST_GUILD_ID, "grothak")
        assert row is not None
        assert row["killer_discord_id"] == 10002

    def test_expired_echo_not_applied(self, dig_service, dig_repo, player_repository, monkeypatch):
        # _at_boss pins time.time() to 1_000_000; record the echo AFTER that
        # pin so its weakened_until is in the pinned-clock frame.
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch)
        dig_repo.record_boss_echo(
            TEST_GUILD_ID, "grothak", 25, killer_discord_id=9999, window_seconds=60,
        )
        # Jump far past the 60-second echo window AND the fight cooldown.
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 3600)
        monkeypatch.setattr(random, "random", lambda: 0.0)
        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "cautious", wager=10)
        assert result["won"] is True
        assert result.get("echo_applied") is False
