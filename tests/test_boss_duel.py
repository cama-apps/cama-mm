"""Tests for the boss HP duel system."""

from __future__ import annotations

import json
import random
import time

import pytest

from repositories.dig_repository import DigRepository
from services.dig._common import DIG_BOSS_STAT_POINT_BONUS
from services.dig_constants import (
    ARMOR_TIERS,
    BOSS_ARCHETYPE_BY_ID,
    BOSS_ARCHETYPES,
    BOSS_DUEL_STATS,
    BOSS_LOSS_EXTRA_COOLDOWN_SECONDS,
    BOSS_LOSS_EXTRA_GEAR_TICKS,
    BOSS_LOSS_KNOCKBACK_MAX,
    BOSS_LOSS_KNOCKBACK_MIN,
    BOSS_LOSS_REPAIR_BILL,
    BOSS_PAYOUTS,
    BOSS_PRESTIGE_BONUS,
    BOSS_TIER_BONUS,
    BOSS_VICTORY_BASE_JC,
    FREE_DIG_COOLDOWN_SECONDS,
    PHASE_TRANSITION_EVENTS,
)
from services.dig_data.balance import scale_positive_dig_jc
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
        assert BOSS_LOSS_KNOCKBACK_MIN <= result["knockback"] <= BOSS_LOSS_KNOCKBACK_MAX
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


class TestMechanicSelectionPerAttempt:
    """Mechanics reroll per attempt while paused duels keep their selected id."""

    EXPECTED_POOL = (
        "grothak_earthquake",
        "grothak_crumble_wall",
        "grothak_bedrock_bellow",
    )

    def test_pause_resume_uses_persisted_selected_mechanic(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch)
        selected_id = "grothak_bedrock_bellow"

        def select_new_mechanic(_rng, pool):
            assert tuple(pool) == self.EXPECTED_POOL
            return selected_id

        monkeypatch.setattr(random.Random, "choice", select_new_mechanic)
        monkeypatch.setattr(random, "random", lambda: 0.5)
        started = dig_service.start_boss_duel(
            10001, TEST_GUILD_ID, "cautious", wager=0,
        )
        assert started["mechanic_id"] == selected_id
        assert dig_repo.get_active_duel(10001, TEST_GUILD_ID)["mechanic_id"] == selected_id

        def unexpected_reroll(_rng, _pool):
            raise AssertionError("resume must use the persisted mechanic id")

        monkeypatch.setattr(random.Random, "choice", unexpected_reroll)
        monkeypatch.setattr(random, "random", lambda: 0.0)
        resumed = dig_service.resume_boss_duel(
            10001, TEST_GUILD_ID, option_idx=0,
        )
        assert resumed["success"]
        assert any(
            entry.get("mechanic_id") == selected_id
            for entry in resumed["round_log"]
        )

    def test_later_attempt_rerolls_from_full_pool_and_may_repeat(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch)
        sampled_pools: list[tuple[str, ...]] = []

        def repeat_same_mechanic(_rng, pool):
            sampled_pools.append(tuple(pool))
            return "grothak_earthquake"

        monkeypatch.setattr(random.Random, "choice", repeat_same_mechanic)
        monkeypatch.setattr(random, "random", lambda: 0.5)

        first = dig_service.start_boss_duel(
            10001, TEST_GUILD_ID, "cautious", wager=0,
        )
        second = dig_service.start_boss_duel(
            10001, TEST_GUILD_ID, "cautious", wager=0,
        )

        assert first["mechanic_id"] == second["mechanic_id"] == "grothak_earthquake"
        assert sampled_pools == [self.EXPECTED_POOL, self.EXPECTED_POOL]


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

        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "cautious", wager=0)
        first_round_boss_hp_after_hit = result["round_log"][0]["boss_hp"]
        assert first_round_boss_hp_after_hit == expected - int(BOSS_DUEL_STATS["cautious"]["player_dmg"])

    def test_boss_hp_scales_extra_hard_at_p4(self, dig_service, dig_repo, player_repository, monkeypatch):
        """P4+ delvers face a small extra boss-HP multiplier on top of the
        flat prestige table."""
        base_boss_hp = int(BOSS_DUEL_STATS["cautious"]["boss_hp"])
        prestige = 4
        archetype = BOSS_ARCHETYPES[BOSS_ARCHETYPE_BY_ID["grothak"]]
        table_hp = (
            int(round(base_boss_hp * archetype["hp_mult"]))
            + int(BOSS_PRESTIGE_BONUS[prestige]["hp"])
        )
        # P4 adds a 3% bump; the boss must end up tougher than the table alone.
        expected = int(round(table_hp * 1.03))
        assert expected > table_hp

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

        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "cautious", wager=0)
        first_round_boss_hp_after_hit = result["round_log"][0]["boss_hp"]
        assert first_round_boss_hp_after_hit == expected - int(BOSS_DUEL_STATS["cautious"]["player_dmg"])


class TestDuelPayout:
    """Win pays wager * BOSS_PAYOUTS[depth][tier]; loss forfeits wager + cave-in."""

    def test_positive_boss_payout_uses_dig_reward_policy(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch)
        balance_before = player_repository.get_balance(10001, TEST_GUILD_ID)
        monkeypatch.setattr(random, "random", lambda: 0.0)

        result = dig_service.fight_boss(
            10001, TEST_GUILD_ID, "cautious", wager=0,
        )

        assert result["won"] is True
        assert result["gross_payout"] == BOSS_VICTORY_BASE_JC[25]
        assert result["payout"] == 10
        assert player_repository.get_balance(
            10001, TEST_GUILD_ID,
        ) == balance_before + 10
        action = dig_repo.get_recent_actions(
            10001, TEST_GUILD_ID, limit=1, action_type="boss_fight",
        )[0]
        detail = json.loads(action["detail"])
        assert detail["gross_jc"] == BOSS_VICTORY_BASE_JC[25]
        assert detail["reward_multiplier"] == 0.65

    @pytest.mark.parametrize("entrypoint", ["legacy", "state_machine"])
    def test_p4_ascension_scales_regular_boss_base_reward(
        self,
        entrypoint,
        dig_service,
        dig_repo,
        player_repository,
        monkeypatch,
    ):
        monkeypatch.setattr("domain.models.boss_mechanics.get_mechanic", lambda mid: None)
        _at_boss(
            dig_service, dig_repo, player_repository, monkeypatch, prestige=4,
        )
        dig_repo.update_tunnel(
            10001,
            TEST_GUILD_ID,
            boss_progress=json.dumps({
                "25": {"boss_id": "grothak", "status": "phase1_defeated"},
            }),
        )
        starter_id = dig_repo.get_equipped_gear(
            10001, TEST_GUILD_ID,
        )["weapon"]["id"]
        dig_repo.unequip_gear(starter_id)
        weapon_id = dig_repo.add_gear(10001, TEST_GUILD_ID, "weapon", 7)
        armor_id = dig_repo.add_gear(10001, TEST_GUILD_ID, "armor", 7)
        dig_repo.equip_gear(weapon_id, 10001, TEST_GUILD_ID, "weapon")
        dig_repo.equip_gear(armor_id, 10001, TEST_GUILD_ID, "armor")
        monkeypatch.setattr(random, "random", lambda: 0.0)

        if entrypoint == "legacy":
            result = dig_service.fight_boss(
                10001, TEST_GUILD_ID, "cautious", wager=0,
            )
        else:
            result = dig_service.start_boss_duel(
                10001, TEST_GUILD_ID, "cautious", wager=0,
            )

        assert result["won"] is True
        assert result["payout"] == scale_positive_dig_jc(22)

    def test_win_pays_from_payout_table(self, dig_service, dig_repo, player_repository, monkeypatch):
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch)
        balance_before = player_repository.get_balance(10001, TEST_GUILD_ID)
        monkeypatch.setattr(random, "random", lambda: 0.0)
        # Pin win chance below the taper knee so this exercises the authored
        # payout-table multiplier untapered.
        monkeypatch.setattr(
            "services.dig_service._approx_duel_win_prob", lambda **kw: 0.50,
        )

        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "cautious", wager=10)
        assert result["won"] is True
        expected_multiplier = BOSS_PAYOUTS[25][0]
        expected_profit = int(10 * (expected_multiplier - 1))
        # Every victory pays the flat base reward on top of the wager profit.
        expected_credit = (
            scale_positive_dig_jc(BOSS_VICTORY_BASE_JC[25]) + expected_profit
        )
        assert player_repository.get_balance(10001, TEST_GUILD_ID) == balance_before + expected_credit
        # Reported payout is the real net credited, not the gross return.
        assert result["payout"] == expected_credit

    def test_audit_log_jc_delta_matches_real_payout(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """The boss_fight audit log's detail.jc_delta must equal the JC the
        player actually received — not a separate gross-loot estimate."""
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch)
        balance_before = player_repository.get_balance(10001, TEST_GUILD_ID)
        monkeypatch.setattr(random, "random", lambda: 0.0)
        monkeypatch.setattr(
            "services.dig_service._approx_duel_win_prob", lambda **kw: 0.50,
        )

        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "cautious", wager=10)
        assert result["won"] is True
        real_payout = player_repository.get_balance(10001, TEST_GUILD_ID) - balance_before

        actions = dig_repo.get_recent_actions(
            10001, TEST_GUILD_ID, action_type="boss_fight",
        )
        assert actions, "expected a boss_fight audit row"
        detail = json.loads(actions[0]["detail"])
        assert detail["won"] is True
        # The logged delta is the actual JC change, matching result["payout"].
        assert detail["jc_delta"] == real_payout
        assert detail["jc_delta"] == result["payout"]

    def test_duel_audit_log_jc_delta_matches_real_payout(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """The duel path (start_boss_duel -> _resolve_duel_outcome) must log
        the real JC payout, not a separate mana-scaled gross-loot estimate."""
        # No mechanic so the duel auto-resolves in one call (no pause prompt).
        monkeypatch.setattr("domain.models.boss_mechanics.get_mechanic", lambda mid: None)
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch)
        balance_before = player_repository.get_balance(10001, TEST_GUILD_ID)
        monkeypatch.setattr(random, "random", lambda: 0.0)
        monkeypatch.setattr(
            "services.dig_service._approx_duel_win_prob", lambda **kw: 0.50,
        )

        result = dig_service.start_boss_duel(10001, TEST_GUILD_ID, "cautious", wager=10)
        assert result["success"]
        assert result["won"] is True
        real_payout = player_repository.get_balance(10001, TEST_GUILD_ID) - balance_before

        actions = dig_repo.get_recent_actions(
            10001, TEST_GUILD_ID, action_type="boss_fight",
        )
        assert actions, "expected a boss_fight audit row"
        detail = json.loads(actions[0]["detail"])
        assert detail["won"] is True
        assert detail["jc_delta"] == real_payout
        assert detail["jc_delta"] == result["payout"]

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
        assert BOSS_LOSS_KNOCKBACK_MIN <= knockback <= BOSS_LOSS_KNOCKBACK_MAX
        tunnel = dig_repo.get_tunnel(10001, TEST_GUILD_ID)
        assert tunnel["depth"] == 99 - knockback

    def test_loss_takes_extra_gear_tick_and_extends_cooldown(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """A boss loss is harsher than a win: gear takes an extra durability tick
        beyond the per-fight tick, and the next-dig cooldown is pushed forward."""
        _register(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        bp_defeated = json.dumps({"25": "defeated", "50": "defeated", "75": "defeated"})
        dig_repo.update_tunnel(10001, TEST_GUILD_ID, depth=99, boss_progress=bp_defeated)
        gid = dig_repo.add_gear(10001, TEST_GUILD_ID, "armor", 1)
        dig_repo.equip_gear(gid, 10001, TEST_GUILD_ID, "armor")
        dur_before = dig_repo.get_gear_by_id(gid)["durability"]

        fight_time = 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1
        monkeypatch.setattr(time, "time", lambda: fight_time)
        monkeypatch.setattr(random, "random", lambda: 0.999)  # guaranteed loss

        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "cautious", wager=10)
        assert result["won"] is False

        # One per-fight tick + BOSS_LOSS_EXTRA_GEAR_TICKS on the loss.
        dur_after = dig_repo.get_gear_by_id(gid)["durability"]
        assert dur_before - dur_after == 1 + BOSS_LOSS_EXTRA_GEAR_TICKS
        # The legacy path applies no stinger, so the cooldown is exactly the
        # flat post-loss extension on top of the fight timestamp.
        tunnel = dig_repo.get_tunnel(10001, TEST_GUILD_ID)
        assert tunnel["last_dig_at"] == fight_time + BOSS_LOSS_EXTRA_COOLDOWN_SECONDS

    def test_loss_suppresses_soften_line_when_no_chip_damage(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """A boss loss in which the player lands NO hits must NOT surface a
        soften_line: starting HP == ending HP, so there is nothing to report.

        Deterministic: with every global roll pinned to 0.999 (above any
        player_hit), the player never lands a hit, the boss survives at full
        HP, and the round-cap loss fires. The soften-suppression assertion now
        ALWAYS runs (no guard)."""
        _register(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)

        seed_max = 8
        bp_seed = {
            "25": "defeated", "50": "defeated", "75": "defeated",
            "100": {
                "hp_remaining": seed_max, "hp_max": seed_max,
                "last_engaged_at": 1_000_000,
                "status": "active",
            },
        }
        dig_repo.update_tunnel(
            10001, TEST_GUILD_ID,
            depth=99, boss_progress=json.dumps(bp_seed),
        )
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        # Player never hits → boss ends untouched at full HP.
        monkeypatch.setattr(random, "random", lambda: 0.999)

        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "cautious", wager=10)
        assert result["won"] is False
        # No chip damage → boss at full HP and the soften line is suppressed.
        assert result["boss_hp_remaining"] == seed_max
        assert result.get("soften_line") is None

    def test_loss_soften_line_present_when_player_chips(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """When the player lands a hit before losing, soften_line surfaces the
        boss's HP transition from its pre-fight HP to its surviving HP.

        Deterministic roll sequence — cautious has crit_chance 0, so each round
        consumes exactly two global rolls (player-hit, then boss-hit); the
        Monte-Carlo win-prob estimate uses a *local* RNG and does not touch this
        stream:
          round 1: player 0.0 -> HIT (boss -1); boss 0.0 -> HIT (player 5->4)
          rounds 2-5: player 0.99 -> MISS; boss 0.0 -> HIT
        Player (5 HP, boss_dmg 1) dies at round 5 having chipped exactly 1 HP.
        The boss is seeded with enough HP that one chip can't kill it."""
        _register(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, TEST_GUILD_ID)
        seed_max = 6
        bp_seed = {
            "25": "defeated", "50": "defeated", "75": "defeated",
            "100": {
                "hp_remaining": seed_max, "hp_max": seed_max,
                "last_engaged_at": 1_000_000,
                "status": "active",
            },
        }
        dig_repo.update_tunnel(
            10001, TEST_GUILD_ID,
            depth=99, boss_progress=json.dumps(bp_seed),
        )
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        # Player lands round 1, then whiffs; boss hits every round → loss at
        # round 5 after the player has chipped exactly 1 HP off the boss.
        rolls = iter([0.0, 0.0, 0.99, 0.0, 0.99, 0.0, 0.99, 0.0, 0.99, 0.0])
        monkeypatch.setattr(random, "random", lambda: next(rolls))

        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "cautious", wager=10)
        assert result["won"] is False
        # Boss survived with exactly one chip taken — soften_line is present and
        # reports the surviving HP over the max. These assertions ALWAYS run.
        assert result["boss_hp_remaining"] == seed_max - 1
        assert result.get("soften_line") is not None
        assert f"{seed_max - 1}/{seed_max}" in result["soften_line"]


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
        # Pin win chance below the taper knee so this isolates the 0.7x echo
        # penalty from the high-win-chance payout taper.
        monkeypatch.setattr(
            "services.dig_service._approx_duel_win_prob", lambda **kw: 0.50,
        )

        result = dig_service.fight_boss(10002, TEST_GUILD_ID, "cautious", wager=10)
        assert result["won"] is True
        assert result.get("echo_applied") is True
        assert result.get("echo_killer_id") == 10001

        # Wager profit is 0.7x the normal cautious multiplier; the flat base
        # reward is still paid on top.
        base_multiplier = BOSS_PAYOUTS[25][0]
        expected_profit = int(10 * (base_multiplier * 0.7 - 1))
        assert player_repository.get_balance(10002, TEST_GUILD_ID) == (
            balance_before
            + scale_positive_dig_jc(BOSS_VICTORY_BASE_JC[25])
            + expected_profit
        )

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
        """A stale (abandoned) duel with no gear snapshot falls back to ticking
        the live loadout, then the row is cleared. With every roll pinned to
        0.99 the *new* grothak fight pauses on its mechanic before it can tick
        its own gear, so the equipped piece drops by EXACTLY the one cleanup
        tick (20 -> 19), and the fake stale row is gone."""
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch)
        # Equip a piece so the legacy-fallback cleanup tick has something to bite.
        gid = dig_repo.add_gear(10001, TEST_GUILD_ID, "armor", 1)
        dig_repo.equip_gear(gid, 10001, TEST_GUILD_ID, "armor")
        dur_before = dig_repo.get_gear_by_id(gid)["durability"]
        assert dur_before == 20
        self._seed_stale_duel(dig_repo, 10001, TEST_GUILD_ID)
        # Confirm the stale row is the fake one before the cleanup runs.
        assert dig_repo.get_active_duel(10001, TEST_GUILD_ID)["mechanic_id"] == "fake_mechanic"

        # Rolls stay at 0.99 (set by _at_boss): the new fight reaches grothak's
        # mechanic and pauses, so its own gear tick is deferred to resume.
        dig_service.start_boss_duel(10001, TEST_GUILD_ID, "cautious", wager=0)

        # The fake stale row was cleared/replaced — the cleanup actually ran.
        active = dig_repo.get_active_duel(10001, TEST_GUILD_ID)
        assert active is None or active.get("mechanic_id") != "fake_mechanic"
        # Exactly one cleanup tick landed on the live loadout (the new fight
        # paused before ticking), so durability dropped by precisely 1.
        assert dig_repo.get_gear_by_id(gid)["durability"] == dur_before - 1

    def test_stale_cleanup_ticks_snapshot_gear_not_current_gear(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """Finding-21: the stale-duel cleanup must tick the gear that fought
        the abandoned duel (its start-time snapshot), NOT the player's current
        equipment.

        Old behavior called tick_gear_durability (current equipped loadout),
        so a player who swapped/repaired gear during the abandoned pause would
        have the WRONG pieces ticked — the snapshot pieces that actually fought
        escape wear, while a freshly-equipped piece is wrongly worn. The fix
        ticks the gear_snapshot_ids recorded with the duel. This test fails on
        the old path (snapshot piece untouched) and passes on the fix."""
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch)
        # The piece that "fought" the stale duel — recorded in the snapshot but
        # NOT currently equipped (the player swapped it out during the pause).
        snapshot_gid = dig_repo.add_gear(10001, TEST_GUILD_ID, "armor", 1)
        # No gear is equipped now, so the old current-loadout tick bites nothing.

        state = {
            "boss_id": "grothak", "tier": 25, "mechanic_id": "fake_mechanic",
            "risk_tier": "cautious", "wager": 0, "player_hp": 5, "boss_hp": 5,
            "round_num": 3, "round_log": "[]", "pending_prompt": "{}",
            "rng_state": "",
            "status_effects": json.dumps({"gear_snapshot_ids": [snapshot_gid]}),
            "echo_applied": 0, "echo_killer_id": None,
            "player_hit": 0.6, "player_dmg": 1, "boss_hit": 0.3, "boss_dmg": 1,
        }
        dig_repo.save_active_duel(10001, TEST_GUILD_ID, state)

        durability_before = dig_repo.get_gear_by_id(snapshot_gid)["durability"]
        dig_service.start_boss_duel(10001, TEST_GUILD_ID, "cautious", wager=0)

        # The snapshot piece (the one that actually fought) lost durability,
        # even though it is unequipped — only tick_gear_durability_ids reaches
        # an unequipped piece, so this proves the cleanup ticked the snapshot.
        durability_after = dig_repo.get_gear_by_id(snapshot_gid)["durability"]
        assert durability_after == durability_before - 1, (
            "stale cleanup did not tick the snapshot gear that fought the duel"
        )

    def test_stale_cleanup_reports_snapshot_gear_that_breaks(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch)
        snapshot_gid = dig_repo.add_gear(
            10001, TEST_GUILD_ID, "armor", 1, durability=1,
        )
        state = {
            "boss_id": "grothak", "tier": 25, "mechanic_id": "fake_mechanic",
            "risk_tier": "cautious", "wager": 0, "player_hp": 5, "boss_hp": 5,
            "round_num": 3, "round_log": "[]", "pending_prompt": "{}",
            "rng_state": "",
            "status_effects": json.dumps({"gear_snapshot_ids": [snapshot_gid]}),
            "echo_applied": 0, "echo_killer_id": None,
            "player_hit": 0.6, "player_dmg": 1, "boss_hit": 0.3, "boss_dmg": 1,
        }
        dig_repo.save_active_duel(10001, TEST_GUILD_ID, state)

        result = dig_service.start_boss_duel(
            10001, TEST_GUILD_ID, "cautious", wager=0,
        )

        assert result["gear_broken"] == [ARMOR_TIERS[1].name]
        assert dig_repo.get_gear_by_id(snapshot_gid)["durability"] == 0

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
        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "cautious", wager=0)
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
        result = dig_service.start_boss_duel(10001, TEST_GUILD_ID, "cautious", wager=0)
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

    def test_consumed_phase_event_not_resurrected_on_auto_resolve_loss(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """Finding-12: a phase event consumed at start_boss_duel must stay
        consumed after an auto-resolved LOSS.

        start_boss_duel consumes pending_phase_event_id (writing boss_progress
        to DB) but does NOT refresh the in-memory tunnel. The loss branch of
        _resolve_duel_outcome used to re-read boss_progress from that stale
        tunnel and persist it back — re-arming the one-shot so it re-fires on
        the next fight. The fix persists from the already-consumed boss_progress
        argument instead. This test fails on the old path (the event survives
        the loss) and passes on the fix."""
        monkeypatch.setattr("domain.models.boss_mechanics.get_mechanic", lambda mid: None)
        _at_phase2(dig_service, dig_repo, player_repository, monkeypatch, 10003,
                   pending_event="void_pull")
        # Pre-condition: the pending event is present before the fight.
        entry_before = json.loads(
            dig_repo.get_tunnel(10003, TEST_GUILD_ID)["boss_progress"]
        )["25"]
        assert entry_before.get("pending_phase_event_id") == "void_pull"

        # Force an auto-resolved loss (no mechanic pause, no hits land).
        monkeypatch.setattr(random, "random", lambda: 0.99)
        result = dig_service.start_boss_duel(10003, TEST_GUILD_ID, "cautious", wager=10)
        assert result["success"]
        assert result["won"] is False
        # No duel paused — this was a clean auto-resolve.
        assert dig_repo.get_active_duel(10003, TEST_GUILD_ID) is None

        # The one-shot must NOT be re-armed by the loss-path persist.
        entry_after = json.loads(
            dig_repo.get_tunnel(10003, TEST_GUILD_ID)["boss_progress"]
        )["25"]
        assert "pending_phase_event_id" not in entry_after, (
            "consumed phase event was resurrected on the auto-resolve loss path"
        )

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


class TestAmuletCritPersistence:
    """A paused multi-phase fight must keep the amulet's gear-derived crit.

    Cautious risk contributes 0 crit, so any non-zero crit persisted in the
    duel row can only come from the equipped amulet. This guards the
    fight-start snapshot (the crit could silently be lost on resume if it
    weren't persisted) and the resume read's no-resurrection behavior.
    """

    def test_amulet_crit_persisted_in_paused_duel(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _at_phase2(dig_service, dig_repo, player_repository, monkeypatch, 10001)
        # Void-Touched amulet (tier 7: crit 0.10 / +1), added directly to
        # bypass the prestige buy-gate — combat doesn't re-check it.
        gid = dig_repo.add_gear(10001, TEST_GUILD_ID, "amulet", 7)
        dig_repo.equip_gear(gid, 10001, TEST_GUILD_ID, "amulet")
        # Nobody hits (0.99), so the fight survives to grothak's mechanic and
        # pauses instead of resolving.
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.start_boss_duel(10001, TEST_GUILD_ID, "cautious", wager=10)

        active = dig_repo.get_active_duel(10001, TEST_GUILD_ID)
        assert active is not None, "expected the fight to pause on a mechanic"
        # Cautious base crit is 0; the amulet is the sole source.
        assert active["crit_chance"] == pytest.approx(0.10)
        assert active["crit_bonus"] == 1

        # Resume must read the persisted crit (not recompute from risk-tier)
        # and finish the fight without error.
        resumed = dig_service.resume_boss_duel(10001, TEST_GUILD_ID, option_idx=0)
        assert resumed["success"]
        assert dig_repo.get_active_duel(10001, TEST_GUILD_ID) is None

    def test_paused_duel_without_amulet_persists_zero_crit(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        # No amulet: persisted crit must be 0. The resume path must NOT
        # resurrect a risk-tier crit from this 0 (the removed fallback bug).
        _at_phase2(dig_service, dig_repo, player_repository, monkeypatch, 10002)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.start_boss_duel(10002, TEST_GUILD_ID, "cautious", wager=10)

        active = dig_repo.get_active_duel(10002, TEST_GUILD_ID)
        assert active is not None, "expected the fight to pause on a mechanic"
        assert active["crit_chance"] == pytest.approx(0.0)
        assert active["crit_bonus"] == 0


class TestWagerTaper:
    """Payout multiplier tapers toward break-even at high win chance, so
    softening a boss to a near-sure win then betting big stops printing money.
    """

    def test_wager_payout_untouched_below_knee(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch)
        # A genuine ~50/50 bet sits below the taper knee: the authored
        # BOSS_PAYOUTS multiplier is used unchanged.
        monkeypatch.setattr(
            "services.dig_service._approx_duel_win_prob", lambda **kw: 0.50,
        )
        monkeypatch.setattr(random, "random", lambda: 0.0)  # deterministic win
        balance_before = player_repository.get_balance(10001, TEST_GUILD_ID)

        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "cautious", wager=100)

        assert result["won"] is True
        wager_profit = int(100 * (BOSS_PAYOUTS[25][0] - 1))
        expected = scale_positive_dig_jc(BOSS_VICTORY_BASE_JC[25]) + wager_profit
        assert (player_repository.get_balance(10001, TEST_GUILD_ID)
                == balance_before + expected)

    def test_wager_payout_tapers_at_high_win_chance(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch)
        # A near-certain (softened) bet at 95%: the multiplier tapers to fair
        # odds, so a 100 JC wager profits only ~+5 (plus the flat base
        # reward) instead of the untapered +50.
        monkeypatch.setattr(
            "services.dig_service._approx_duel_win_prob", lambda **kw: 0.95,
        )
        monkeypatch.setattr(random, "random", lambda: 0.0)  # deterministic win
        balance_before = player_repository.get_balance(10001, TEST_GUILD_ID)

        result = dig_service.fight_boss(10001, TEST_GUILD_ID, "cautious", wager=100)

        assert result["won"] is True
        expected = scale_positive_dig_jc(BOSS_VICTORY_BASE_JC[25]) + 5
        assert (player_repository.get_balance(10001, TEST_GUILD_ID)
                == balance_before + expected)
        assert result["payout"] == expected

    def test_won_wager_at_high_win_chance_never_loses_money(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        # First digger kills the boss, leaving a weakened echo (0.7x payout).
        _at_boss(dig_service, dig_repo, player_repository, monkeypatch)
        monkeypatch.setattr(random, "random", lambda: 0.0)
        dig_service.fight_boss(10001, TEST_GUILD_ID, "reckless", wager=10)

        # Second digger fights the echo at a near-certain win chance. The
        # taper plus the 0.7x echo penalty would drive a winning wager
        # negative — but a win must never cost the player money.
        _register(player_repository, discord_id=10002, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 10)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10002, TEST_GUILD_ID)
        dig_repo.update_tunnel(
            10002, TEST_GUILD_ID, depth=24,
            boss_progress=json.dumps({"25": {"boss_id": "grothak", "status": "active"}}),
        )
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 2 * FREE_DIG_COOLDOWN_SECONDS + 10)
        monkeypatch.setattr(
            "services.dig_service._approx_duel_win_prob", lambda **kw: 0.95,
        )
        monkeypatch.setattr(random, "random", lambda: 0.0)  # deterministic win
        balance_before = player_repository.get_balance(10002, TEST_GUILD_ID)

        result = dig_service.fight_boss(10002, TEST_GUILD_ID, "cautious", wager=100)
        balance_after = player_repository.get_balance(10002, TEST_GUILD_ID)

        assert result["won"] is True
        assert result.get("echo_applied") is True
        # A win never costs money, and the reported payout is the real balance
        # change — not an inflated gross figure.
        assert balance_after >= balance_before
        assert result["payout"] == balance_after - balance_before


# ---------------------------------------------------------------------------
# Finding-11: live boss-duel path (start_boss_duel → _resolve_duel_outcome)
# win and loss — wager atomicity + balance assertions
# ---------------------------------------------------------------------------


def _at_boss_for_duel(
    dig_service, dig_repo, player_repository, monkeypatch, *,
    uid=30001, depth=24, balance=300,
):
    """Register a fresh player one block before the depth-25 boss and disable mechanics."""
    player_repository.add(
        discord_id=uid,
        discord_username=f"DuelUser{uid}",
        guild_id=TEST_GUILD_ID,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    player_repository.update_balance(uid, TEST_GUILD_ID, balance)
    monkeypatch.setattr(time, "time", lambda: 2_000_000)
    monkeypatch.setattr(random, "random", lambda: 0.99)
    dig_service.dig(uid, TEST_GUILD_ID)
    dig_repo.update_tunnel(
        uid, TEST_GUILD_ID,
        depth=depth,
        boss_progress=json.dumps({"25": {"boss_id": "grothak", "status": "active"}}),
    )
    monkeypatch.setattr(time, "time", lambda: 2_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
    # Disable mechanics so start_boss_duel auto-resolves without mid-fight pause
    monkeypatch.setattr("domain.models.boss_mechanics.get_mechanic", lambda mid: None)
    return uid


class TestLiveDuelPathWagerAtomicity:
    """Finding-11: start_boss_duel → _resolve_duel_outcome must commit balance
    and tunnel changes atomically, and wager payouts must match reality."""

    def test_win_pays_wager_and_boss_jc(self, dig_service, dig_repo, player_repository, monkeypatch):
        """A win via start_boss_duel must credit wager*multiplier + base JC."""
        uid = _at_boss_for_duel(dig_service, dig_repo, player_repository, monkeypatch, uid=30001)
        balance_before = player_repository.get_balance(uid, TEST_GUILD_ID)
        # Force a guaranteed win (all random rolls succeed)
        monkeypatch.setattr(random, "random", lambda: 0.0)
        monkeypatch.setattr("services.dig_service._approx_duel_win_prob", lambda **kw: 0.50)

        result = dig_service.start_boss_duel(uid, TEST_GUILD_ID, "cautious", wager=20)
        assert result["success"]
        assert result["won"] is True

        balance_after = player_repository.get_balance(uid, TEST_GUILD_ID)
        real_delta = balance_after - balance_before
        # Win must credit the player (net positive)
        assert real_delta > 0, f"win paid nothing: balance_after={balance_after}, before={balance_before}"
        # Reported payout must exactly match the real balance change
        assert result["payout"] == real_delta, (
            f"reported payout {result['payout']} != real balance change {real_delta}"
        )

    def test_first_clear_stat_point_commits_with_boss_clear_and_is_idempotent(
        self, dig_service, dig_repo, player_repository, monkeypatch
    ):
        """A4: the first-clear S-point award must commit in the SAME atomic write
        as the boss-defeated flip (live duel path), and must never re-award on a
        retry. Previously the live path did its own update_tunnel for the stat
        point BEFORE the atomic victory txn, so a crash between them could desync
        the award from the boss-clear.
        """
        uid = _at_boss_for_duel(dig_service, dig_repo, player_repository, monkeypatch, uid=30009)
        points_before = dig_repo.get_tunnel(uid, TEST_GUILD_ID)["stat_points"]
        monkeypatch.setattr(random, "random", lambda: 0.0)
        monkeypatch.setattr("services.dig_service._approx_duel_win_prob", lambda **kw: 0.50)

        result = dig_service.start_boss_duel(uid, TEST_GUILD_ID, "cautious", wager=20)
        assert result["won"] is True
        assert result["stat_point_awarded"] is True

        tunnel_after = dig_repo.get_tunnel(uid, TEST_GUILD_ID)
        # Both effects land in the one row that atomic_boss_full_victory wrote:
        # the boss is flipped to defeated AND the stat point is incremented.
        boss_progress = json.loads(tunnel_after["boss_progress"])
        assert boss_progress["25"]["status"] == "defeated", (
            "boss not flipped to defeated in the committed row"
        )
        assert tunnel_after["stat_points"] == points_before + DIG_BOSS_STAT_POINT_BONUS, (
            "stat point not committed alongside the boss-clear"
        )
        # The boundary is now recorded in the same row, so the pure award helper
        # returns None for a retry: the award is idempotent and will not re-pay.
        assert 25 in dig_service._get_stat_boss_awards(tunnel_after)
        assert dig_service._boss_stat_point_award_updates(tunnel_after, 25) is None, (
            "stat point would be awarded a second time on retry"
        )

    def test_loss_debits_wager_atomically(self, dig_service, dig_repo, player_repository, monkeypatch):
        """A loss via start_boss_duel must debit wager; depth + balance change
        are committed in one atomic_tunnel_balance_update, not two separate writes."""
        uid = _at_boss_for_duel(dig_service, dig_repo, player_repository, monkeypatch, uid=30002, balance=500)
        balance_before = player_repository.get_balance(uid, TEST_GUILD_ID)
        depth_before = dig_repo.get_tunnel(uid, TEST_GUILD_ID)["depth"]
        # Force a guaranteed loss (no rolls succeed)
        monkeypatch.setattr(random, "random", lambda: 0.999)
        monkeypatch.setattr("services.dig_service._approx_duel_win_prob", lambda **kw: 0.50)

        result = dig_service.start_boss_duel(uid, TEST_GUILD_ID, "cautious", wager=30)
        assert result["success"]
        assert result["won"] is False

        balance_after = player_repository.get_balance(uid, TEST_GUILD_ID)
        tunnel_after = dig_repo.get_tunnel(uid, TEST_GUILD_ID)
        # Wager must be debited
        assert balance_after == balance_before - 30, (
            f"wager not fully debited: before={balance_before}, after={balance_after}"
        )
        # Knockback must have reduced depth
        assert tunnel_after["depth"] < depth_before, (
            "no knockback on duel loss"
        )

    def test_loss_with_no_wager_charges_repair_bill(
        self, dig_service, dig_repo, player_repository, monkeypatch
    ):
        """A wager-free loss is no longer free: it charges a flat repair bill so
        losing a boss always costs something."""
        uid = _at_boss_for_duel(dig_service, dig_repo, player_repository, monkeypatch, uid=30003, balance=200)
        balance_before = player_repository.get_balance(uid, TEST_GUILD_ID)
        monkeypatch.setattr(random, "random", lambda: 0.999)
        monkeypatch.setattr("services.dig_service._approx_duel_win_prob", lambda **kw: 0.50)

        result = dig_service.start_boss_duel(uid, TEST_GUILD_ID, "cautious", wager=0)
        assert result["success"]
        assert result["won"] is False

        balance_after = player_repository.get_balance(uid, TEST_GUILD_ID)
        assert balance_after == balance_before - BOSS_LOSS_REPAIR_BILL, (
            "zero-wager loss should charge the flat repair bill"
        )
        assert result["jc_delta"] == -BOSS_LOSS_REPAIR_BILL

    def test_forced_no_wager_phase_loss_does_not_charge_repair_bill(
        self, dig_service, dig_repo, player_repository, monkeypatch
    ):
        """Mid-phase no-wager fights are mandatory, not voluntary free fights."""
        uid = _at_boss_for_duel(
            dig_service, dig_repo, player_repository, monkeypatch,
            uid=30010, balance=200,
        )
        dig_repo.update_tunnel(uid, TEST_GUILD_ID, prestige_level=2)
        balance_before = player_repository.get_balance(uid, TEST_GUILD_ID)
        monkeypatch.setattr(random, "random", lambda: 0.999)
        monkeypatch.setattr("services.dig_service._approx_duel_win_prob", lambda **kw: 0.50)

        result = dig_service.start_boss_duel(uid, TEST_GUILD_ID, "cautious", wager=0)
        assert result["success"]
        assert result["won"] is False

        balance_after = player_repository.get_balance(uid, TEST_GUILD_ID)
        assert balance_after == balance_before
        assert result["jc_delta"] == 0


# ---------------------------------------------------------------------------
# Finding-5: a boss-duel loss can never drive the balance negative.
#
# The wager is only *validated* at start_boss_duel, never escrowed. A player
# who spends JC during a mid-fight pause can reach resolution with a balance
# below the wager; the loss debit must be floored at the current balance so
# it can never go negative. (Behavior: the loss debit is clamped, not the
# stake escrowed — see the fix note in _resolve_duel_outcome.)
# ---------------------------------------------------------------------------


def _seed_loss_bound_duel(dig_repo, discord_id, guild_id, *, wager):
    """Seed a paused duel rigged to resolve to a LOSS on resume.

    player_hp=1 with boss_hit pinned high and player_hit=0 means the player
    dies in the continued auto-rounds (random pinned to 0.99 by the caller),
    so the resume resolves as a loss with the given wager forfeited.
    """
    from domain.models.boss_mechanics import MECHANIC_REGISTRY as _MECHS

    mechanic_id = next(iter(_MECHS))
    mech = _MECHS[mechanic_id]
    pp = {
        "mechanic_id": mechanic_id,
        "prompt_title": mech.prompt_title,
        "prompt_description": mech.prompt_description,
        "options": [
            {"option_idx": i, "label": o.label} for i, o in enumerate(mech.options)
        ],
        "safe_option_idx": mech.safe_option_idx,
    }
    state = {
        "boss_id": "grothak", "tier": 25, "mechanic_id": mechanic_id,
        "risk_tier": "cautious", "wager": wager,
        "player_hp": 1, "boss_hp": 50, "round_num": 2,
        "round_log": "[]", "pending_prompt": json.dumps(pp), "rng_state": "",
        "status_effects": json.dumps({
            "attempts_this_fight": 1, "initial_win_chance": 0.5, "multiplier": 2.0,
        }),
        "echo_applied": 0, "echo_killer_id": None,
        "player_hit": 0.0, "player_dmg": 1, "boss_hit": 1.0, "boss_dmg": 5,
    }
    dig_repo.save_active_duel(discord_id, guild_id, state)
    return mech.safe_option_idx


class TestLossCannotGoNegative:
    """Finding-5: a wagered loss must floor the debit at the current balance."""

    def test_loss_after_spending_during_pause_floors_at_zero(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """Player wagers 100, the fight pauses, they spend down to 30 during the
        pause, then the resumed fight is lost. The 100-JC wager debit must be
        floored to 30 so the balance lands at 0, never negative.

        Fails on the old path (balance goes to -70) and passes on the fix."""
        uid = _register(player_repository, discord_id=40001, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(uid, TEST_GUILD_ID)
        dig_repo.update_tunnel(
            uid, TEST_GUILD_ID, depth=24,
            boss_progress=json.dumps({"25": {"boss_id": "grothak", "status": "active"}}),
        )
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)

        safe_idx = _seed_loss_bound_duel(dig_repo, uid, TEST_GUILD_ID, wager=100)
        # Simulate spending during the pause: balance drops below the wager.
        player_repository.update_balance(uid, TEST_GUILD_ID, 30)

        result = dig_service.resume_boss_duel(uid, TEST_GUILD_ID, option_idx=safe_idx)
        assert result["success"]
        assert result["won"] is False

        balance_after = player_repository.get_balance(uid, TEST_GUILD_ID)
        assert balance_after >= 0, (
            f"loss drove balance negative: {balance_after}"
        )
        assert balance_after == 0, (
            f"loss debit should floor to the available 30 JC, leaving 0; got {balance_after}"
        )

    def test_loss_with_sufficient_balance_debits_full_wager(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """Control: when the player can afford the wager, the full amount is
        still debited — the floor must not under-charge a solvent player."""
        uid = _register(player_repository, discord_id=40002, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(uid, TEST_GUILD_ID)
        dig_repo.update_tunnel(
            uid, TEST_GUILD_ID, depth=24,
            boss_progress=json.dumps({"25": {"boss_id": "grothak", "status": "active"}}),
        )
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)

        safe_idx = _seed_loss_bound_duel(dig_repo, uid, TEST_GUILD_ID, wager=100)
        balance_before = player_repository.get_balance(uid, TEST_GUILD_ID)

        result = dig_service.resume_boss_duel(uid, TEST_GUILD_ID, option_idx=safe_idx)
        assert result["success"]
        assert result["won"] is False

        balance_after = player_repository.get_balance(uid, TEST_GUILD_ID)
        assert balance_after == balance_before - 100, (
            "solvent loss should debit the full wager"
        )

    def test_in_debt_loss_is_not_credited(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """Regression: an in-debt player who LOSES a free boss fight must NOT be
        credited. The old clamp did `jc_delta = -current_balance`, which is
        POSITIVE when the balance is already negative — minting coins (wiping the
        debt) on a loss. The fix clamps the debit to the positive balance only,
        so a negative balance is left unchanged on a loss (never credited)."""
        uid = _register(player_repository, discord_id=40004, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(uid, TEST_GUILD_ID)
        dig_repo.update_tunnel(
            uid, TEST_GUILD_ID, depth=24,
            boss_progress=json.dumps({"25": {"boss_id": "grothak", "status": "active"}}),
        )
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)

        # Free fight (no wager) so the loss charges the flat repair bill.
        safe_idx = _seed_loss_bound_duel(dig_repo, uid, TEST_GUILD_ID, wager=0)
        # Player is already in debt when the fight resolves.
        player_repository.update_balance(uid, TEST_GUILD_ID, -100)

        result = dig_service.resume_boss_duel(uid, TEST_GUILD_ID, option_idx=safe_idx)
        assert result["success"]
        assert result["won"] is False

        balance_after = player_repository.get_balance(uid, TEST_GUILD_ID)
        assert balance_after <= 0, (
            f"a loss must never CREDIT an in-debt player; balance rose to {balance_after}"
        )
        assert balance_after == -100, (
            f"an already-negative balance must be unchanged by a clamped loss; got {balance_after}"
        )
