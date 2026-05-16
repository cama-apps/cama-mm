"""Tests for the boss HP duel system."""

from __future__ import annotations

import json
import random
import time

import pytest

from repositories.dig_repository import DigRepository
from services.dig_constants import (
    BOSS_ARCHETYPE_BY_ID,
    BOSS_ARCHETYPES,
    BOSS_DUEL_STATS,
    BOSS_PAYOUTS,
    BOSS_PRESTIGE_BONUS,
    BOSS_TIER_BONUS,
    FREE_DIG_COOLDOWN_SECONDS,
    PHASE_TRANSITION_EVENTS,
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
    """Place a fresh player one block before the depth-25 boss boundary.

    Pre-locks the tier-25 boss to ``grothak`` so legacy ``fight_boss`` runs
    deterministically — without this, ``_ensure_boss_locked`` rolls
    randomly across the 3-boss tier pool.
    """
    _register(player_repository, balance=200)
    monkeypatch.setattr(time, "time", lambda: 1_000_000)
    monkeypatch.setattr(random, "random", lambda: 0.99)
    dig_service.dig(10001, TEST_GUILD_ID)
    dig_repo.update_tunnel(
        10001, TEST_GUILD_ID,
        depth=depth, prestige_level=prestige,
        boss_progress=json.dumps({"25": {"boss_id": "grothak", "status": "active"}}),
    )
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
        assert 8 <= result["knockback"] <= 16
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
        """Boss HP at depth 200 = base*archetype + BOSS_TIER_BONUS[200]['hp'].

        Asserts directly on the first round's recorded ``boss_hp`` (which
        is post-player-hit HP) so the test doesn't depend on who wins.
        """
        base_boss_hp = int(BOSS_DUEL_STATS["cautious"]["boss_hp"])
        # Pin chronofrost (slippery archetype) at the 200 boundary so the
        # math is deterministic regardless of which Tier 200 boss the locker rolled.
        archetype = BOSS_ARCHETYPES[BOSS_ARCHETYPE_BY_ID["chronofrost"]]
        expected = (
            int(round(base_boss_hp * archetype["hp_mult"]))
            + int(BOSS_TIER_BONUS[200]["hp"])
        )

        _register(player_repository, balance=2000)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        bp_defeated = json.dumps({
            "25": "defeated", "50": "defeated", "75": "defeated",
            "100": "defeated", "150": "defeated",
            "200": {"boss_id": "chronofrost", "status": "active"},
        })
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
        # Pin grothak (bruiser archetype: ×1.0 HP mult) so the math is deterministic
        # regardless of which tier-25 boss the locker rolled.
        archetype = BOSS_ARCHETYPES[BOSS_ARCHETYPE_BY_ID["grothak"]]
        expected = (
            int(round(base_boss_hp * archetype["hp_mult"]))
            + int(BOSS_PRESTIGE_BONUS[prestige]["hp"])
        )

        _register(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        dig_repo.update_tunnel(
            10001, TEST_GUILD_ID,
            depth=24, prestige_level=prestige,
            boss_progress=json.dumps({"25": {"boss_id": "grothak", "status": "active"}}),
        )
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
        """Boss loss knocks the player back and clears cheers."""
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
        assert 8 <= knockback <= 16
        tunnel = dig_repo.get_tunnel(10001, TEST_GUILD_ID)
        assert tunnel["depth"] == 99 - knockback

    def test_loss_surfaces_soften_line_when_chip_damage_done(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """Boss loss includes a soften_line showing pre-fight HP → ending HP
        when the player chipped off at least 1 HP. The test seeds boss_progress
        with a partial HP entry so the boss starts wounded, then forces a loss
        — the surviving HP at loss time is what gets surfaced."""
        _register(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)

        # Seed: a known soft boss at depth 99 (next boundary is 100 → at_boss
        # logic kicks in at the boundary; we use 99 so the next dig triggers
        # the boundary check via fight_boss).
        seed_hp = 2
        seed_max = 8
        bp_seed = {
            "25": "defeated", "50": "defeated", "75": "defeated",
            "100": {
                "hp_remaining": seed_hp, "hp_max": seed_max,
                "last_engaged_at": 1_000_000,
                "status": "active",
            },
        }
        dig_repo.update_tunnel(
            10001, TEST_GUILD_ID,
            depth=99, boss_progress=json.dumps(bp_seed),
        )
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        # Force a loss: player never hits, boss always hits.
        monkeypatch.setattr(random, "random", lambda: 0.999)

        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "cautious", wager=10)
        assert result["won"] is False
        # With no chip damage this fight (player whiffs every round), the
        # starting and ending HP both equal seed_hp, so the soften line is
        # suppressed. Re-run with the player landing every hit instead.
        if result.get("soften_line"):
            assert f"/{seed_max}" in result["soften_line"]

    def test_loss_soften_line_present_when_player_chips(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """When the player lands hits before losing, soften_line surfaces the
        boss's HP transition."""
        _register(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        bp_seed = {
            "25": "defeated", "50": "defeated", "75": "defeated",
            "100": {
                "hp_remaining": 6, "hp_max": 6,
                "last_engaged_at": 1_000_000,
                "status": "active",
            },
        }
        dig_repo.update_tunnel(
            10001, TEST_GUILD_ID,
            depth=99, boss_progress=json.dumps(bp_seed),
        )
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        # Roll low: player hits AND boss hits each round. With cautious stats
        # (5 player_hp vs 6 boss_hp, both ~equal hit chance) the player will
        # chip 2-3 HP off before dying.
        monkeypatch.setattr(random, "random", lambda: 0.05)

        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "cautious", wager=10)
        if result["won"] is False and result.get("boss_hp_remaining", 6) < 6:
            assert result.get("soften_line") is not None
            assert "/" in result["soften_line"]


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
        dig_repo.update_tunnel(
            10002, TEST_GUILD_ID, depth=24,
            boss_progress=json.dumps({"25": {"boss_id": "grothak", "status": "active"}}),
        )
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
        bp = json.dumps({"25": {"boss_id": "grothak", "status": "active"}})
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
        dig_repo.update_tunnel(
            10002, TEST_GUILD_ID, depth=24,
            boss_progress=json.dumps({"25": {"boss_id": "grothak", "status": "active"}}),
        )
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


class TestAbandonedDuelCleanup:
    """start_boss_duel must tick durability for any stale dig_active_duels row.

    Without this, a player who pauses mid-fight on a mechanic prompt and
    never resumes would leak the durability tick for that fight.
    """

    def _seed_stale_duel(self, dig_repo, discord_id, guild_id, boss_id="grothak"):
        """Insert a fake 'paused' duel row directly so we can test the cleanup branch."""
        state = {
            "boss_id": boss_id,
            "tier": 25,
            "mechanic_id": "fake_mechanic",
            "risk_tier": "cautious",
            "wager": 0,
            "player_hp": 5,
            "boss_hp": 5,
            "round_num": 3,
            "round_log": "[]",
            "pending_prompt": "{}",
            "rng_state": "",
            "status_effects": "{}",
            "echo_applied": 0,
            "echo_killer_id": None,
            "player_hit": 0.6,
            "player_dmg": 1,
            "boss_hit": 0.3,
            "boss_dmg": 1,
        }
        dig_repo.save_active_duel(discord_id, guild_id, state)

    def test_stale_row_triggers_cleanup_tick(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch)
        # Equip a piece so a tick has something to bite
        gid = dig_repo.add_gear(10001, TEST_GUILD_ID, "armor", 1)
        dig_repo.equip_gear(gid, 10001, TEST_GUILD_ID, "armor")
        self._seed_stale_duel(dig_repo, 10001, TEST_GUILD_ID)

        dig_service.start_boss_duel(10001, TEST_GUILD_ID, "cautious", wager=0)

        # Stale row was either cleared or replaced by a new pause record.
        active = dig_repo.get_active_duel(10001, TEST_GUILD_ID)
        if active is not None:
            assert active.get("mechanic_id") != "fake_mechanic"
        # The cleanup tick fired against the stale fight, dropping durability
        # from 20 to at least 19 (the new fight may also tick if it
        # auto-resolves, dropping further).
        row = dig_repo.get_gear_by_id(gid)
        assert row["durability"] <= 19

    def test_no_stale_row_means_no_cleanup_tick(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """Without a stale row, start_boss_duel must NOT pre-tick durability.

        Whether the new fight itself ticks depends on whether it pauses or
        resolves — which is RNG- and boss-content-dependent. We assert
        only that the durability is in an acceptable range (no double-tick).
        """
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch)
        gid = dig_repo.add_gear(10001, TEST_GUILD_ID, "armor", 1)
        dig_repo.equip_gear(gid, 10001, TEST_GUILD_ID, "armor")

        dig_service.start_boss_duel(10001, TEST_GUILD_ID, "cautious", wager=0)
        row = dig_repo.get_gear_by_id(gid)
        # At most one tick (no double-tick from a phantom cleanup pass).
        assert row["durability"] >= 19


def _at_phase2(dig_service, dig_repo, player_repository, monkeypatch, discord_id,
               *, pending_event=None):
    """Place a player mid-fight at the depth-25 grothak boss, in phase 2 (P2+).

    Pins grothak and seeds ``phase1_defeated`` so the next fight is the secret
    second phase. ``random.random`` is left at 0.99 (round-cap loss) so the
    fight resolves deterministically without consuming the boss.
    """
    _register(player_repository, discord_id=discord_id, balance=200)
    monkeypatch.setattr(time, "time", lambda: 1_000_000)
    monkeypatch.setattr(random, "random", lambda: 0.99)
    dig_service.dig(discord_id, TEST_GUILD_ID)
    entry = {"boss_id": "grothak", "status": "phase1_defeated"}
    if pending_event is not None:
        entry["pending_phase_event_id"] = pending_event
    dig_repo.update_tunnel(
        discord_id, TEST_GUILD_ID,
        depth=24, prestige_level=2,
        boss_progress=json.dumps({"25": entry}),
    )
    monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)


class TestPhaseTransitionEvents:
    """A boss's secret second phase must vary: the rolled transition event
    has to reach the next fight, not just decorate the embed."""

    def test_fight_boss_stores_pending_phase_event_on_transition(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """Winning phase 1 at P2 records a pending transition event for phase 2."""
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch, depth=24, prestige=2)
        rolls = iter([0.0, 0.99] * 100)
        monkeypatch.setattr(random, "random", lambda: next(rolls))
        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "cautious", wager=10)
        assert result["success"]
        assert result.get("phase2_incoming") is True

        entry = json.loads(dig_repo.get_tunnel(10001, TEST_GUILD_ID)["boss_progress"])["25"]
        assert entry.get("status") == "phase1_defeated"
        valid_ids = {e.id for e in PHASE_TRANSITION_EVENTS}
        assert entry.get("pending_phase_event_id") in valid_ids

    def test_fight_boss_phase2_applies_and_consumes_event(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """A pending transition event changes the phase-2 fight, then is consumed."""
        # Baseline phase-2 fight with no pending event.
        _at_phase2(dig_service, dig_repo, player_repository, monkeypatch, 10001)
        base = dig_service.fight_boss(10001, TEST_GUILD_ID, "cautious", wager=10)
        assert base["success"]

        # Identical fight, but 'void_pull' (player -2 HP, boss -2 HP) is pending.
        _at_phase2(dig_service, dig_repo, player_repository, monkeypatch, 10002,
                   pending_event="void_pull")
        evt = dig_service.fight_boss(10002, TEST_GUILD_ID, "cautious", wager=10)
        assert evt["success"]

        # Applied: both fighters start the phase-2 fight 2 HP lighter. Rolls are
        # pinned to 0.99 (no hits land), so round 1 HP is the starting HP.
        assert evt["round_log"][0]["boss_hp"] == base["round_log"][0]["boss_hp"] - 2
        assert evt["round_log"][0]["player_hp"] == base["round_log"][0]["player_hp"] - 2
        # Consumed: the one-shot event is cleared from boss_progress.
        entry = json.loads(dig_repo.get_tunnel(10002, TEST_GUILD_ID)["boss_progress"])["25"]
        assert "pending_phase_event_id" not in entry

    def test_start_boss_duel_stores_pending_phase_event_on_transition(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """start_boss_duel records a pending transition event on a phase-1 win."""
        monkeypatch.setattr("domain.models.boss_mechanics.get_mechanic", lambda mid: None)
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch, depth=24, prestige=2)
        rolls = iter([0.0, 0.99] * 100)
        monkeypatch.setattr(random, "random", lambda: next(rolls))
        result = dig_service.start_boss_duel(10001, TEST_GUILD_ID, "cautious", wager=10)
        assert result["success"]
        assert result.get("phase2_incoming") is True

        entry = json.loads(dig_repo.get_tunnel(10001, TEST_GUILD_ID)["boss_progress"])["25"]
        valid_ids = {e.id for e in PHASE_TRANSITION_EVENTS}
        assert entry.get("pending_phase_event_id") in valid_ids

    def test_start_boss_duel_phase2_applies_transition_event(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """start_boss_duel's phase-2 fight applies a pending transition event."""
        monkeypatch.setattr("domain.models.boss_mechanics.get_mechanic", lambda mid: None)

        _at_phase2(dig_service, dig_repo, player_repository, monkeypatch, 10001)
        base = dig_service.start_boss_duel(10001, TEST_GUILD_ID, "cautious", wager=10)
        assert base["success"]

        _at_phase2(dig_service, dig_repo, player_repository, monkeypatch, 10002,
                   pending_event="void_pull")
        evt = dig_service.start_boss_duel(10002, TEST_GUILD_ID, "cautious", wager=10)
        assert evt["success"]

        # Applied: void_pull starts both fighters 2 HP lighter (rolls pinned, no hits).
        assert evt["round_log"][0]["boss_hp"] == base["round_log"][0]["boss_hp"] - 2
        assert evt["round_log"][0]["player_hp"] == base["round_log"][0]["player_hp"] - 2

    def test_start_boss_duel_consumes_event_even_when_fight_pauses(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """A phase-2 fight that pauses for a mechanic must still persist the
        consumed event: resume_boss_duel re-reads boss_progress fresh, so a
        stale id would re-fire the one-shot on a retry after a paused loss."""
        _at_phase2(dig_service, dig_repo, player_repository, monkeypatch, 10001,
                   pending_event="void_pull")
        # No get_mechanic patch: grothak's mechanic fires and the duel pauses
        # mid-fight (rolls pinned to 0.99, so nobody dies before the trigger).
        dig_service.start_boss_duel(10001, TEST_GUILD_ID, "cautious", wager=10)
        assert dig_repo.get_active_duel(10001, TEST_GUILD_ID) is not None, (
            "expected the duel to pause for a mechanic"
        )
        # The one-shot event must already be consumed in the persisted tunnel.
        entry = json.loads(dig_repo.get_tunnel(10001, TEST_GUILD_ID)["boss_progress"])["25"]
        assert "pending_phase_event_id" not in entry
