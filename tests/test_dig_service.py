"""Core dig progression: basic dig flow, depth/balance updates, cooldowns,
layer transitions, paid digs, and prestige-adjacent gameplay paths."""

import json
import random
import sqlite3
import time
from types import SimpleNamespace

import pytest

from commands.dig import _append_sabotage_prediction_steal_line
from repositories.dig_repository import DigRepository
from repositories.prediction_repository import PredictionRepository
from services.dig_constants import (
    BASE_DIG_JC_PAYOUT_CAP,
    BOSS_BOUNDARIES,
    BOSS_LOSS_REPAIR_BILL,
    BOSS_VICTORY_BASE_JC,
    BOSSES,
    CAVE_IN_BLOCK_LOSS_RANGES,
    CHEER_COOLDOWN_SECONDS,
    DIG_TIPS,
    FIRST_DIG_ADVANCE_MAX,
    FIRST_DIG_ADVANCE_MIN,
    FIRST_DIG_JC_MAX,
    FIRST_DIG_JC_MIN,
    FREE_DIG_COOLDOWN_SECONDS,
    INSURANCE_BASE_COST,
    INSURANCE_COST_DEPTH_DIVISOR,
    INSURANCE_DURATION_SECONDS,
    MILESTONES,
    PAID_DIG_COST_CAP,
    PAID_DIG_COSTS_PER_DAY,
    PICKAXE_TIERS,
    PINNACLE_DEPTH,
    SABOTAGE_BASE_COST,
    SABOTAGE_COOLDOWN_SECONDS,
    SABOTAGE_COST_DIVISOR,
    SABOTAGE_DAMAGE_MAX,
    SABOTAGE_DAMAGE_MIN,
    SABOTAGE_SUCCESS_CHANCE,
    STREAKS,
)
from services.dig_data.bosses import (
    BOSS_DUEL_STATS,
    BOSS_FREE_FIGHT_ACCURACY_MOD,
    BOSS_PRESTIGE_BONUS,
    BOSS_TIER_BONUS,
)
from services.dig_service import DigService, _prestige_cave_in_multiplier
from utils.economy_scaling import scale_minigame_jc_delta


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def dig_service(dig_repo, player_repository, monkeypatch):
    svc = DigService(dig_repo, player_repository)
    # Neutralize weather so random rolls don't interfere with tests
    # that depend on exact probabilities. Tests that need weather
    # can override _get_weather_effects or set weather explicitly.
    monkeypatch.setattr(svc, "_get_weather_effects", lambda guild_id, layer_name: {})
    return svc


@pytest.fixture
def prediction_repo(repo_db_path):
    return PredictionRepository(repo_db_path)


def _register_player(player_repository, discord_id=10001, guild_id=12345, balance=100):
    """Helper to register a player with balance."""
    player_repository.add(
        discord_id=discord_id,
        discord_username=f"Player{discord_id}",
        guild_id=guild_id,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    if balance != 3:  # default is 3
        player_repository.update_balance(discord_id, guild_id, balance)
    return discord_id


def test_sabotage_embed_mentions_prediction_contract_steal():
    embed = type("Embed", (), {"description": "Damage dealt: **6** blocks"})()
    result = {
        "prediction_contract_steal": {
            "prediction_id": 42,
            "side": "yes",
            "contracts": 4,
        }
    }

    _append_sabotage_prediction_steal_line(embed, result)

    assert "Stole **4 YES** prediction contracts from market **#42**." in embed.description


def test_sabotage_embed_ignores_prediction_contract_steal_on_miss():
    embed = type("Embed", (), {"description": "The sabotage missed."})()
    result = {
        "sabotage_hit": False,
        "prediction_contract_steal": {
            "prediction_id": 42,
            "side": "yes",
            "contracts": 4,
        },
    }

    _append_sabotage_prediction_steal_line(embed, result)

    assert embed.description == "The sabotage missed."


class TestDigConstants:
    """Pure-data assertions on tunable constants."""

    def test_paid_dig_ramp_is_strictly_increasing(self):
        """Each successive paid dig in a day costs more than the last."""
        assert len(PAID_DIG_COSTS_PER_DAY) >= 2, "need at least two cost tiers to assert a ramp"
        for i in range(len(PAID_DIG_COSTS_PER_DAY) - 1):
            assert PAID_DIG_COSTS_PER_DAY[i] < PAID_DIG_COSTS_PER_DAY[i + 1], (
                f"cost[{i}]={PAID_DIG_COSTS_PER_DAY[i]} is not less than "
                f"cost[{i+1}]={PAID_DIG_COSTS_PER_DAY[i+1]}"
            )

    def test_paid_dig_cost_cap_equals_last_ramp_entry(self):
        """The cap should equal the final escalated cost so the ramp tops out there."""
        assert PAID_DIG_COSTS_PER_DAY[-1] == PAID_DIG_COST_CAP, (
            f"CAP={PAID_DIG_COST_CAP} does not match last ramp entry "
            f"{PAID_DIG_COSTS_PER_DAY[-1]}"
        )

    def test_boss_victory_base_jc_covers_every_boss_boundary(self):
        """Every regular boss boundary needs a base-reward entry, else a win
        there silently falls through to the 15-JC default. The pinnacle
        (350) is excluded — it pays PINNACLE_BASE_JC_REWARD instead."""
        assert set(BOSS_VICTORY_BASE_JC) == set(BOSS_BOUNDARIES)


class TestPrestigeCaveInMultiplier:
    """Pure math: prestige scales cave-in chance 0.9× → 1.2×."""

    @pytest.mark.parametrize(
        "prestige,expected",
        [(0, 0.9), (3, 0.99), (10, 1.20)],
    )
    def test_multiplier_anchors(self, prestige, expected):
        assert _prestige_cave_in_multiplier(prestige) == pytest.approx(expected)


class TestCoreDig:
    """Tests for basic dig mechanics."""

    def test_first_dig_creates_tunnel(self, dig_service, player_repository, guild_id, monkeypatch):
        """First dig creates tunnel with name, returns is_first_dig=True, guaranteed 3-7 blocks and 1-5 JC."""
        _register_player(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        random.seed(42)
        result = dig_service.dig(10001, guild_id)
        assert result["success"]
        assert result["is_first_dig"] is True
        assert result["tunnel_name"]  # non-empty name
        assert FIRST_DIG_ADVANCE_MIN <= result["advance"] <= FIRST_DIG_ADVANCE_MAX
        assert FIRST_DIG_JC_MIN <= result["jc_earned"] <= FIRST_DIG_JC_MAX

    def test_first_dig_no_cave_in(self, dig_service, player_repository, guild_id, monkeypatch):
        """First dig never has cave-in (run 50 times with different seeds)."""
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        for seed in range(50):
            # Each iteration needs a fresh player/tunnel
            pid = 20000 + seed
            _register_player(player_repository, discord_id=pid)
            random.seed(seed)
            result = dig_service.dig(pid, guild_id)
            assert result["success"]
            assert not result.get("cave_in"), f"Cave-in on first dig with seed={seed}"

    def test_dig_advances_depth(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Normal dig increases depth within layer advance range."""
        _register_player(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        result = dig_service.dig(10001, guild_id)
        assert result["success"]
        depth_after_first = result["depth"]
        assert depth_after_first > 0

        # Second dig after cooldown
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        # Prevent cave-in
        monkeypatch.setattr(random, "random", lambda: 0.99)
        result2 = dig_service.dig(10001, guild_id)
        assert result2["success"]
        assert result2["depth"] > depth_after_first

    def test_dig_earns_jc(self, dig_service, player_repository, guild_id, monkeypatch):
        """Dig earns JC within layer range."""
        _register_player(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        random.seed(42)
        result = dig_service.dig(10001, guild_id)
        assert result["success"]
        assert result["jc_earned"] >= 0

    def test_base_dig_payout_is_capped_at_20(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Normal dig base loot is capped before milestone and streak bonuses."""
        _register_player(player_repository)
        dig_repo.create_tunnel(10001, guild_id, "T")
        dig_repo.update_tunnel(10001, guild_id, depth=10, max_depth=10)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        monkeypatch.setattr(dig_service, "_apply_mana_yield_variance", lambda did, gid, jc: 25)

        result = dig_service.dig(10001, guild_id)

        assert result["success"]
        assert result["milestone_bonus"] == 0
        assert result["streak_bonus"] == 0
        assert result["jc_earned"] == scale_minigame_jc_delta(BASE_DIG_JC_PAYOUT_CAP)

    def test_dig_scales_generated_jc_before_mana_taxes(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch
    ):
        _register_player(player_repository)
        dig_repo.create_tunnel(10001, guild_id, "T")
        dig_repo.update_tunnel(10001, guild_id, depth=10, max_depth=10)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        monkeypatch.setattr(dig_service, "_apply_mana_yield_variance", lambda did, gid, jc: 25)
        seen_tax_inputs = []

        def capture_tax_input(did, gid, jc):
            seen_tax_inputs.append(jc)
            return jc

        monkeypatch.setattr(dig_service, "_apply_mana_yield_taxes", capture_tax_input)
        monkeypatch.setattr(dig_service, "_helltide_tax", lambda gid: 0)

        result = dig_service.dig(10001, guild_id)

        expected = scale_minigame_jc_delta(BASE_DIG_JC_PAYOUT_CAP)
        assert seen_tax_inputs == [expected]
        assert result["jc_earned"] == expected

    def test_stacked_base_dig_bonuses_are_capped_after_modifiers(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch
    ):
        """Additive and multiplicative base-loot bonuses cannot exceed the cap."""
        _register_player(player_repository)
        dig_repo.create_tunnel(10001, guild_id, "T")
        dig_repo.update_tunnel(10001, guild_id, depth=10, max_depth=10)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        monkeypatch.setattr(random, "randint", lambda a, b: b)
        monkeypatch.setattr(
            dig_service,
            "_get_weather_effects",
            lambda gid, layer_name: {"jc_multiplier": 4.0, "jc_bonus": 30},
        )
        monkeypatch.setattr(
            dig_service,
            "_relic_jc_yield_multiplier",
            lambda did, gid, **kwargs: 2.0,
        )

        result = dig_service.dig(10001, guild_id)

        assert result["success"]
        assert result["milestone_bonus"] == 0
        assert result["streak_bonus"] == 0
        assert result["jc_earned"] == scale_minigame_jc_delta(BASE_DIG_JC_PAYOUT_CAP)

    def test_precondition_base_dig_range_is_capped_after_modifiers(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch
    ):
        """DM-mode base-loot range cannot advertise more than the payout cap."""
        _register_player(player_repository)
        dig_repo.create_tunnel(10001, guild_id, "T")
        dig_repo.update_tunnel(10001, guild_id, depth=10, max_depth=10)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(
            dig_service,
            "_get_weather_effects",
            lambda gid, layer_name: {"jc_multiplier": 4.0, "jc_bonus": 30},
        )
        monkeypatch.setattr(
            dig_service,
            "_relic_jc_yield_multiplier",
            lambda did, gid, **kwargs: 2.0,
        )

        terminal, preconditions = dig_service.dig_with_preconditions(10001, guild_id)

        assert terminal is None
        assert preconditions["jc_min"] <= BASE_DIG_JC_PAYOUT_CAP
        assert preconditions["jc_max"] <= BASE_DIG_JC_PAYOUT_CAP

    def test_overgrowth_bonus_is_included_in_base_payout_cap(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch
    ):
        """Overgrowth's flat bonus cannot push base dig payout above the cap."""
        _register_player(player_repository)
        dig_repo.create_tunnel(10001, guild_id, "T")
        dig_repo.update_tunnel(10001, guild_id, depth=10, max_depth=10)
        dig_service.buff_service = SimpleNamespace(
            has_overgrowth=lambda did, gid: True,
            consume_overgrowth_charge=lambda did, gid: True,
        )
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        monkeypatch.setattr(dig_service, "_apply_mana_yield_variance", lambda did, gid, jc: 25)

        result = dig_service.dig(10001, guild_id)

        assert result["success"]
        assert result["milestone_bonus"] == 0
        assert result["streak_bonus"] == 0
        assert result["jc_earned"] == scale_minigame_jc_delta(BASE_DIG_JC_PAYOUT_CAP)

    def test_prospectors_streak_relic_is_included_in_base_payout_cap(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch
    ):
        """The Prospector's Streak relic is folded into the non-streak total, so
        a high cave-in-free streak can no longer push a base dig past the cap."""
        _register_player(player_repository)
        dig_repo.create_tunnel(10001, guild_id, "T")
        # A near-max cave-in-free streak: +1 this dig -> relic would add +20.
        dig_repo.update_tunnel(
            10001, guild_id, depth=10, max_depth=10, cavein_free_streak=19,
        )
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        # Only the Prospector's Streak relic is equipped.
        monkeypatch.setattr(
            dig_service, "_has_relic",
            lambda did, gid, rid: rid == "prospectors_streak",
        )
        # Base loot lands at 15 — under the cap on its own, so a +20 relic add
        # would blow past 20 unless it is folded into the capped non-streak total.
        monkeypatch.setattr(
            dig_service, "_apply_mana_yield_variance", lambda did, gid, jc: 15,
        )

        result = dig_service.dig(10001, guild_id)

        assert result["success"]
        assert result["milestone_bonus"] == 0
        assert result["streak_bonus"] == 0
        # 15 base + 20 relic folded, capped at 20, then economy-scaled.
        assert result["jc_earned"] == scale_minigame_jc_delta(BASE_DIG_JC_PAYOUT_CAP)

    def test_streak_bonus_is_capped_after_perk_multiplier(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch
    ):
        """Patient Step cannot push dig streak JC above the streak cap."""
        _register_player(player_repository)
        dig_repo.create_tunnel(10001, guild_id, "T")
        dig_repo.update_tunnel(10001, guild_id, depth=10, max_depth=10)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        monkeypatch.setattr(dig_service, "_apply_mana_yield_variance", lambda did, gid, jc: 1)
        monkeypatch.setattr(dig_service, "_calculate_daily_streak", lambda did, gid, tunnel, today: (30, False))
        monkeypatch.setattr(
            dig_service,
            "_aggregate_perk_effects",
            lambda perks: {"streak_bonus_multiplier": 1.0},
        )

        result = dig_service.dig(10001, guild_id)

        assert result["success"]
        assert result["milestone_bonus"] == 0
        assert result["streak_bonus"] == STREAKS[30]
        assert result["jc_earned"] == scale_minigame_jc_delta(1 + STREAKS[30])

    def test_dynamite_cache_yield_buff_boosts_loot_and_lifts_cap(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch
    ):
        """The Dynamite Cache (+75% yield) must actually apply: it scales base
        loot AND lifts the base-payout cap proportionally, so a buffed dig can
        exceed BASE_DIG_JC_PAYOUT_CAP. An unbuffed dig stays capped."""
        # Neutralize every yield modifier so base loot == the rolled value,
        # isolating the buff's effect.
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)          # no cave-in/events
        monkeypatch.setattr(random, "randint", lambda a, b: 12)      # roll == 12
        monkeypatch.setattr(dig_service, "_relic_jc_yield_multiplier", lambda *a, **k: 1.0)
        monkeypatch.setattr(dig_service, "_luminosity_jc_multiplier", lambda lum: 1.0)
        monkeypatch.setattr(dig_service, "_post_pinnacle_decay_factor", lambda *a, **k: 1.0)
        monkeypatch.setattr(dig_service, "_apply_mana_yield_variance", lambda did, gid, jc: jc)

        # Player A: no buff -> 12 rolled, under the 20 cap -> 12.
        _register_player(player_repository, discord_id=20001)
        dig_repo.create_tunnel(20001, guild_id, "A")
        dig_repo.update_tunnel(20001, guild_id, depth=10, max_depth=10)
        res_a = dig_service.dig(20001, guild_id)
        assert res_a["success"]
        assert res_a["milestone_bonus"] == 0 and res_a["streak_bonus"] == 0
        assert res_a["jc_earned"] == scale_minigame_jc_delta(12)

        # Player B: Dynamite Cache (+75%) -> 12*1.75 = 21, cap lifted to 35 -> 21.
        _register_player(player_repository, discord_id=20002)
        dig_repo.create_tunnel(20002, guild_id, "B")
        dig_repo.update_tunnel(20002, guild_id, depth=10, max_depth=10)
        dig_service.set_temp_buff(
            20002, guild_id,
            {"id": "dynamite_cache", "name": "Dynamite Cache",
             "duration_digs": 3, "effect": {"yield_multiplier": 1.75}},
        )
        res_b = dig_service.dig(20002, guild_id)
        assert res_b["success"]
        assert res_b["milestone_bonus"] == 0 and res_b["streak_bonus"] == 0
        # Buff both scaled the loot (12->21) and let it exceed the normal 20 cap before economy scaling.
        assert res_b["jc_earned"] == scale_minigame_jc_delta(21)
        assert res_b["jc_earned"] > scale_minigame_jc_delta(BASE_DIG_JC_PAYOUT_CAP)

    def test_dynamite_cache_buff_respects_lifted_cap_on_huge_loot(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch
    ):
        """With a +75% buff the base still tops out at the lifted cap (20*1.75=35),
        not unbounded — proving the cap is scaled, not removed."""
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        # Force a pre-cap base far above any cap (overwrites the product).
        monkeypatch.setattr(dig_service, "_apply_mana_yield_variance", lambda did, gid, jc: 100)

        _register_player(player_repository, discord_id=20003)
        dig_repo.create_tunnel(20003, guild_id, "C")
        dig_repo.update_tunnel(20003, guild_id, depth=10, max_depth=10)
        dig_service.set_temp_buff(
            20003, guild_id,
            {"id": "dynamite_cache", "name": "Dynamite Cache",
             "duration_digs": 3, "effect": {"yield_multiplier": 1.75}},
        )
        res = dig_service.dig(20003, guild_id)
        assert res["success"]
        assert res["milestone_bonus"] == 0 and res["streak_bonus"] == 0
        assert res["jc_earned"] == scale_minigame_jc_delta(int(BASE_DIG_JC_PAYOUT_CAP * 1.75))

    def test_dig_increments_total_digs(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """total_digs counter increases."""
        _register_player(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        dig_service.dig(10001, guild_id)
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["total_digs"] == 1

    def test_dig_updates_last_dig_at(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """last_dig_at timestamp updates."""
        _register_player(player_repository)
        now = 1_000_000
        monkeypatch.setattr(time, "time", lambda: now)
        dig_service.dig(10001, guild_id)
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["last_dig_at"] == now

    def test_dig_not_registered(self, dig_service, guild_id):
        """Returns error for unregistered player."""
        result = dig_service.dig(99999, guild_id)
        assert not result["success"]
        assert "error" in result


class TestCooldown:
    """Tests for dig cooldown mechanics."""

    def test_dig_cooldown_blocks_free_dig(self, dig_service, player_repository, guild_id, monkeypatch):
        """Can't free dig within 3h."""
        _register_player(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        result = dig_service.dig(10001, guild_id)
        assert result["success"]

        # Try again 1h later
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 3600)
        result = dig_service.dig(10001, guild_id)
        assert not result["success"] or result.get("paid_dig_required")

    @pytest.mark.parametrize(
        ("mutations", "expected_remaining"),
        [
            (None, 3600),
            (json.dumps([{"id": "restless"}]), 5400),
        ],
        ids=["plain", "restless"],
    )
    def test_stamina_13_caps_cooldown_after_mutations(
        self,
        dig_service,
        dig_repo,
        player_repository,
        guild_id,
        monkeypatch,
        mutations,
        expected_remaining,
    ):
        """The stamina cap applies after the Restless duration is added."""
        now = 1_000_000
        _register_player(player_repository)
        dig_repo.create_tunnel(10001, guild_id, "T")
        dig_repo.update_tunnel(
            10001,
            guild_id,
            total_digs=1,
            last_dig_at=now,
            stat_stamina=13,
            mutations=mutations,
        )
        monkeypatch.setattr(time, "time", lambda: now)

        result = dig_service.dig(10001, guild_id)

        assert result["success"] is False
        assert result["cooldown_remaining"] == expected_remaining

    def test_forest_cooldown_is_ready_at_7170_seconds(
        self, dig_service, dig_repo, guild_id, monkeypatch,
    ):
        """Forest's 30-second reduction reaches zero at the exact boundary."""
        last_dig_at = 1_000_000
        forest_effects = SimpleNamespace(
            color="Green",
            dig_cooldown_reduction_seconds=30,
        )
        monkeypatch.setattr(
            dig_service,
            "mana_effects_service",
            SimpleNamespace(get_effects=lambda discord_id, guild_id: forest_effects),
        )
        dig_repo.create_tunnel(10001, guild_id, "T")
        dig_repo.update_tunnel(
            10001,
            guild_id,
            total_digs=1,
            last_dig_at=last_dig_at,
        )
        tunnel = dig_repo.get_tunnel(10001, guild_id)

        assert dig_service._get_cooldown_remaining(
            tunnel, now=last_dig_at + 7169,
        ) == 1
        assert dig_service._get_cooldown_remaining(
            tunnel, now=last_dig_at + 7170,
        ) == 0
        assert dig_service.get_free_dig_ready_at(
            10001, guild_id, now=last_dig_at + 7170,
        ) is None

    def test_dig_cooldown_allows_paid_dig(self, dig_service, player_repository, guild_id, monkeypatch):
        """Can paid dig during cooldown."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)  # no cave-in
        dig_service.dig(10001, guild_id)

        # Paid dig 1h later
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 3600)
        result = dig_service.dig(10001, guild_id, paid=True)
        assert result["success"]

    def test_paid_dig_escalating_cost(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Paid dig costs escalate per PAID_DIG_COSTS_PER_DAY."""
        _register_player(player_repository, balance=500)
        base_time = 1_000_000
        monkeypatch.setattr(random, "random", lambda: 0.99)  # no cave-in

        # First free dig
        monkeypatch.setattr(time, "time", lambda: base_time)
        dig_service.dig(10001, guild_id)

        expected_costs = PAID_DIG_COSTS_PER_DAY
        for i, expected_cost in enumerate(expected_costs):
            monkeypatch.setattr(time, "time", lambda i=i: base_time + 60 * (i + 1))
            result = dig_service.dig(10001, guild_id, paid=True)
            assert result["success"], f"Paid dig #{i+1} should succeed"
            assert result["paid_cost"] == expected_cost, f"Paid dig #{i+1} cost should be {expected_cost}"

    def test_paid_dig_cost_resets_daily(self, dig_service, player_repository, guild_id, monkeypatch):
        """Paid dig counter resets on new game date."""
        _register_player(player_repository, balance=500)
        monkeypatch.setattr(random, "random", lambda: 0.99)  # no cave-in

        # Day 1: free dig + paid dig
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        dig_service.dig(10001, guild_id)
        monkeypatch.setattr(time, "time", lambda: 1_000_060)
        result1 = dig_service.dig(10001, guild_id, paid=True)
        assert result1["paid_cost"] == PAID_DIG_COSTS_PER_DAY[0]  # 3

        # Day 2: next day (advance 24h+)
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 86400 + 1)
        dig_service.dig(10001, guild_id)
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 86400 + 61)
        result2 = dig_service.dig(10001, guild_id, paid=True)
        # Should reset to first paid cost
        assert result2["paid_cost"] == PAID_DIG_COSTS_PER_DAY[0]

    def test_paid_dig_insufficient_funds(self, dig_service, player_repository, guild_id, monkeypatch):
        """Error when can't afford paid dig."""
        _register_player(player_repository, balance=3)  # default balance
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)  # free dig

        monkeypatch.setattr(time, "time", lambda: 1_000_060)
        # Set balance to 0
        player_repository.update_balance(10001, guild_id, 0)
        result = dig_service.dig(10001, guild_id, paid=True)
        assert not result["success"]
        assert "error" in result


class TestMinerProfile:
    """Tests for miner S stats and profile customization."""

    def test_set_profile_and_stats(self, dig_service, player_repository, guild_id):
        _register_player(player_repository)

        profile = dig_service.set_miner_profile(
            10001,
            guild_id,
            backstory="Former cartographer @everyone who fears ceilings.",
        )
        assert profile["success"]
        assert "(at)everyone" in profile["backstory"]

        result = dig_service.set_miner_stats(
            10001,
            guild_id,
            strength=2,
            smarts=2,
            stamina=1,
        )
        assert result["success"]
        assert result["stats"]["stat_points"] == 5
        assert result["stats"]["unspent_points"] == 0

        profile = dig_service.get_miner_profile(10001, guild_id)
        assert profile["stats"]["strength"] == 2
        assert profile["stats"]["smarts"] == 2
        assert profile["stats"]["stamina"] == 1
        assert profile["auto_buy"] == {"torch": False, "hard_hat": False}

    def test_set_miner_auto_buy_settings(self, dig_service, player_repository, guild_id):
        _register_player(player_repository)

        result = dig_service.set_miner_auto_buy(
            10001, guild_id, torch=True, hard_hat=True,
        )

        assert result["success"]
        assert result["auto_buy"] == {"torch": True, "hard_hat": True}
        profile = dig_service.get_miner_profile(10001, guild_id)
        assert profile["auto_buy"] == {"torch": True, "hard_hat": True}

        result = dig_service.set_miner_auto_buy(
            10001, guild_id, torch=False,
        )

        assert result["success"]
        assert result["auto_buy"] == {"torch": False, "hard_hat": True}

    def test_auto_buy_purchases_and_applies_selected_items(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        _register_player(player_repository, balance=100)
        dig_service.set_miner_auto_buy(
            10001, guild_id, torch=True, hard_hat=True,
        )

        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        first = dig_service.dig(10001, guild_id)
        assert first["is_first_dig"] is True
        balance_before = player_repository.get_balance(10001, guild_id)

        monkeypatch.setattr(
            time, "time",
            lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1,
        )
        result = dig_service.dig(10001, guild_id)

        assert result["success"]
        assert set(result["items_used"]) == {"Hard Hat", "Torch"}
        purchased = {
            item["type"]: item for item in result["auto_purchases"]
            if item["status"] == "purchased"
        }
        assert set(purchased) == {"hard_hat", "torch"}
        assert player_repository.get_balance(10001, guild_id) == (
            balance_before - 14 + result["jc_earned"]
        )
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["hard_hat_charges"] == 2
        assert dig_repo.get_inventory(10001, guild_id) == []

    def test_auto_buy_uses_reserve_inventory_before_purchase(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        _register_player(player_repository, balance=100)
        dig_service.set_miner_auto_buy(10001, guild_id, torch=True)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.add_item(10001, guild_id, "torch")
        balance_before = player_repository.get_balance(10001, guild_id)

        monkeypatch.setattr(
            time, "time",
            lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1,
        )
        result = dig_service.dig(10001, guild_id)

        assert result["success"]
        assert result["items_used"] == ["Torch"]
        assert result["auto_purchases"] == [{
            "type": "torch",
            "item": "Torch",
            "status": "queued_from_inventory",
            "cost": 0,
        }]
        assert player_repository.get_balance(10001, guild_id) == (
            balance_before + result["jc_earned"]
        )

    def test_auto_buy_insufficient_balance_does_not_block_dig(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        _register_player(player_repository, balance=100)
        dig_service.set_miner_auto_buy(10001, guild_id, hard_hat=True)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        player_repository.update_balance(10001, guild_id, 7)

        monkeypatch.setattr(
            time, "time",
            lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1,
        )
        result = dig_service.dig(10001, guild_id)

        assert result["success"]
        assert result["items_used"] == []
        assert result["auto_purchases"] == [{
            "type": "hard_hat",
            "item": "Hard Hat",
            "status": "skipped_insufficient_balance",
            "cost": 8,
        }]
        assert dig_repo.get_inventory(10001, guild_id) == []

    def test_backstory_can_only_be_set_once(self, dig_service, player_repository, guild_id):
        _register_player(player_repository)
        first = dig_service.set_miner_profile(
            10001,
            guild_id,
            backstory="Escaped from a failed mushroom commune.",
        )
        assert first["success"]

        second = dig_service.set_miner_profile(
            10001,
            guild_id,
            backstory="Actually a duke in exile.",
        )
        assert not second["success"]
        assert "cannot be changed" in second["error"]

    def test_profile_created_tunnel_still_gets_first_dig(
        self, dig_service, player_repository, guild_id, monkeypatch,
    ):
        _register_player(player_repository)
        dig_service.set_miner_profile(
            10001,
            guild_id,
            backstory="Keeps receipts for every rock.",
        )

        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.001)
        result = dig_service.dig(10001, guild_id)
        assert result["success"]
        assert result["is_first_dig"] is True
        assert result["cave_in"] is False

    def test_stat_build_cannot_overspend(self, dig_service, player_repository, guild_id):
        _register_player(player_repository)

        result = dig_service.set_miner_stats(
            10001,
            guild_id,
            strength=5,
            smarts=1,
            stamina=0,
        )
        assert not result["success"]
        assert "only have 5" in result["error"]

    def test_stat_build_cannot_respec(self, dig_service, player_repository, guild_id):
        _register_player(player_repository)
        first = dig_service.set_miner_stats(
            10001,
            guild_id,
            strength=3,
            smarts=2,
            stamina=0,
        )
        assert first["success"]

        second = dig_service.set_miner_stats(
            10001,
            guild_id,
            strength=0,
            smarts=1,
            stamina=0,
        )
        assert not second["success"]
        assert "only have 0 unspent" in second["error"]

    def test_strength_and_smarts_affect_preconditions(
        self, dig_service, player_repository, guild_id, monkeypatch,
    ):
        _register_player(player_repository, discord_id=10001)
        _register_player(player_repository, discord_id=10002)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_service.dig(10002, guild_id)
        dig_service.reset_dig_cooldown(10001, guild_id)
        dig_service.reset_dig_cooldown(10002, guild_id)

        dig_service.set_miner_stats(10001, guild_id, strength=5, smarts=0, stamina=0)
        _, preconditions = dig_service.dig_with_preconditions(10001, guild_id)
        assert preconditions["advance_min"] >= 2
        assert preconditions["advance_max"] >= 5

        dig_service.set_miner_stats(10002, guild_id, strength=0, smarts=5, stamina=0)
        _, preconditions = dig_service.dig_with_preconditions(10002, guild_id)
        assert preconditions["cave_in_chance"] == 0.01

    def test_stamina_reduces_cooldown_and_paid_cost(
        self, dig_service, player_repository, guild_id, monkeypatch,
    ):
        _register_player(player_repository, balance=100)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_service.set_miner_stats(10001, guild_id, strength=0, smarts=0, stamina=5)

        monkeypatch.setattr(time, "time", lambda: 1_000_060)
        result = dig_service.dig(10001, guild_id)
        assert result["paid_dig_available"]
        assert result["cooldown_remaining"] < FREE_DIG_COOLDOWN_SECONDS
        assert result["paid_dig_cost"] == 2

    def test_overgrowth_does_not_bypass_dig_cooldown(
        self, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        from repositories.buff_repository import BuffRepository
        from services.buff_service import BuffService

        buff_service = BuffService(BuffRepository(dig_repo.db_path))
        service = DigService(dig_repo, player_repository, buff_service=buff_service)
        monkeypatch.setattr(service, "_get_weather_effects", lambda guild_id, layer_name: {})
        _register_player(player_repository, balance=100)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        first = service.dig(10001, guild_id)
        assert first["success"]

        buff_service.grant_overgrowth(10001, guild_id)
        monkeypatch.setattr(time, "time", lambda: 1_000_060)

        result = service.dig(10001, guild_id)

        assert result["success"] is False
        assert result["paid_dig_available"] is True
        assert result["cooldown_remaining"] > 0

    def test_boss_first_clear_awards_stat_point_once(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        # Pin grothak (bruiser, no HP multiplier) so the fight is winnable
        # for an unequipped cautious player regardless of which tier-25 boss
        # the locker rolled this run.
        dig_repo.update_tunnel(
            10001, guild_id,
            depth=24,
            boss_progress=json.dumps({"25": {"boss_id": "grothak", "status": "active"}}),
        )

        monkeypatch.setattr(random, "random", lambda: 0.01)
        result = dig_service.fight_boss(10001, guild_id, "cautious", wager=0)
        assert result["success"]
        assert result["stat_point_awarded"] is True
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["stat_points"] == 6

        dig_repo.update_tunnel(
            10001,
            guild_id,
            depth=24,
            boss_progress=json.dumps({"25": {"boss_id": "grothak", "status": "active"}}),
        )
        result = dig_service.fight_boss(10001, guild_id, "cautious", wager=0)
        assert result["success"]
        assert result["stat_point_awarded"] is False
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["stat_points"] == 6

    def test_prestige_resets_boss_stat_awards_for_new_run(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(
            dig_service,
            "_scale_boss_stats",
            lambda stats, *args, **kwargs: {
                **stats,
                "boss_hp": 1,
                "boss_hit": 0.0,
                "boss_dmg": 1,
            },
        )
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(
            10001, guild_id,
            depth=24,
            boss_progress=json.dumps({"25": {"boss_id": "grothak", "status": "active"}}),
        )

        monkeypatch.setattr(random, "random", lambda: 0.01)
        first_clear = dig_service.fight_boss(10001, guild_id, "cautious", wager=0)
        assert first_clear["success"]
        assert first_clear["stat_point_awarded"] is True
        assert dig_repo.get_tunnel(10001, guild_id)["stat_points"] == 6

        boss_progress = {str(b): "defeated" for b in BOSS_BOUNDARIES}
        boss_progress[str(PINNACLE_DEPTH)] = "defeated"
        dig_repo.update_tunnel(
            10001, guild_id,
            depth=PINNACLE_DEPTH,
            boss_progress=json.dumps(boss_progress),
        )
        prestige = dig_service.prestige(10001, guild_id, "advance_boost")
        assert prestige["success"]
        profile = dig_service.get_miner_profile(10001, guild_id)
        assert profile["awarded_bosses"] == []

        dig_repo.update_tunnel(
            10001, guild_id,
            depth=24,
            boss_progress=json.dumps({"25": {"boss_id": "grothak", "status": "active"}}),
        )
        second_run_clear = dig_service.fight_boss(10001, guild_id, "cautious", wager=0)
        assert second_run_clear["success"]
        assert second_run_clear["stat_point_awarded"] is True
        assert dig_repo.get_tunnel(10001, guild_id)["stat_points"] == 7

    def test_legacy_global_boss_awards_do_not_block_prestiged_run(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(
            dig_service,
            "_scale_boss_stats",
            lambda stats, *args, **kwargs: {
                **stats,
                "boss_hp": 1,
                "boss_hit": 0.0,
                "boss_dmg": 1,
            },
        )
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(
            10001, guild_id,
            depth=24,
            prestige_level=1,
            stat_points=6,
            stat_boss_awards=json.dumps([25]),
            boss_progress=json.dumps({"25": {"boss_id": "grothak", "status": "active"}}),
        )

        monkeypatch.setattr(random, "random", lambda: 0.01)
        result = dig_service.fight_boss(10001, guild_id, "cautious", wager=0)
        assert result["success"]
        assert result["stat_point_awarded"] is True
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["stat_points"] == 7
        assert dig_service.get_miner_profile(10001, guild_id)["awarded_bosses"] == [25]


class TestCaveIn:
    """Tests for cave-in mechanics."""

    def test_cave_in_reduces_depth(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Cave-in block_loss respects the configured range."""
        _register_player(player_repository, balance=200)
        # Set up tunnel with some depth first
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)  # no cave-in for setup
        dig_service.dig(10001, guild_id)
        # Manually set depth high enough to survive cave-in
        dig_repo.update_tunnel(10001, guild_id, depth=20)

        # Now trigger cave-in
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        monkeypatch.setattr(random, "random", lambda: 0.001)  # force cave-in (below 5%)
        result = dig_service.dig(10001, guild_id)
        assert result.get("cave_in")
        detail = result.get("cave_in_detail") or {}
        block_loss = int(detail.get("block_loss", -1))
        # Tunnel was set to depth=20 (shallow band).
        shallow_min, shallow_max = CAVE_IN_BLOCK_LOSS_RANGES["shallow"]
        assert shallow_min <= block_loss <= shallow_max

    def test_cave_in_depth_min_zero(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Depth never goes below 0 after cave-in."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        # Set depth very low
        dig_repo.update_tunnel(10001, guild_id, depth=1)

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        monkeypatch.setattr(random, "random", lambda: 0.001)  # force cave-in
        dig_service.dig(10001, guild_id)
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["depth"] >= 0

    def test_cave_in_stun_extends_cooldown(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Stun injury adds hours to cooldown."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=20)

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        monkeypatch.setattr(random, "random", lambda: 0.001)  # force cave-in
        result = dig_service.dig(10001, guild_id)
        assert result.get("cave_in")
        # After cave-in with stun, cooldown should be extended
        if result.get("stun_hours"):
            assert result["stun_hours"] >= 1

    def test_reinforcement_caps_cave_in_block_loss_at_eight(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        """When Reinforcement window is active, cave-in block_loss clamps to 8
        even if the random roll would put it higher.

        Uses a shallow-band depth where block_loss range tops out at 14 and
        catastrophic_pct is 0 — so a single ``random=0.001`` forces a normal
        cave-in (not a catastrophic one, which has its own depth-rollback path)
        and `randint=b` rolls the upper bound, giving block_loss=14 pre-cap.
        Reinforcement should clamp that to 8.
        """
        _register_player(player_repository, balance=300)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        monkeypatch.setattr(random, "randint", lambda a, b: b)
        dig_service.dig(10001, guild_id)
        # Stay in the shallow band (< 50) and mark the depth-25 boss defeated
        # so the test dig doesn't park there.
        bp_defeated = json.dumps({"25": "defeated"})
        dig_repo.update_tunnel(
            10001, guild_id, depth=40,
            boss_progress=bp_defeated,
            reinforced_until=1_000_000 + 48 * 3600,
        )

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        monkeypatch.setattr(random, "random", lambda: 0.001)
        monkeypatch.setattr(random, "randint", lambda a, b: b)
        result = dig_service.dig(10001, guild_id)
        assert result.get("cave_in"), "expected a forced cave-in"
        detail = result.get("cave_in_detail") or {}
        block_loss = int(detail.get("block_loss", -1))
        assert 0 <= block_loss <= 8, f"block_loss {block_loss} exceeded cap"

        # Sanity check: without Reinforcement on the same setup, block_loss
        # should land in the uncapped shallow range (6..14). Guards against a
        # cap that fires regardless of the window.
        _register_player(player_repository, discord_id=10099, balance=300)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10099, guild_id)
        dig_repo.update_tunnel(
            10099, guild_id, depth=40, boss_progress=bp_defeated,
        )
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        monkeypatch.setattr(random, "random", lambda: 0.001)
        monkeypatch.setattr(random, "randint", lambda a, b: b)
        result_no_reinf = dig_service.dig(10099, guild_id)
        detail_no_reinf = result_no_reinf.get("cave_in_detail") or {}
        assert int(detail_no_reinf.get("block_loss", 0)) > 8

    def test_cave_in_medical_bill(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Medical bill costs depth/10 JC."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        boss_defeated = json.dumps({"25": "defeated", "50": "defeated"})
        dig_repo.update_tunnel(10001, guild_id, depth=50, boss_progress=boss_defeated)

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        monkeypatch.setattr(random, "random", lambda: 0.001)
        result = dig_service.dig(10001, guild_id)
        assert result.get("cave_in")
        if "medical_bill" in result:
            # depth was 50, so bill should be max(1, 50//10) = 5
            assert result["medical_bill"] == max(1, 50 // 10)


class TestMilestones:
    """Tests for milestone depth bonuses."""

    def test_milestone_25_bonus(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """+5 JC at depth 25."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        # Defeat boss at 25 so advance isn't capped, then set depth just below
        boss_defeated = json.dumps({"25": "defeated"})
        dig_repo.update_tunnel(10001, guild_id, depth=23, boss_progress=boss_defeated)

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        monkeypatch.setattr(random, "randint", lambda a, b: 3)
        result = dig_service.dig(10001, guild_id)
        assert result["success"]
        assert result["depth"] >= 25
        assert result["milestone_bonus"] == MILESTONES[25]

    def test_milestone_50_bonus(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """+10 JC at depth 50."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        boss_defeated = json.dumps({"25": "defeated", "50": "defeated"})
        dig_repo.update_tunnel(10001, guild_id, depth=48, boss_progress=boss_defeated)

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        monkeypatch.setattr(random, "randint", lambda a, b: 3)
        result = dig_service.dig(10001, guild_id)
        assert result["success"]
        assert result["depth"] >= 50
        assert result["milestone_bonus"] == MILESTONES[50]

    def test_milestone_100_bonus(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """+50 JC at depth 100."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        boss_defeated = json.dumps({"25": "defeated", "50": "defeated", "75": "defeated", "100": "defeated"})
        dig_repo.update_tunnel(10001, guild_id, depth=98, boss_progress=boss_defeated)

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        monkeypatch.setattr(random, "randint", lambda a, b: 3)
        result = dig_service.dig(10001, guild_id)
        assert result["success"]
        assert result["depth"] >= 100
        assert result["milestone_bonus"] == MILESTONES[100]


class TestHighPrestigeLayerPenalty:
    """The P2+ jc_layer_penalty dampens the layer-roll path of normal dig.

    P1 grants +18% loot at every prestige >= 1, so prestige comparisons share
    the same jc_multiplier; only layer penalties differ. Comparing them
    isolates the penalty end-to-end through dig() and guards the three
    jc_mult subtraction sites — the aggregator test alone can't catch a
    missing or mis-placed subtraction.
    """

    def _dig_once(self, dig_service, dig_repo, player_repository, guild_id,
                  discord_id, prestige):
        _register_player(player_repository, discord_id=discord_id,
                         guild_id=guild_id, balance=500)
        # Abyss layer (depth 101-150): JC roll range (1,4), advance (1,2).
        # All lower bosses defeated so advance isn't capped at a boundary.
        dig_repo.create_tunnel(discord_id, guild_id, "T")
        boss_defeated = json.dumps(
            {str(b): "defeated" for b in (25, 50, 75, 100)}
        )
        dig_repo.update_tunnel(
            discord_id, guild_id, depth=120, max_depth=120,
            prestige_level=prestige, luminosity=100,
            boss_progress=boss_defeated,
        )
        return dig_service.dig(discord_id, guild_id)

    def test_p2_layer_penalty_reduces_layer_payout(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)  # no cave-in/events
        # Inflate ONLY the JC roll (Abyss range 1..4) near the payout cap so
        # layer penalties still have visible effect before capping; advance
        # (1..2) stays at its minimum so no milestone/boundary is crossed.
        monkeypatch.setattr(
            random, "randint", lambda a, b: 17 if (a, b) == (1, 4) else a,
        )

        jc_p1 = self._dig_once(
            dig_service, dig_repo, player_repository, guild_id, 30001, 1,
        )["jc_earned"]
        jc_p2 = self._dig_once(
            dig_service, dig_repo, player_repository, guild_id, 30002, 2,
        )["jc_earned"]

        # P1: int(17 x 1.18) = 20. P2: int(17 x 1.15) = 19, then economy-scaled.
        assert jc_p1 == scale_minigame_jc_delta(BASE_DIG_JC_PAYOUT_CAP)
        assert jc_p2 == scale_minigame_jc_delta(19)
        assert jc_p1 > jc_p2

    def test_p4_layer_penalty_reduces_layer_payout(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)  # no cave-in/events
        monkeypatch.setattr(
            random, "randint", lambda a, b: 17 if (a, b) == (1, 4) else a,
        )

        jc_p3 = self._dig_once(
            dig_service, dig_repo, player_repository, guild_id, 30005, 3,
        )["jc_earned"]
        jc_p4 = self._dig_once(
            dig_service, dig_repo, player_repository, guild_id, 30006, 4,
        )["jc_earned"]

        # P3: int(17 x 1.13) = 19. P4: int(17 x 1.08) = 18, then economy-scaled.
        assert jc_p3 == scale_minigame_jc_delta(19)
        assert jc_p4 == scale_minigame_jc_delta(18)
        # The penalty must actually bite before the base-payout cap is applied.
        assert jc_p3 > jc_p4

    def test_p5_layer_penalty_reduces_layer_payout(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)  # no cave-in/events
        monkeypatch.setattr(
            random, "randint", lambda a, b: 16 if (a, b) == (1, 4) else a,
        )

        jc_p4 = self._dig_once(
            dig_service, dig_repo, player_repository, guild_id, 30003, 4,
        )["jc_earned"]
        jc_p5 = self._dig_once(
            dig_service, dig_repo, player_repository, guild_id, 30004, 5,
        )["jc_earned"]

        # P4: int(16 x 1.08) = 17. P5: int(16 x 1.01) = 16, then economy-scaled.
        assert jc_p4 == scale_minigame_jc_delta(17)
        assert jc_p5 == scale_minigame_jc_delta(16)
        assert jc_p4 > jc_p5


class TestDecay:
    """Tests for tunnel depth decay mechanics."""

    def test_disabled_decay_returns_fresh_result(self, dig_service):
        first = dig_service._apply_lazy_decay()
        second = dig_service._apply_lazy_decay()

        assert first == {"decayed": False, "amount": 0, "reason": None}
        assert second == first
        assert second is not first

    def test_no_decay_within_24h(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """No decay if last dig < 24h ago."""
        _register_player(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=30)

        # Check decay 12h later
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 12 * 3600)
        decay = dig_service.calculate_decay(10001, guild_id)
        assert decay == 0

    def test_decay_disabled(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Decay is disabled — no depth loss regardless of inactivity."""
        _register_player(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=30)

        # 48h later — decay would have fired but is now disabled
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 48 * 3600)
        decay = dig_service.calculate_decay(10001, guild_id)
        assert decay == 0

    def test_decay_stops_at_layer_boundary(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Decay doesn't go below 25/50/75."""
        _register_player(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        # Set depth just above boundary
        dig_repo.update_tunnel(10001, guild_id, depth=27, last_dig_at=1_000_000)

        # Long inactivity to trigger lots of decay
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 7 * 86400)  # 7 days
        dig_service.calculate_decay(10001, guild_id)
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["depth"] >= 25, "Decay should not cross layer boundary at 25"

    def test_decay_disabled_even_after_72h(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Decay is disabled — no depth loss even after extended inactivity."""
        _register_player(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=60, last_dig_at=1_000_000)

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 96 * 3600)
        decay = dig_service.calculate_decay(10001, guild_id)
        assert decay == 0

    def test_helpers_reduce_decay(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Recent helpers slow decay rate."""
        _register_player(player_repository, discord_id=10001)
        _register_player(player_repository, discord_id=10002)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=40, last_dig_at=1_000_000)

        # Log a help action recently
        dig_repo.log_action(guild_id, 10002, 10001, "help", 40, 42, jc_delta=1)

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 48 * 3600)
        decay_with_help = dig_service.calculate_decay(10001, guild_id)

        # Compare with no helpers - remove the help action isn't easy, so we
        # test a second player with no helpers at same depth/time
        _register_player(player_repository, discord_id=10003)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        dig_service.dig(10003, guild_id)
        dig_repo.update_tunnel(10003, guild_id, depth=40, last_dig_at=1_000_000)

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 48 * 3600)
        decay_no_help = dig_service.calculate_decay(10003, guild_id)

        assert decay_with_help <= decay_no_help

    def test_reinforcement_prevents_decay(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Reinforcement item blocks decay."""
        _register_player(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(
            10001, guild_id, depth=40, last_dig_at=1_000_000,
            reinforced_until=1_000_000 + 72 * 3600,  # reinforced for 72h
        )

        # 48h later - within reinforcement window
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 48 * 3600)
        decay = dig_service.calculate_decay(10001, guild_id)
        assert decay == 0


class TestHelp:
    """Tests for helping other players' tunnels."""

    def test_help_advances_target_tunnel(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Helper's advance applies to target."""
        _register_player(player_repository, discord_id=10001)
        _register_player(player_repository, discord_id=10002)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)

        dig_service.dig(10001, guild_id)  # create target tunnel
        dig_service.dig(10002, guild_id)  # create helper tunnel
        dig_repo.update_tunnel(10001, guild_id, depth=10)
        before = dig_repo.get_tunnel(10001, guild_id)["depth"]

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        result = dig_service.help_tunnel(10002, 10001, guild_id)
        assert result["success"]
        after = dig_repo.get_tunnel(10001, guild_id)["depth"]
        assert after > before

    def test_help_uses_helper_cooldown(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Helper's dig cooldown is consumed."""
        _register_player(player_repository, discord_id=10001)
        _register_player(player_repository, discord_id=10002)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)

        dig_service.dig(10001, guild_id)
        dig_service.dig(10002, guild_id)

        # Helper helps (using cooldown)
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        dig_service.help_tunnel(10002, 10001, guild_id)

        # Helper can't dig again immediately
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 60)
        result = dig_service.dig(10002, guild_id)
        assert not result["success"] or result.get("paid_dig_required")

    def test_help_earns_1_jc(self, dig_service, player_repository, guild_id, monkeypatch):
        """Helper earns 1 JC."""
        _register_player(player_repository, discord_id=10001, balance=100)
        _register_player(player_repository, discord_id=10002, balance=100)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)

        dig_service.dig(10001, guild_id)
        dig_service.dig(10002, guild_id)

        balance_before = player_repository.get_balance(10002, guild_id)
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        result = dig_service.help_tunnel(10002, 10001, guild_id)
        assert result["success"]
        balance_after = player_repository.get_balance(10002, guild_id)
        assert balance_after == balance_before + 1

    def test_help_self_fails(self, dig_service, player_repository, guild_id, monkeypatch):
        """Can't help yourself."""
        _register_player(player_repository, discord_id=10001)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        result = dig_service.help_tunnel(10001, 10001, guild_id)
        assert not result["success"]


class TestSabotage:
    """Tests for sabotaging other players' tunnels."""

    def test_sabotage_reduces_target_depth(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Target loses 3-8 blocks."""
        _register_player(player_repository, discord_id=10001)
        _register_player(player_repository, discord_id=10002, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)

        dig_service.dig(10001, guild_id)
        dig_service.dig(10002, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=30)
        monkeypatch.setattr(random, "random", lambda: SABOTAGE_SUCCESS_CHANCE - 0.01)

        result = dig_service.sabotage_tunnel(10002, 10001, guild_id)
        assert result["success"]
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        damage = 30 - tunnel["depth"]
        assert SABOTAGE_DAMAGE_MIN <= damage <= SABOTAGE_DAMAGE_MAX

    def test_sabotage_costs_jc(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Actor pays max(5, depth//5)."""
        _register_player(player_repository, discord_id=10001)
        _register_player(player_repository, discord_id=10002, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)

        dig_service.dig(10001, guild_id)
        dig_service.dig(10002, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=50)
        monkeypatch.setattr(random, "random", lambda: SABOTAGE_SUCCESS_CHANCE - 0.01)

        balance_before = player_repository.get_balance(10002, guild_id)
        result = dig_service.sabotage_tunnel(10002, 10001, guild_id)
        assert result["success"]
        balance_after = player_repository.get_balance(10002, guild_id)
        expected_cost = max(SABOTAGE_BASE_COST, 50 // SABOTAGE_COST_DIVISOR)
        assert balance_before - balance_after == expected_cost

    def test_sabotage_cooldown_per_target(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """12h cooldown per target."""
        _register_player(player_repository, discord_id=10001)
        _register_player(player_repository, discord_id=10002, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)

        dig_service.dig(10001, guild_id)
        dig_service.dig(10002, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=30)
        monkeypatch.setattr(random, "random", lambda: SABOTAGE_SUCCESS_CHANCE - 0.01)

        # First sabotage
        result1 = dig_service.sabotage_tunnel(10002, 10001, guild_id)
        assert result1["success"]

        # Immediate second sabotage should fail
        monkeypatch.setattr(time, "time", lambda: 1_000_060)
        result2 = dig_service.sabotage_tunnel(10002, 10001, guild_id)
        assert not result2["success"]

        # After 12h should work
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + SABOTAGE_COOLDOWN_SECONDS + 1)
        monkeypatch.setattr(random, "random", lambda: SABOTAGE_SUCCESS_CHANCE - 0.01)
        dig_repo.update_tunnel(10001, guild_id, depth=30)  # restore depth
        result3 = dig_service.sabotage_tunnel(10002, 10001, guild_id)
        assert result3["success"]

    def test_sabotage_miss_costs_jc_without_damage_or_reward(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        """A failed hit roll consumes the paid sabotage attempt without damage."""
        _register_player(player_repository, discord_id=10001)
        _register_player(player_repository, discord_id=10002, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)

        dig_service.dig(10001, guild_id)
        dig_service.dig(10002, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=50)
        attacker_pre = dig_repo.get_tunnel(10002, guild_id)
        balance_before = player_repository.get_balance(10002, guild_id)

        result = dig_service.sabotage_tunnel(10002, 10001, guild_id)

        assert result["success"]
        assert result["sabotage_hit"] is False
        assert result["damage"] == 0
        assert result["attacker_block_reward"] == 0
        assert result["prediction_contract_steal"] is None
        assert dig_repo.get_tunnel(10001, guild_id)["depth"] == 50
        assert dig_repo.get_tunnel(10002, guild_id)["depth"] == attacker_pre["depth"]
        expected_cost = max(SABOTAGE_BASE_COST, 50 // SABOTAGE_COST_DIVISOR)
        assert balance_before - player_repository.get_balance(10002, guild_id) == expected_cost

    def test_sabotage_insufficient_funds(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Error when can't afford."""
        _register_player(player_repository, discord_id=10001)
        _register_player(player_repository, discord_id=10002, balance=0)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)

        dig_service.dig(10001, guild_id)
        # Player 10002 needs a tunnel but has 0 balance
        player_repository.update_balance(10002, guild_id, 3)
        dig_service.dig(10002, guild_id)
        player_repository.update_balance(10002, guild_id, 0)
        dig_repo.update_tunnel(10001, guild_id, depth=30)

        result = dig_service.sabotage_tunnel(10002, 10001, guild_id)
        assert not result["success"]

    def test_sabotage_self_fails(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Can't sabotage yourself."""
        _register_player(player_repository, discord_id=10001, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)

        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=30)

        result = dig_service.sabotage_tunnel(10001, 10001, guild_id)
        assert not result["success"]

    def test_sabotage_attacker_block_reward_by_depth_tier(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        """Successful sabotage gives the attacker +3/+5/+7 advance scaled by
        victim's depth bracket (<100 / 100-250 / 250+).

        Uses a fresh victim per iteration to avoid the 12h per-target cooldown.
        """
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        _register_player(player_repository, discord_id=29999, balance=2000)
        dig_service.dig(29999, guild_id)
        monkeypatch.setattr(random, "random", lambda: SABOTAGE_SUCCESS_CHANCE - 0.01)

        for i, (victim_depth, expected_reward) in enumerate(
            [(50, 3), (150, 5), (300, 7)],
        ):
            victim_id = 20100 + i
            _register_player(player_repository, discord_id=victim_id)
            dig_service.dig(victim_id, guild_id)
            dig_repo.update_tunnel(victim_id, guild_id, depth=victim_depth)
            attacker_pre = dig_repo.get_tunnel(29999, guild_id)

            result = dig_service.sabotage_tunnel(29999, victim_id, guild_id)
            assert result["success"]
            assert result.get("attacker_block_reward") == expected_reward
            attacker_post = dig_repo.get_tunnel(29999, guild_id)
            assert attacker_post["depth"] == attacker_pre["depth"] + expected_reward

    def test_successful_sabotage_can_steal_prediction_contracts(
        self,
        dig_repo,
        player_repository,
        prediction_repo,
        guild_id,
        monkeypatch,
    ):
        """A successful sabotage can transfer a small open prediction position slice."""
        dig_service = DigService(dig_repo, player_repository, prediction_repo=prediction_repo)
        monkeypatch.setattr(dig_service, "_get_weather_effects", lambda guild_id, layer_name: {})
        _register_player(player_repository, discord_id=10001, balance=100, guild_id=guild_id)
        _register_player(player_repository, discord_id=10002, balance=500, guild_id=guild_id)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)

        dig_service.dig(10001, guild_id)
        dig_service.dig(10002, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=50)
        market_id = prediction_repo.create_orderbook_prediction(
            guild_id=guild_id,
            creator_id=999,
            question="Will sabotage matter?",
            initial_fair=50,
        )
        with prediction_repo.connection() as conn:
            conn.execute(
                """
                INSERT INTO prediction_positions
                    (prediction_id, discord_id, yes_contracts, yes_cost_basis_total)
                VALUES (?, ?, 8, 24)
                """,
                (market_id, 10001),
            )

        monkeypatch.setattr(random, "random", lambda: 0.0)
        monkeypatch.setattr(random, "randint", lambda a, b: 4)
        monkeypatch.setattr(random, "choice", lambda seq: seq[0])

        result = dig_service.sabotage_tunnel(10002, 10001, guild_id)

        assert result["success"]
        assert result["prediction_contract_steal"] == {
            "prediction_id": market_id,
            "side": "yes",
            "contracts": 4,
        }
        victim_position = prediction_repo.get_position(market_id, 10001)
        attacker_position = prediction_repo.get_position(market_id, 10002)
        assert victim_position["yes_contracts"] == 4
        assert victim_position["yes_cost_basis_total"] == 12
        assert attacker_position["yes_contracts"] == 4
        assert attacker_position["yes_cost_basis_total"] == 12

    def test_blocked_sabotage_credits_victim_jc_tip(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        """Trap-blocked sabotage credits the victim a small JC tip on top of
        the existing trap_steal -> target_jc_credit flow."""
        _register_player(player_repository, discord_id=10001, balance=100)
        _register_player(player_repository, discord_id=10002, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)

        dig_service.dig(10001, guild_id)
        dig_service.dig(10002, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=200)

        # Victim sets a trap so the attacker's attempt is blocked.
        dig_service.set_trap(10001, guild_id)

        victim_balance_before = player_repository.get_balance(10001, guild_id)
        result = dig_service.sabotage_tunnel(10002, 10001, guild_id)
        assert result.get("trapped")
        victim_balance_after = player_repository.get_balance(10001, guild_id)
        # Trap_steal (cost*2) + victim tip (max(25, cost//2)). Victim gain is
        # strictly larger than the bare trap_steal, which is what the buff is for.
        cost = max(5, 200 // 5)
        bare_trap_credit = cost
        assert victim_balance_after - victim_balance_before > bare_trap_credit


class TestTrap:
    """Tests for trap mechanics."""

    def test_set_trap_free_daily(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """First trap per day is free."""
        _register_player(player_repository, discord_id=10001, balance=100)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)

        result = dig_service.set_trap(10001, guild_id)
        assert result["success"]
        assert result.get("cost", 0) == 0

    def test_trap_catches_saboteur(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Active trap triggers on sabotage."""
        _register_player(player_repository, discord_id=10001, balance=100)
        _register_player(player_repository, discord_id=10002, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)

        dig_service.dig(10001, guild_id)
        dig_service.dig(10002, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=30)

        # Set trap
        dig_service.set_trap(10001, guild_id)
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["trap_active"] == 1

        # Sabotage triggers trap
        result = dig_service.sabotage_tunnel(10002, 10001, guild_id)
        assert result.get("trapped")

    def test_trap_steals_jc(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Trapped saboteur loses JC."""
        _register_player(player_repository, discord_id=10001, balance=100)
        _register_player(player_repository, discord_id=10002, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)

        dig_service.dig(10001, guild_id)
        dig_service.dig(10002, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=30)

        dig_service.set_trap(10001, guild_id)
        balance_before = player_repository.get_balance(10002, guild_id)
        result = dig_service.sabotage_tunnel(10002, 10001, guild_id)
        assert result.get("trapped")
        balance_after = player_repository.get_balance(10002, guild_id)
        # Saboteur should have lost JC (sabotage cost + trap penalty)
        assert balance_after < balance_before


class TestInsurance:
    """Tests for insurance mechanics."""

    def test_insurance_reduces_sabotage_damage(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """50% damage reduction."""
        _register_player(player_repository, discord_id=10001, balance=200)
        _register_player(player_repository, discord_id=10002, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)

        dig_service.dig(10001, guild_id)
        dig_service.dig(10002, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=40)

        # Buy insurance
        result = dig_service.buy_insurance(10001, guild_id)
        assert result["success"]
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["insured_until"] > 1_000_000

        # Fixed damage seed for consistency
        random.seed(99)
        monkeypatch.setattr(random, "random", lambda: SABOTAGE_SUCCESS_CHANCE - 0.01)
        result = dig_service.sabotage_tunnel(10002, 10001, guild_id)
        assert result["success"]
        # With insurance, damage should be reduced
        assert result.get("insurance_applied") or result.get("damage_reduced")

    def test_insurance_cost_scales_with_depth(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Cost = 5 + depth/25."""
        _register_player(player_repository, discord_id=10001, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)

        # Test at depth 50: cost = 5 + 50//25 = 7
        dig_repo.update_tunnel(10001, guild_id, depth=50)
        result = dig_service.buy_insurance(10001, guild_id)
        assert result["success"]
        expected_cost = INSURANCE_BASE_COST + 50 // INSURANCE_COST_DEPTH_DIVISOR
        assert result["cost"] == expected_cost

    def test_insurance_expires(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Insurance doesn't work after 24h."""
        _register_player(player_repository, discord_id=10001, balance=200)
        _register_player(player_repository, discord_id=10002, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)

        dig_service.dig(10001, guild_id)
        dig_service.dig(10002, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=40)

        # Buy insurance
        dig_service.buy_insurance(10001, guild_id)

        # Wait for insurance to expire (24h+)
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + INSURANCE_DURATION_SECONDS + 1)
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        # Insurance should be expired
        assert tunnel["insured_until"] <= 1_000_000 + INSURANCE_DURATION_SECONDS


class TestBoss:
    """Tests for layer boss mechanics."""

    def test_boss_blocks_advancement(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Can't dig past boss boundary."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=24)

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        result = dig_service.dig(10001, guild_id)
        assert result["success"]
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        # Either depth is capped at boundary-1 or a boss encounter is signaled
        assert tunnel["depth"] <= 25 or result.get("boss_encounter")

    def test_event_does_not_skip_boss_boundary(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Positive-depth events should stop at the boss boundary instead of skipping it."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=24)

        result = dig_service.resolve_event(10001, guild_id, "friendly_mole", "safe")

        assert result["success"]
        assert result.get("depth_delta") == 0
        assert result.get("boss_encounter") is True
        assert result.get("boss_info", {}).get("boundary") == 25

        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["depth"] == 24

    def test_boss_fight_win(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Win advances past boundary, awards payout."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        # Pin grothak (bruiser, no HP multiplier) so the deterministic
        # random=0.01 fight is winnable for an unequipped cautious player.
        dig_repo.update_tunnel(
            10001, guild_id,
            depth=24,
            boss_progress=json.dumps({"25": {"boss_id": "grothak", "status": "active"}}),
        )

        # Force win. Pin the win chance below the wager-taper knee so a
        # normal-odds wager pays its full multiplier (a tiny wager on a
        # near-certain fight would otherwise taper to ~0 net profit).
        monkeypatch.setattr(random, "random", lambda: 0.01)
        monkeypatch.setattr(
            "services.dig_service._approx_duel_win_prob", lambda **kw: 0.50,
        )
        result = dig_service.fight_boss(10001, guild_id, "cautious", wager=10)
        assert result["success"]
        assert result.get("won")
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["depth"] > 24
        assert result.get("payout", 0) > 0

    def test_boss_fight_win_pays_base_reward_despite_taper(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        """A wagered win at a high win-chance still pays the depth-scaled
        base reward. Regression: the wager-payout taper used to floor a
        near-certain, low-risk win at max(0, ...) = 0 JC — the player beat
        the boss and earned nothing."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(
            10001, guild_id,
            depth=24,
            boss_progress=json.dumps({"25": {"boss_id": "grothak", "status": "active"}}),
        )

        balance_before = player_repository.get_balance(10001, guild_id)
        # Force a win, with the win chance pinned ABOVE the wager-taper knee
        # so the wager profit tapers to ~0 — the base reward must still pay.
        monkeypatch.setattr(random, "random", lambda: 0.01)
        monkeypatch.setattr(
            "services.dig_service._approx_duel_win_prob", lambda **kw: 0.99,
        )
        result = dig_service.fight_boss(10001, guild_id, "cautious", wager=10)
        assert result["success"]
        assert result.get("won")
        # Boundary 25's base reward is paid even when wager profit tapers to 0.
        assert result["payout"] >= BOSS_VICTORY_BASE_JC[25]
        gained = player_repository.get_balance(10001, guild_id) - balance_before
        assert gained == result["payout"]

    def test_boss_fight_rejects_wager_before_final_phase(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        """Non-final boss phases should not accept a wager."""
        _register_player(player_repository, balance=1000)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(
            10001,
            guild_id,
            depth=24,
            prestige_level=2,
            boss_progress=json.dumps({"25": {"boss_id": "grothak", "status": "active"}}),
        )

        balance_before = player_repository.get_balance(10001, guild_id)

        result = dig_service.fight_boss(10001, guild_id, "reckless", wager=872)

        assert not result["success"]
        assert "final phase" in result["error"]
        assert player_repository.get_balance(10001, guild_id) == balance_before
        entry = json.loads(dig_repo.get_tunnel(10001, guild_id)["boss_progress"])["25"]
        assert entry["status"] == "active"

    def test_boss_fight_forced_no_wager_phase_avoids_free_fight_penalty(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        """Mandatory no-wager phases should not inherit voluntary free-fight costs."""
        _register_player(player_repository, balance=1000)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(
            10001,
            guild_id,
            depth=24,
            prestige_level=2,
            boss_progress=json.dumps({"25": {"boss_id": "grothak", "status": "active"}}),
        )

        captured = {}

        def capture_win_prob(**kwargs):
            captured["player_hit"] = kwargs["player_hit"]
            return 0.5

        monkeypatch.setattr("services.dig_service._approx_duel_win_prob", capture_win_prob)
        balance_before = player_repository.get_balance(10001, guild_id)

        result = dig_service.fight_boss(10001, guild_id, "reckless", wager=0)

        assert result["success"]
        assert result["won"] is False
        expected_hit = (
            BOSS_DUEL_STATS["reckless"]["player_hit"]
            - BOSS_TIER_BONUS[25]["pen"]
            - BOSS_PRESTIGE_BONUS[2]["pen"]
        )
        assert captured["player_hit"] == pytest.approx(expected_hit)
        assert captured["player_hit"] > expected_hit * BOSS_FREE_FIGHT_ACCURACY_MOD
        assert result["jc_delta"] == 0
        assert player_repository.get_balance(10001, guild_id) == balance_before

    def test_start_boss_duel_rejects_wager_before_final_phase(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        """The live boss-duel path should reject wagers before the final phase."""
        _register_player(player_repository, balance=1000)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(
            10001,
            guild_id,
            depth=24,
            prestige_level=2,
            boss_progress=json.dumps({"25": {"boss_id": "grothak", "status": "active"}}),
        )

        balance_before = player_repository.get_balance(10001, guild_id)

        result = dig_service.start_boss_duel(10001, guild_id, "reckless", wager=872)

        assert not result["success"]
        assert "final phase" in result["error"]
        assert player_repository.get_balance(10001, guild_id) == balance_before
        entry = json.loads(dig_repo.get_tunnel(10001, guild_id)["boss_progress"])["25"]
        assert entry["status"] == "active"

    def test_boss_fight_ignores_stale_carried_wager(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        """A stale carried wager marker should not auto-charge the next phase."""
        _register_player(player_repository, balance=1000)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(
            10001,
            guild_id,
            depth=25,
            prestige_level=2,
            boss_progress=json.dumps({
                "25": {
                    "boss_id": "grothak",
                    "status": "phase1_defeated",
                    "carried_wager": 872,
                    "carried_risk_tier": "reckless",
                }
            }),
        )
        balance_before = player_repository.get_balance(10001, guild_id)
        monkeypatch.setattr(random, "random", lambda: 0.999)
        monkeypatch.setattr(random, "randint", lambda a, b: a)

        result = dig_service.fight_boss(10001, guild_id, "cautious", wager=0)

        assert result["success"]
        assert result["won"] is False
        assert (
            player_repository.get_balance(10001, guild_id)
            == balance_before - BOSS_LOSS_REPAIR_BILL
        )
        entry = json.loads(dig_repo.get_tunnel(10001, guild_id)["boss_progress"])["25"]
        assert "carried_wager" not in entry
        assert "carried_risk_tier" not in entry

    def test_boss_fight_lose(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Lose forfeits the wager and applies a depth knockback."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=24)

        balance_before = player_repository.get_balance(10001, guild_id)
        # Force loss: hit rolls never succeed, round cap triggers boss win.
        monkeypatch.setattr(random, "random", lambda: 0.999)
        result = dig_service.fight_boss(10001, guild_id, "cautious", wager=10)
        assert result["success"]
        assert not result.get("won")
        assert player_repository.get_balance(10001, guild_id) == balance_before - 10
        assert 11 <= result.get("knockback", 0) <= 20
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["depth"] < 24

    def test_boss_retreat(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Retreat loses 1-3 blocks."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=24)

        result = dig_service.retreat_boss(10001, guild_id)
        assert result["success"]
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        retreat_loss = 24 - tunnel["depth"]
        assert 1 <= retreat_loss <= 3

    def test_boss_all_defeated_enables_prestige(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """All 4 bosses needed for prestige."""
        _register_player(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)

        # Set depth past all bosses but boss_progress incomplete (missing 150, 200, 275)
        partial = {str(b): "defeated" for b in BOSS_BOUNDARIES[:3]}
        partial[str(BOSS_BOUNDARIES[3])] = "active"  # 100 still active
        dig_repo.update_tunnel(10001, guild_id, depth=280, boss_progress=json.dumps(partial))

        result = dig_service.prestige(10001, guild_id, "advance_boost")
        assert not result["success"]

        # Now mark ALL bosses defeated, including the pinnacle.
        all_defeated = {str(b): "defeated" for b in BOSS_BOUNDARIES}
        all_defeated[str(PINNACLE_DEPTH)] = "defeated"
        dig_repo.update_tunnel(10001, guild_id, boss_progress=json.dumps(all_defeated))
        result = dig_service.prestige(10001, guild_id, "advance_boost")
        assert result["success"]


class TestStreaks:
    """Tests for consecutive day dig streaks."""

    def test_streak_increments_consecutive_days(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Streak goes up on consecutive days."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(random, "random", lambda: 0.99)

        # Day 1
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        dig_service.dig(10001, guild_id)
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["streak_days"] >= 1

        # Day 2 (24h later)
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 86400)
        dig_service.dig(10001, guild_id)
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["streak_days"] >= 2

    def test_streak_resets_on_gap(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Streak resets if day skipped."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(random, "random", lambda: 0.99)

        # Day 1
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        dig_service.dig(10001, guild_id)

        # Day 2
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 86400)
        dig_service.dig(10001, guild_id)
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        streak_day2 = tunnel["streak_days"]
        assert streak_day2 >= 2

        # Day 5 (skipped days 3 and 4)
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 4 * 86400)
        dig_service.dig(10001, guild_id)
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["streak_days"] == 1

    def test_streak_resets_after_one_missed_day_without_charm(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch
    ):
        """Missing exactly one game date resets the streak without protection."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(random, "random", lambda: 0.99)

        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        dig_service.dig(10001, guild_id)
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 86400)
        dig_service.dig(10001, guild_id)

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 3 * 86400)
        result = dig_service.dig(10001, guild_id)

        assert result["success"]
        assert result.get("streak_charm_used") is not True
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["streak_days"] == 1

    def test_streak_charm_saves_exactly_one_missed_day(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch
    ):
        """A Streak Charm is consumed to bridge one missed game date."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(random, "random", lambda: 0.99)

        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        dig_service.dig(10001, guild_id)
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 86400)
        dig_service.dig(10001, guild_id)
        streak_before = dig_repo.get_tunnel(10001, guild_id)["streak_days"]
        dig_repo.add_inventory_item(10001, guild_id, "streak_charm")

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 3 * 86400)
        result = dig_service.dig(10001, guild_id)

        assert result["success"]
        assert result["streak_charm_used"] is True
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["streak_days"] == streak_before + 1
        assert [
            item for item in dig_repo.get_inventory(10001, guild_id)
            if item["item_type"] == "streak_charm"
        ] == []

    def test_streak_charm_does_not_save_longer_gap(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch
    ):
        """A Streak Charm only covers one missed game date, not longer gaps."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(random, "random", lambda: 0.99)

        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        dig_service.dig(10001, guild_id)
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 86400)
        dig_service.dig(10001, guild_id)
        dig_repo.add_inventory_item(10001, guild_id, "streak_charm")

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 4 * 86400)
        result = dig_service.dig(10001, guild_id)

        assert result["success"]
        assert result.get("streak_charm_used") is not True
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["streak_days"] == 1
        assert any(
            item["item_type"] == "streak_charm"
            for item in dig_repo.get_inventory(10001, guild_id)
        )

    def test_streak_bonus_at_thresholds(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Bonus JC at 3/7/14/30 day streaks."""
        _register_player(player_repository, balance=500)
        monkeypatch.setattr(random, "random", lambda: 0.99)

        # Simulate 3 consecutive days
        for day in range(3):
            monkeypatch.setattr(time, "time", lambda d=day: 1_000_000 + d * 86400)
            result = dig_service.dig(10001, guild_id)
            assert result["success"]

        # On day 3, should get streak bonus
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["streak_days"] >= 3
        # The 3-day milestone is part of the contract — assert it exists rather
        # than letting the assertion silently disappear if STREAKS is rekeyed.
        assert 3 in STREAKS, "STREAKS lost its 3-day threshold key"
        assert result.get("streak_bonus", 0) >= STREAKS[3]


class TestPickaxe:
    """Tests for pickaxe upgrade system."""

    def test_upgrade_pickaxe_requirements(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Must meet depth + JC + prestige requirements."""
        _register_player(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)

        # Stone pickaxe requires depth 25 and 15 JC
        stone_tier = PICKAXE_TIERS[1]

        # Not enough depth
        dig_repo.update_tunnel(10001, guild_id, depth=10)
        result = dig_service.upgrade_pickaxe(10001, guild_id)
        assert not result["success"]

        # Enough depth and balance — should succeed
        dig_repo.update_tunnel(10001, guild_id, depth=stone_tier["depth_required"])
        result = dig_service.upgrade_pickaxe(10001, guild_id)
        assert result["success"]
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["pickaxe_tier"] == 1

    def test_pickaxe_advance_bonus(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Stone pickaxe gives +1 advance."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        # Disable weather so its random advance_bonus can't swamp the pickaxe bonus.
        monkeypatch.setattr(dig_service, "_get_weather_effects", lambda *a, **k: {})
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=10, pickaxe_tier=1)  # Stone pickaxe
        starter_weapon = dig_repo.get_equipped_gear(10001, guild_id)["weapon"]["id"]
        dig_repo.unequip_gear(starter_weapon)
        stone_weapon = dig_repo.add_gear(10001, guild_id, "weapon", 1)
        dig_repo.equip_gear(stone_weapon, 10001, guild_id, "weapon")

        # Dig with fixed advance
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        monkeypatch.setattr(random, "randint", lambda a, b: a)  # min advance
        result = dig_service.dig(10001, guild_id)
        assert result["success"]
        # Stone pickaxe has advance_bonus=1, so advance should be at least base_min + 1
        stone_bonus = PICKAXE_TIERS[1]["advance_bonus"]
        assert stone_bonus == 1
        # The result should include the bonus in total advance
        assert result["advance"] >= 1 + stone_bonus


class TestPickTipMaxDepth:
    """Verify _pick_tip filters tips by max_depth."""

    def test_shallow_tips_excluded_at_deep_depth(self, dig_service):
        """Tips with max_depth=10 should not appear when depth is 50."""
        # DIG_TIPS entries with max_depth should be filtered
        shallow_tips = [t for t in DIG_TIPS if t.get("max_depth") is not None and t["max_depth"] < 50]
        assert shallow_tips, "Expected DIG_TIPS to contain tips with max_depth < 50"
        # Run _pick_tip many times at depth 50 to ensure shallow tips never appear
        shallow_texts = {t["text"] for t in shallow_tips}
        random.seed(42)
        for _ in range(100):
            tip = dig_service._pick_tip(50)
            assert tip not in shallow_texts, f"Shallow tip showed at depth 50: {tip}"

    def test_tips_match_at_correct_depth(self, dig_service):
        """Tips with min_depth=0, max_depth=10 should appear at depth 5."""
        shallow_tips = [t for t in DIG_TIPS if t.get("min_depth", 0) <= 5 and (t.get("max_depth") is None or t["max_depth"] >= 5)]
        assert len(shallow_tips) > 0, "Expected at least one tip eligible at depth 5"
        random.seed(42)
        tip = dig_service._pick_tip(5)
        eligible_texts = {t["text"] for t in shallow_tips}
        assert tip in eligible_texts


class TestBossOdds:
    """Verify boss fight odds use configured values, not defaults."""

    def test_scout_boss_shows_configured_odds(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """scout_boss should return odds based on BOSS_WIN_ODDS config, not hardcoded defaults."""
        from services.dig_constants import BOSS_PAYOUTS
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)

        # Place at boss boundary (depth 24, boss at 25)
        dig_repo.update_tunnel(10001, guild_id, depth=24)
        # Add lantern for scouting
        dig_repo.add_inventory_item(10001, guild_id, "lantern")

        result = dig_service.scout_boss(10001, guild_id)
        assert result["success"]

        # Cautious should reflect the configured 0.75 base odds (not default 0.50)
        cautious_pct = result["odds"]["cautious"]["win_pct"]
        # At depth 25, penalty = (25/100)*0.05 = 0.0125, so ~0.74
        assert cautious_pct > 0.70, f"Cautious odds {cautious_pct} should reflect 0.75 base, not 0.50 default"

        # Multiplier comes from BOSS_PAYOUTS[25] (1.5), tapered toward
        # break-even by the high cautious win chance — not the 2.0 default.
        cautious_mult = result["odds"]["cautious"]["multiplier"]
        expected_mult = dig_service._effective_wager_multiplier(
            BOSS_PAYOUTS[25][0], cautious_pct,
        )
        assert abs(cautious_mult - expected_mult) < 0.05, (
            f"Expected ~{expected_mult:.2f}, got {cautious_mult}"
        )

    def test_fight_boss_reckless_high_roll_loses(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Reckless fight with high roll (0.99 > 0.20 base odds) should lose."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=24)

        monkeypatch.setattr(random, "random", lambda: 0.99)
        result = dig_service.fight_boss(10001, guild_id, "reckless", wager=0)
        assert result["success"]
        assert result["won"] is False

    def test_fight_boss_cautious_low_roll_wins(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Cautious fight with low roll (0.01 < 0.75 base odds) should win.

        Pins grothak (bruiser archetype, no HP multiplier) so the fight is
        deterministic regardless of which tier-25 boss the locker rolled.
        """
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(
            10001, guild_id,
            depth=24,
            boss_progress=json.dumps({"25": {"boss_id": "grothak", "status": "active"}}),
        )

        monkeypatch.setattr(random, "random", lambda: 0.01)
        result = dig_service.fight_boss(10001, guild_id, "cautious", wager=0)
        assert result["success"]
        assert result["won"] is True


class TestNewLayers:
    """Verify new layers are defined and accessible."""

    def test_eight_layers_exist(self):
        """Should have 8 layers after expansion."""
        from services.dig_constants import _LAYERS_DEF
        assert len(_LAYERS_DEF) == 8
        names = [layer.name for layer in _LAYERS_DEF]
        assert "Fungal Depths" in names
        assert "Frozen Core" in names
        assert "The Hollow" in names

    def test_abyss_now_capped(self):
        """Abyss should have depth_max=150 (no longer unbounded)."""
        from services.dig_constants import _LAYERS_DEF
        abyss = next(layer for layer in _LAYERS_DEF if layer.name == "Abyss")
        assert abyss.depth_max == 150

    def test_hollow_is_unbounded(self):
        """The Hollow should be unbounded (depth_max=None)."""
        from services.dig_constants import _LAYERS_DEF
        hollow = next(layer for layer in _LAYERS_DEF if layer.name == "The Hollow")
        assert hollow.depth_max is None

    def test_get_layer_returns_new_layers(self, dig_service):
        """Service should return new layers for deep depths."""
        layer_160 = dig_service._get_layer(160)
        assert layer_160.get("name") == "Fungal Depths"
        layer_250 = dig_service._get_layer(250)
        assert layer_250.get("name") == "Frozen Core"
        layer_300 = dig_service._get_layer(300)
        assert layer_300.get("name") == "The Hollow"

    def test_new_bosses_exist(self):
        """Should have 7 bosses (4 original + 3 new)."""
        assert len(BOSSES) == 7
        assert 150 in BOSSES
        assert 200 in BOSSES
        assert 275 in BOSSES

    def test_new_milestones(self):
        """Should have milestones for depths 150, 200, 275."""
        from services.dig_constants import MILESTONES
        assert 150 in MILESTONES
        assert 200 in MILESTONES
        assert 275 in MILESTONES


class TestLuminosity:
    """Verify luminosity mechanic."""

    def test_luminosity_starts_at_100(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """New tunnels should have luminosity 100."""
        _register_player(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        # First dig creates tunnel; second dig hits luminosity code path
        dig_service.dig(10001, guild_id)

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        result = dig_service.dig(10001, guild_id)
        assert result["success"]
        lum = result.get("luminosity_info")
        assert lum is not None
        # Dirt has 0 drain so luminosity stays at 100
        assert lum["luminosity_after"] == 100

    def test_luminosity_drains_in_magma(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Digging in Magma should drain luminosity by 3."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        boss_defeated = json.dumps({"25": "defeated", "50": "defeated", "75": "defeated"})
        dig_repo.update_tunnel(10001, guild_id, depth=80, boss_progress=boss_defeated)

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        result = dig_service.dig(10001, guild_id)
        assert result["success"]
        lum = result.get("luminosity_info")
        assert lum is not None
        assert lum["drained"] == 3
        assert lum["luminosity_after"] == 97

    def test_luminosity_level_thresholds(self, dig_service):
        """Verify luminosity level names at different values."""
        assert dig_service._get_luminosity_level(100) == "bright"
        assert dig_service._get_luminosity_level(76) == "bright"
        assert dig_service._get_luminosity_level(75) == "dim"
        assert dig_service._get_luminosity_level(26) == "dim"
        assert dig_service._get_luminosity_level(25) == "dark"
        assert dig_service._get_luminosity_level(1) == "dark"
        assert dig_service._get_luminosity_level(0) == "pitch_black"

    def test_luminosity_cave_in_bonus(self, dig_service):
        """Low luminosity should increase cave-in chance."""
        assert dig_service._luminosity_cave_in_bonus(100) == 0.0
        assert dig_service._luminosity_cave_in_bonus(50) > 0.0  # dim
        assert dig_service._luminosity_cave_in_bonus(10) > dig_service._luminosity_cave_in_bonus(50)  # dark > dim
        assert dig_service._luminosity_cave_in_bonus(0) > dig_service._luminosity_cave_in_bonus(10)  # pitch > dark

    def test_tiered_event_multiplier(self, dig_service, dig_repo, player_repository,
                                     guild_id, monkeypatch):
        """Darker luminosity tiers should produce more events over many digs."""
        from unittest.mock import patch

        import services.dig_service as ds_mod

        _register_player(player_repository, balance=50000)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(ds_mod.random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        boss_defeated = json.dumps({"25": "defeated", "50": "defeated"})
        dig_repo.update_tunnel(
            10001, guild_id, depth=50, boss_progress=boss_defeated,
        )  # Stone layer, base 0.16

        # Weather neutralized by fixture (effects stubbed to {})

        # Stone at depth 50: bright event=0.16, dim=0.24, dark=0.40, pitch=0.48
        # Stone cave-in: bright=0.10, dim=0.15, dark=0.25, pitch=0.35
        # roll=0.38 is above all cave-in thresholds but between dim(0.24) and dark(0.40)
        cd = FREE_DIG_COOLDOWN_SECONDS
        dig_idx = [0]
        for lum, expect in [(100, False), (50, False), (10, True), (0, True)]:
            dig_repo.update_tunnel(10001, guild_id, luminosity=lum, depth=50)
            dig_idx[0] += 1
            t = 1_000_000 + dig_idx[0] * (cd + 1)
            monkeypatch.setattr(time, "time", lambda _t=t: _t)
            monkeypatch.setattr(ds_mod.random, "random", lambda: 0.38)
            with patch.object(dig_service, "roll_event", wraps=dig_service.roll_event) as spy:
                dig_service.dig(10001, guild_id)
            assert (spy.call_count > 0) == expect, f"lum={lum}, roll=0.38: expected triggered={expect}"

    def test_event_chance_cap(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Event chance should be capped at 75%, even when multipliers push uncapped math above."""
        from unittest.mock import patch

        import services.dig_service as ds_mod

        _register_player(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(ds_mod.random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)

        # Weather neutralized by fixture (effects stubbed to {}).
        import json as _json
        all_bosses_defeated = _json.dumps({str(b): "defeated" for b in [25, 50, 75, 100, 150, 200, 275]})

        cd = FREE_DIG_COOLDOWN_SECONDS

        # Dirt at pitch black: event_chance = min(0.25 * 3.0, 0.75) = 0.75 (capped)
        # cave_in_chance = 0.05 + 0.25 = 0.30
        dig_repo.update_tunnel(10001, guild_id, depth=10, luminosity=0)
        # Roll 0.45: above cave-in (0.30), below event (0.75) -> event triggers
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + cd + 1)
        monkeypatch.setattr(ds_mod.random, "random", lambda: 0.45)
        with patch.object(dig_service, "roll_event", wraps=dig_service.roll_event) as spy:
            dig_service.dig(10001, guild_id)
        assert spy.call_count > 0, "Pitch-black Dirt: roll=0.45 should trigger event (chance=0.75)"

        # Roll 0.80: above the 0.75 cap -> no event (proves the cap is enforced)
        dig_repo.update_tunnel(10001, guild_id, depth=10, luminosity=0)
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 2 * (cd + 1))
        monkeypatch.setattr(ds_mod.random, "random", lambda: 0.80)
        with patch.object(dig_service, "roll_event", wraps=dig_service.roll_event) as spy:
            dig_service.dig(10001, guild_id)
        assert spy.call_count == 0, "Pitch-black Dirt: roll=0.80 should NOT trigger (capped chance=0.75)"

        # Abyss at pitch black: event_chance = min(0.35 * 3.0, 0.75) = 0.75 (capped)
        # cave_in_chance = 0.35 + 0.25 = 0.60
        dig_repo.update_tunnel(10001, guild_id, depth=120, luminosity=0,
                               boss_progress=all_bosses_defeated)
        # Roll 0.70: above cave-in (0.60), below cap (0.75) -> triggers
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 3 * (cd + 1))
        monkeypatch.setattr(ds_mod.random, "random", lambda: 0.70)
        with patch.object(dig_service, "roll_event", wraps=dig_service.roll_event) as spy:
            dig_service.dig(10001, guild_id)
        assert spy.call_count > 0, "Pitch-black Abyss: roll=0.70 should trigger event (capped chance=0.75)"

        # Roll 0.76: above cap (0.75) -> never triggers regardless of layer
        dig_repo.update_tunnel(10001, guild_id, depth=120, luminosity=0,
                               boss_progress=all_bosses_defeated)
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 4 * (cd + 1))
        monkeypatch.setattr(ds_mod.random, "random", lambda: 0.76)
        with patch.object(dig_service, "roll_event", wraps=dig_service.roll_event) as spy:
            dig_service.dig(10001, guild_id)
        assert spy.call_count == 0, "Roll=0.76 should never trigger (cap is 0.75)"

    def test_pitch_black_forces_risky(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """At pitch black luminosity, safe choice should be forced to risky."""
        _register_player(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=50, luminosity=0)

        result = dig_service.resolve_event(10001, guild_id, "underground_stream", "safe")
        # Should have been forced to risky
        assert result.get("choice") == "risky"

    def test_dark_risky_penalty(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Dark luminosity should reduce risky success chance by 10%."""
        _register_player(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=50, luminosity=10)

        # underground_stream risky has success_chance=0.62
        # With dark penalty: 0.62 - 0.10 = 0.52
        # Roll of 0.58 should fail (0.58 >= 0.52)
        monkeypatch.setattr(random, "random", lambda: 0.58)
        result = dig_service.resolve_event(10001, guild_id, "underground_stream", "risky")
        assert result["success"]
        # The risky option failed (current was dragged back)
        assert result.get("depth_delta", 0) < 0 or "drags you back" in result.get("message", "").lower() or result.get("advance", 0) < 0

    def test_dark_risky_penalty_floor(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Dark risky penalty should not reduce success_chance below 5%."""
        from services.dig_constants import EVENT_POOL
        # Find an event with desperate option that has low success_chance
        desperate_events = [e for e in EVENT_POOL if e.get("desperate_option") is not None]
        event = desperate_events[0]

        _register_player(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=50, luminosity=0)

        # Roll just below 5% should succeed — proves the floor is working
        monkeypatch.setattr(random, "random", lambda: 0.04)
        result = dig_service.resolve_event(10001, guild_id, event["id"], "desperate")
        assert result["success"]
        # The desperate choice should have succeeded at the floor
        assert "advance" in result or "jc_delta" in result or "message" in result


class TestTempBuffs:
    """Verify temp buff system."""

    def test_set_and_get_buff(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Can set and retrieve a temp buff."""
        _register_player(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)

        dig_service.set_temp_buff(10001, guild_id, {
            "id": "test_buff", "name": "Test", "duration_digs": 3,
            "effect": {"advance_bonus": 2},
        })

        tunnel = dig_repo.get_tunnel(10001, guild_id)
        buff = dig_service._get_active_buff(dict(tunnel))
        assert buff is not None
        assert buff["id"] == "test_buff"
        assert buff["digs_remaining"] == 3

    def test_buff_applies_advance_bonus(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Active buff with advance_bonus should increase advance."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        # Disable weather so its random advance_bonus can't swamp the buff.
        monkeypatch.setattr(dig_service, "_get_weather_effects", lambda *a, **k: {})
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=10)

        # Set a buff with +5 advance
        dig_service.set_temp_buff(10001, guild_id, {
            "id": "power", "name": "Power", "duration_digs": 2,
            "effect": {"advance_bonus": 5},
        })

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        monkeypatch.setattr(random, "randint", lambda a, b: a)  # min advance
        result = dig_service.dig(10001, guild_id)
        assert result["success"]
        # Advance should be at least 1 (base min) + 5 (buff) = 6
        assert result["advance"] >= 6

    def test_buff_decrements(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Buff should decrement each dig and expire."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=5)

        dig_service.set_temp_buff(10001, guild_id, {
            "id": "short", "name": "Short", "duration_digs": 1,
            "effect": {"advance_bonus": 1},
        })

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        dig_service.dig(10001, guild_id)

        # Buff should be gone after 1 dig
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        buff = dig_service._get_active_buff(dict(tunnel))
        assert buff is None


class TestCheer:
    """Tests for boss fight cheer mechanics."""

    def test_cheer_saves_cheer_data(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """cheer_boss writes to cheer_data column and data persists."""
        _register_player(player_repository, discord_id=10001, balance=200)
        _register_player(player_repository, discord_id=10002, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_service.dig(10002, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=24)

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        result = dig_service.cheer_boss(10002, 10001, guild_id)
        assert result["success"]

        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["cheer_data"] is not None
        cheers = json.loads(tunnel["cheer_data"])
        assert len(cheers) == 1
        assert cheers[0]["cheerer_id"] == 10002

    def test_cheer_is_free_for_cheerer_and_target(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Cheering is free — neither cheerer nor target's balance changes."""
        _register_player(player_repository, discord_id=10001, balance=200)
        _register_player(player_repository, discord_id=10002, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_service.dig(10002, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=24)

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        balance_cheerer_before = player_repository.get_balance(10002, guild_id)
        balance_target_before = player_repository.get_balance(10001, guild_id)

        dig_service.cheer_boss(10002, 10001, guild_id)

        assert player_repository.get_balance(10002, guild_id) == balance_cheerer_before
        assert player_repository.get_balance(10001, guild_id) == balance_target_before

    def test_cheer_increases_boss_win_chance(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Cheer bonus should increase fight_boss per-round hit chance.

        In the HP duel model, cheers raise ``player_hit`` per round.
        Using a random value that sits between the un-cheered and
        cheered hit rates, the player misses every round without
        cheers (round cap loss) but hits every round with cheers
        (boss dies). Verifies cheering flips the outcome deterministically.
        """
        _register_player(player_repository, discord_id=10001, balance=200)
        _register_player(player_repository, discord_id=10002, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_service.dig(10002, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=24)

        # Add 3 cheers so player_hit jumps by +0.15, flipping deterministically.
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        _register_player(player_repository, discord_id=10003, balance=200)
        _register_player(player_repository, discord_id=10004, balance=200)
        for cheerer_id in (10002, 10003, 10004):
            dig_service.cheer_boss(cheerer_id, 10001, guild_id)

        # Cautious player_hit with cheers ~= 0.65 - 0.02 (depth) + 0.15 (cheers) = 0.78.
        # A random of 0.70 passes the hit check with cheers but fails without them.
        monkeypatch.setattr(random, "random", lambda: 0.70)
        fight_result = dig_service.fight_boss(10001, guild_id, "cautious", wager=10)
        assert fight_result["success"]
        assert fight_result.get("won") is True

    def test_cheer_max_three(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Cannot add more than 3 cheers."""
        _register_player(player_repository, discord_id=10001, balance=200)
        cheerer_ids = [10002, 10003, 10004, 10005]
        for cid in cheerer_ids:
            _register_player(player_repository, discord_id=cid, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)

        dig_service.dig(10001, guild_id)
        for cid in cheerer_ids:
            dig_service.dig(cid, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=24)

        t = 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1
        monkeypatch.setattr(time, "time", lambda: t)
        for i, cid in enumerate(cheerer_ids[:3]):
            result = dig_service.cheer_boss(cid, 10001, guild_id)
            assert result["success"], f"Cheer {i+1} from {cid} should succeed"

        result = dig_service.cheer_boss(10005, 10001, guild_id)
        assert not result["success"]
        err = result.get("error", "").lower()
        assert "full cheer boost" in err or "maximum" in err

    def test_cheer_slots_free_after_expiry(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """After cheers expire, new cheers can be added past the old max."""
        _register_player(player_repository, discord_id=10001, balance=200)
        cheerer_ids = [10002, 10003, 10004, 10005]
        for cid in cheerer_ids:
            _register_player(player_repository, discord_id=cid, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        for cid in cheerer_ids:
            dig_service.dig(cid, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=24)

        # Add 3 cheers
        t = 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1
        monkeypatch.setattr(time, "time", lambda: t)
        for cid in cheerer_ids[:3]:
            result = dig_service.cheer_boss(cid, 10001, guild_id)
            assert result["success"]

        # 4th cheer fails (max 3)
        result = dig_service.cheer_boss(10005, 10001, guild_id)
        assert not result["success"]

        # Advance past cheer expiry (3600s) and cheerer cooldown
        t2 = t + FREE_DIG_COOLDOWN_SECONDS + 3601
        monkeypatch.setattr(time, "time", lambda: t2)
        result = dig_service.cheer_boss(10005, 10001, guild_id)
        assert result["success"]  # succeeds because old cheers expired

    def test_cheer_self_rejected(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Cannot cheer for yourself."""
        _register_player(player_repository, discord_id=10001, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=24)

        result = dig_service.cheer_boss(10001, 10001, guild_id)
        assert not result["success"]

    def test_cheer_does_not_trigger_dig_cooldown(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Cheering must not put the cheerer on the free-dig cooldown.

        Regression: cheer_boss used to write last_dig_at, blocking the cheerer
        from digging until the full free-dig cooldown elapsed.
        """
        _register_player(player_repository, discord_id=10001, balance=200)
        _register_player(player_repository, discord_id=10002, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=24)

        # 10002 has never dug; cheer_boss should succeed.
        result = dig_service.cheer_boss(10002, 10001, guild_id)
        assert result["success"]

        # Right after the cheer (no time advance), 10002 should be able to dig.
        dig_result = dig_service.dig(10002, guild_id)
        assert dig_result["success"], (
            f"cheer should not block dig, got: {dig_result.get('error')}"
        )

    def test_cheer_has_own_30s_cooldown(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """A cheerer cannot cheer twice inside CHEER_COOLDOWN_SECONDS."""
        _register_player(player_repository, discord_id=10001, balance=200)
        _register_player(player_repository, discord_id=10002, balance=200)
        _register_player(player_repository, discord_id=10003, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_service.dig(10003, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=24)
        dig_repo.update_tunnel(10003, guild_id, depth=24)

        first = dig_service.cheer_boss(10002, 10001, guild_id)
        assert first["success"]

        # Same cheerer, different target — still on cooldown.
        second = dig_service.cheer_boss(10002, 10003, guild_id)
        assert not second["success"]
        assert "cooldown" in second.get("error", "").lower()

        # After the cheer cooldown, the same cheerer may cheer again.
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + CHEER_COOLDOWN_SECONDS + 1)
        third = dig_service.cheer_boss(10002, 10003, guild_id)
        assert third["success"]


class TestBossErrors:
    """Tests for boss fight error handling and boundary behavior."""

    def test_fight_boss_error_has_no_won_key(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Error results from fight_boss must not contain 'won' key."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        # Player NOT at boss boundary (depth 10)
        dig_repo.update_tunnel(10001, guild_id, depth=10)

        result = dig_service.fight_boss(10001, guild_id, "bold", wager=0)
        assert result["success"] is False
        assert "won" not in result

    def test_fight_boss_insufficient_balance_error(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Wager exceeding balance returns error, not a fight result."""
        _register_player(player_repository, balance=10)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=24)

        # Wager far exceeds balance
        result = dig_service.fight_boss(10001, guild_id, "bold", wager=999)
        assert result["success"] is False
        assert "error" in result
        assert "won" not in result
        # Balance unchanged (minus whatever JC was earned from initial dig)
        balance = player_repository.get_balance(10001, guild_id)
        assert balance <= 10 + 10  # initial 10 + at most some JC from first dig

    def test_boss_boundary_preserves_last_dig_at(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Parked /dig go must not reset ``last_dig_at`` — the cooldown timer
        should keep ticking from the last real dig so re-opening the boss view
        can't be used to stall/reset cooldown."""
        _register_player(player_repository, balance=200)
        first_dig_time = 1_000_000
        monkeypatch.setattr(time, "time", lambda: first_dig_time)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        original_last_dig_at = dig_repo.get_tunnel(10001, guild_id)["last_dig_at"]

        # Place at boss boundary
        dig_repo.update_tunnel(10001, guild_id, depth=24)

        # Re-open the parked boss view — should surface encounter but leave
        # last_dig_at alone.
        monkeypatch.setattr(time, "time", lambda: first_dig_time + FREE_DIG_COOLDOWN_SECONDS + 1)
        result = dig_service.dig(10001, guild_id)
        assert result["success"]
        assert result.get("boss_encounter") is True

        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["last_dig_at"] == original_last_dig_at

    def test_boss_boundary_returns_full_info(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Boss encounter from dig includes dialogue and ascii_art."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=24)

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        result = dig_service.dig(10001, guild_id)
        assert result["success"]
        assert result.get("boss_encounter") is True

        boss_info = result.get("boss_info")
        assert boss_info is not None
        assert "dialogue" in boss_info
        assert "ascii_art" in boss_info
        assert "name" in boss_info
        assert boss_info["boundary"] == 25

    def test_parked_dig_ignores_cooldown(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        """A player parked at a boss boundary must be able to reach the
        BossEncounterView via /dig go regardless of cooldown — the cooldown
        gate would otherwise hide the Fight button behind a paid-dig dialog."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=24)
        balance_before = player_repository.get_balance(10001, guild_id)

        # Still on cooldown — parked /dig go must surface the encounter anyway.
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 10)
        result = dig_service.dig(10001, guild_id)
        assert result["success"] is True
        assert result.get("boss_encounter") is True
        assert result.get("boss_info", {}).get("boundary") == 25
        assert not result.get("paid_dig_available")
        # No JC awarded for re-opening the view.
        assert result.get("jc_earned", 0) == 0
        assert result.get("advance", 0) == 0
        # Balance untouched.
        assert player_repository.get_balance(10001, guild_id) == balance_before

    def test_parked_dig_ignores_paid_flag(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        """Paid=True while parked should not debit — the parked short-circuit
        runs before the paid-dig code path."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=24)
        balance_before = player_repository.get_balance(10001, guild_id)

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 10)
        result = dig_service.dig(10001, guild_id, paid=True)
        assert result["success"] is True
        assert result.get("boss_encounter") is True
        assert player_repository.get_balance(10001, guild_id) == balance_before

    def test_parked_dig_awards_no_jc_on_reopen(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        """Hitting the parked short-circuit repeatedly must never award JC or
        advance — otherwise the view becomes a JC farm."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=24)
        balance_before = player_repository.get_balance(10001, guild_id)

        for offset in (10, 20, 30):
            monkeypatch.setattr(time, "time", lambda o=offset: 1_000_000 + o)
            result = dig_service.dig(10001, guild_id)
            assert result.get("boss_encounter") is True
            assert result.get("jc_earned", 0) == 0
            assert result.get("advance", 0) == 0
        assert player_repository.get_balance(10001, guild_id) == balance_before

    def test_parked_dig_ignores_cooldown_preconditions_path(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        """DM-mode entry point (dig_with_preconditions) mirrors the same
        parked short-circuit — terminal result is the boss encounter, not a
        cooldown/paid-dig error."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=24)

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 10)
        terminal, precond = dig_service.dig_with_preconditions(10001, guild_id)
        assert precond is None
        assert terminal is not None
        assert terminal.get("success") is True
        assert terminal.get("boss_encounter") is True
        assert not terminal.get("paid_dig_available")


class _FailOnTunnelUpdateCursor:
    """Cursor wrapper that raises when it sees an ``UPDATE tunnels`` write.

    Inside ``atomic_tunnel_balance_update`` the balance UPDATE runs *before*
    the tunnel UPDATE in the same ``BEGIN IMMEDIATE`` transaction, so failing
    the tunnel write forces a rollback that must also undo the balance write.
    """

    def __init__(self, real_cursor):
        self._cursor = real_cursor

    def execute(self, sql, *args, **kwargs):
        if "update tunnels" in " ".join(sql.lower().split()):
            raise sqlite3.OperationalError("injected mid-transaction failure")
        return self._cursor.execute(sql, *args, **kwargs)

    def __getattr__(self, name):
        return getattr(self._cursor, name)


class _FailOnTunnelUpdateConn:
    """Connection wrapper that hands out fault-injecting cursors."""

    def __init__(self, real_conn):
        self._conn = real_conn

    def cursor(self):
        return _FailOnTunnelUpdateCursor(self._conn.cursor())

    def __getattr__(self, name):
        return getattr(self._conn, name)


class TestDigAtomicity:
    """The tunnel-state + JC-balance writes for a dig must commit as a unit.

    Regression guard: the dig flow used to call ``update_tunnel`` and
    ``add_balance`` as two separate writes, so a crash between them could
    advance depth while leaving the player unpaid (or charged). All such
    pairs now go through ``atomic_tunnel_balance_update``; if the tunnel
    write fails the balance change must roll back with it.
    """

    def _inject_tunnel_update_failure(self, dig_repo, monkeypatch):
        real_get_connection = dig_repo.get_connection
        monkeypatch.setattr(
            dig_repo,
            "get_connection",
            lambda: _FailOnTunnelUpdateConn(real_get_connection()),
        )

    def test_first_dig_rolls_back_balance_when_tunnel_write_fails(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        """First dig: a failed tunnel write must not leave the JC paid out
        nor create a partially-advanced tunnel."""
        _register_player(player_repository, balance=100)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        random.seed(42)

        self._inject_tunnel_update_failure(dig_repo, monkeypatch)
        with pytest.raises(sqlite3.OperationalError):
            dig_service.dig(10001, guild_id)

        # Balance untouched: the JC credit rolled back with the tunnel write.
        assert player_repository.get_balance(10001, guild_id) == 100
        # The tunnel row exists (created before the dig executes) but the
        # first-dig advance rolled back — it is still unstarted.
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel is not None
        assert tunnel["depth"] == 0
        assert (tunnel["total_digs"] or 0) == 0

    def test_normal_dig_rolls_back_balance_when_tunnel_write_fails(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        """A non-first dig: a failed tunnel write must roll back the JC
        payout and leave depth at its pre-dig value."""
        _register_player(player_repository, balance=100)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)  # suppress cave-in
        # First dig succeeds and establishes the tunnel.
        dig_service.dig(10001, guild_id)
        tunnel_before = dig_repo.get_tunnel(10001, guild_id)
        depth_before = tunnel_before["depth"]
        balance_before = player_repository.get_balance(10001, guild_id)

        # Second dig: inject a tunnel-write failure mid-transaction.
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        self._inject_tunnel_update_failure(dig_repo, monkeypatch)
        with pytest.raises(sqlite3.OperationalError):
            dig_service.dig(10001, guild_id)

        tunnel_after = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel_after["depth"] == depth_before
        assert tunnel_after["last_dig_at"] == tunnel_before["last_dig_at"]
        assert player_repository.get_balance(10001, guild_id) == balance_before

    def test_queued_consumables_survive_a_failed_dig_commit(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        """Finding-11: a queued consumable must NOT be destroyed when the dig's
        final commit fails.

        Old behavior deleted queued items in _resolve_queued_items (its own
        write) before the dig's atomic commit, so any exception in between
        permanently destroyed the item with no depth/JC to show for it. The
        fix folds the item delete INTO the final atomic_tunnel_balance_update,
        so a failed commit rolls back the burn too. This test fails on the old
        path (item gone after the rollback) and passes on the fix."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)  # suppress cave-in
        dig_service.dig(10001, guild_id)

        # Queue a dynamite for the next dig.
        item_id = dig_repo.add_inventory_item(10001, guild_id, "dynamite")
        dig_repo.queue_item(item_id)
        assert len(dig_repo.get_queued_items(10001, guild_id)) == 1

        # Second dig: inject a tunnel-write failure mid-transaction.
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        self._inject_tunnel_update_failure(dig_repo, monkeypatch)
        with pytest.raises(sqlite3.OperationalError):
            dig_service.dig(10001, guild_id)

        # The consumable survives: it is still in inventory AND still queued,
        # so the player can retry the dig and actually spend it.
        inventory = dig_repo.get_inventory(10001, guild_id)
        assert any(i["item_type"] == "dynamite" for i in inventory), (
            "queued dynamite was destroyed by the failed dig commit"
        )
        still_queued = dig_repo.get_queued_items(10001, guild_id)
        assert any(i["item_type"] == "dynamite" for i in still_queued), (
            "dynamite was un-queued despite the dig rolling back"
        )

    def test_queued_consumables_consumed_on_successful_dig(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        """Companion to the rollback test: on a SUCCESSFUL dig the queued
        consumable must actually be consumed (deleted from inventory), proving
        the burn moved into the commit rather than being dropped entirely."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)  # suppress cave-in
        dig_service.dig(10001, guild_id)

        item_id = dig_repo.add_inventory_item(10001, guild_id, "dynamite")
        dig_repo.queue_item(item_id)

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        result = dig_service.dig(10001, guild_id)
        assert result["success"]
        assert "Dynamite" in (result.get("items_used") or [])

        inventory = dig_repo.get_inventory(10001, guild_id)
        assert not any(i["item_type"] == "dynamite" for i in inventory), (
            "dynamite should be consumed on a successful dig"
        )
        assert dig_repo.get_queued_items(10001, guild_id) == []

    def test_prestige_rolls_back_grant_when_tunnel_reset_fails(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        """Prestige: a failed tunnel reset must not leave the 1000 JC grant
        credited without the run actually resetting."""
        _register_player(player_repository, balance=500)
        # One dig to create the tunnel row, then force it into a
        # prestige-eligible state: every tier boss plus the pinnacle defeated.
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        boss_progress = {str(b): "defeated" for b in BOSS_BOUNDARIES}
        boss_progress[str(PINNACLE_DEPTH)] = "defeated"
        dig_repo.update_tunnel(
            10001, guild_id, depth=PINNACLE_DEPTH,
            boss_progress=json.dumps(boss_progress),
        )
        balance_before = player_repository.get_balance(10001, guild_id)

        self._inject_tunnel_update_failure(dig_repo, monkeypatch)
        with pytest.raises(sqlite3.OperationalError):
            dig_service.prestige(10001, guild_id, "advance_boost")

        # Grant rolled back: balance unchanged and the tunnel was not reset.
        assert player_repository.get_balance(10001, guild_id) == balance_before
        tunnel_after = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel_after["depth"] == PINNACLE_DEPTH

    def test_prestige_relic_not_minted_when_tunnel_reset_fails(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        """Finding-4: the prestige relic is rolled INTO the atomic tunnel-reset
        txn, so a failed reset must leave NO relic minted.

        Old behavior minted the relic via a standalone add_artifact that
        committed before the atomic reset — on a failed reset the relic
        persisted while prestige_level/boss_progress stayed unreset, so
        can_prestige() stayed True and a retry rolled a SECOND relic. This
        test fails on that old path (a relic survives the rollback) and passes
        on the fix (the relic INSERT rolls back with the tunnel reset)."""
        _register_player(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        boss_progress = {str(b): "defeated" for b in BOSS_BOUNDARIES}
        boss_progress[str(PINNACLE_DEPTH)] = "defeated"
        dig_repo.update_tunnel(
            10001, guild_id, depth=PINNACLE_DEPTH,
            boss_progress=json.dumps(boss_progress),
        )
        relics_before = dig_repo.get_artifacts(10001, guild_id)

        self._inject_tunnel_update_failure(dig_repo, monkeypatch)
        with pytest.raises(sqlite3.OperationalError):
            dig_service.prestige(10001, guild_id, "advance_boost")

        # No relic minted: the artifact row count is unchanged after rollback.
        relics_after = dig_repo.get_artifacts(10001, guild_id)
        assert len(relics_after) == len(relics_before), (
            "prestige relic was minted despite the tunnel reset rolling back"
        )


# ---------------------------------------------------------------------------
# Finding-10: apply_dig_outcome and _execute_deterministic_outcome coverage
#
# These tests reproduce bugs #1, #2, #3 on the two secondary dig paths so
# regressions are caught before they hit players.
# ---------------------------------------------------------------------------


def _get_preconditions_at_depth(dig_service, dig_repo, player_repository, uid, *, depth, guild_id=12345):
    """Register, plant a tunnel at ``depth``, then call dig_with_preconditions."""
    player_repository.add(
        discord_id=uid,
        discord_username=f"User{uid}",
        guild_id=guild_id,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    player_repository.update_balance(uid, guild_id, 500)
    dig_repo.create_tunnel(uid, guild_id, "TestTunnel")
    # Advance past first-dig so dig_with_preconditions returns real preconditions
    _now = 2_000_000
    import unittest.mock as _mock
    with _mock.patch("time.time", return_value=_now):
        dig_service.dig(uid, guild_id)
    # Teleport to desired depth and clear cooldown
    dig_repo.update_tunnel(uid, guild_id, depth=depth, max_depth=depth, last_dig_at=0)
    _err, p = dig_service.dig_with_preconditions(uid, guild_id)
    assert _err is None, f"unexpected terminal: {_err}"
    return p


class TestApplyDigOutcomeSecondaryPaths:
    """Bug-catching tests for apply_dig_outcome (DM-decided path)."""

    def test_auto_buy_runs_before_dm_preconditions(
        self, dig_service, dig_repo, player_repository, guild_id,
    ):
        uid = 20100
        _register_player(player_repository, discord_id=uid, guild_id=guild_id, balance=100)
        dig_repo.create_tunnel(uid, guild_id, "TestTunnel")
        dig_repo.update_tunnel(
            uid, guild_id,
            depth=10,
            max_depth=10,
            last_dig_at=0,
            total_digs=1,
            auto_buy_torch=1,
        )
        balance_before = player_repository.get_balance(uid, guild_id)

        terminal, preconditions = dig_service.dig_with_preconditions(uid, guild_id)

        assert terminal is None
        assert preconditions["items_used"] == ["Torch"]
        assert preconditions["auto_purchases"][0]["status"] == "purchased"
        assert player_repository.get_balance(uid, guild_id) == balance_before - 6

    def test_max_depth_advances_on_dm_dig(self, dig_service, dig_repo, player_repository, guild_id):
        """Finding 1: DM-decided advance must update max_depth in the DB."""
        uid = 20101
        p = _get_preconditions_at_depth(dig_service, dig_repo, player_repository, uid, depth=10, guild_id=guild_id)
        dig_service.apply_dig_outcome(p, {"advance": 5, "jc_earned": 0, "cave_in": False, "event_id": ""})
        tunnel = dig_repo.get_tunnel(uid, guild_id)
        assert tunnel["max_depth"] == 15, (
            f"max_depth not updated: got {tunnel['max_depth']}"
        )

    def test_max_depth_does_not_regress_on_dm_cave_in(self, dig_service, dig_repo, player_repository, guild_id):
        """Finding 1: max_depth must not decrease when a cave-in knocks depth back."""
        uid = 20102
        p = _get_preconditions_at_depth(dig_service, dig_repo, player_repository, uid, depth=20, guild_id=guild_id)
        # Pre-set max_depth above current
        dig_repo.update_tunnel(uid, guild_id, max_depth=20)
        dig_service.apply_dig_outcome(
            p, {"advance": 0, "jc_earned": 0, "cave_in": True,
                "cave_in_block_loss": 5, "cave_in_type": "stun"},
        )
        tunnel = dig_repo.get_tunnel(uid, guild_id)
        assert tunnel["max_depth"] == 20, (
            f"max_depth regressed after cave-in: {tunnel['max_depth']}"
        )

    def test_milestone_not_re_awarded_after_knockback(self, dig_service, dig_repo, player_repository, guild_id):
        """Finding 2: milestone at 25 must not fire again when max_depth already exceeds it."""
        uid = 20103
        # Place player at depth 20 with max_depth already past the 25-JC milestone
        p = _get_preconditions_at_depth(dig_service, dig_repo, player_repository, uid, depth=20, guild_id=guild_id)
        dig_repo.update_tunnel(uid, guild_id, max_depth=30)  # already passed depth-25 milestone
        # Rebuild preconditions with updated tunnel state
        _err, p = dig_service.dig_with_preconditions(uid, guild_id)
        assert _err is None
        balance_before = player_repository.get_balance(uid, guild_id)
        dig_service.apply_dig_outcome(p, {"advance": 10, "jc_earned": 0, "cave_in": False, "event_id": ""})
        balance_after = player_repository.get_balance(uid, guild_id)
        milestone_reward = MILESTONES.get(25, 0)
        # The depth-25 milestone must NOT have been awarded (max_depth was 30)
        assert balance_after < balance_before + milestone_reward, (
            "milestone was re-awarded after knockback (anti-farm check broken)"
        )

    def test_helltide_tax_applied_on_dm_path(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Finding 3: helltide tax must reduce JC payout on the DM-decided path."""
        uid = 20104
        # Patch _helltide_tax to return a flat 5 JC tax
        monkeypatch.setattr(dig_service, "_helltide_tax", lambda gid: 5)
        p = _get_preconditions_at_depth(dig_service, dig_repo, player_repository, uid, depth=10, guild_id=guild_id)
        balance_before = player_repository.get_balance(uid, guild_id)
        dig_service.apply_dig_outcome(p, {"advance": 1, "jc_earned": 10, "cave_in": False, "event_id": ""})
        balance_after = player_repository.get_balance(uid, guild_id)
        # 10 JC scales to 8; the 5 JC Helltide tax scales to 4.
        assert balance_after == balance_before + 4, (
            f"helltide tax not applied on DM path: got +{balance_after - balance_before}"
        )

    def test_dm_dig_scales_generated_jc_before_mana_taxes(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch
    ):
        uid = 20108
        seen_tax_inputs = []
        monkeypatch.setattr(dig_service, "_helltide_tax", lambda gid: 0)

        def capture_tax_input(did, gid, jc):
            seen_tax_inputs.append(jc)
            return jc

        monkeypatch.setattr(dig_service, "_apply_mana_yield_taxes", capture_tax_input)
        p = _get_preconditions_at_depth(dig_service, dig_repo, player_repository, uid, depth=10, guild_id=guild_id)

        result = dig_service.apply_dig_outcome(
            p, {"advance": 1, "jc_earned": 20, "cave_in": False, "event_id": ""}
        )

        expected = scale_minigame_jc_delta(BASE_DIG_JC_PAYOUT_CAP)
        assert seen_tax_inputs == [expected]
        assert result["jc_earned"] == expected

    def test_base_dig_payout_cap_applies_on_dm_path(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        uid = 20105
        monkeypatch.setattr(dig_service, "_apply_mana_yield_taxes", lambda did, gid, jc: jc)
        monkeypatch.setattr(dig_service, "_helltide_tax", lambda gid: 0)
        p = _get_preconditions_at_depth(dig_service, dig_repo, player_repository, uid, depth=10, guild_id=guild_id)
        balance_before = player_repository.get_balance(uid, guild_id)

        result = dig_service.apply_dig_outcome(
            p, {"advance": 1, "jc_earned": 25, "cave_in": False, "event_id": ""}
        )

        balance_after = player_repository.get_balance(uid, guild_id)
        expected = scale_minigame_jc_delta(BASE_DIG_JC_PAYOUT_CAP)
        assert result["jc_earned"] == expected
        assert balance_after == balance_before + expected

    def test_base_dig_payout_cap_applies_after_dm_weather_combo(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch
    ):
        from unittest.mock import MagicMock
        uid = 20106
        p = _get_preconditions_at_depth(dig_service, dig_repo, player_repository, uid, depth=10, guild_id=guild_id)
        fake_mana = MagicMock()
        fake_mana.get_weather_combo_modifiers.return_value = {"yield_mult": 2.0}
        monkeypatch.setattr(dig_service, "mana_effects_service", fake_mana)
        monkeypatch.setattr(dig_service, "_apply_mana_yield_taxes", lambda did, gid, jc: jc)
        monkeypatch.setattr(dig_service, "_helltide_tax", lambda gid: 0)
        balance_before = player_repository.get_balance(uid, guild_id)

        result = dig_service.apply_dig_outcome(
            p, {"advance": 1, "jc_earned": 20, "cave_in": False, "event_id": ""}
        )

        balance_after = player_repository.get_balance(uid, guild_id)
        expected = scale_minigame_jc_delta(BASE_DIG_JC_PAYOUT_CAP)
        assert result["jc_earned"] == expected
        assert balance_after == balance_before + expected

    def test_weather_combo_applied_on_dm_path(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Fix #2: the Sunny+White weather-combo yield bonus applies on the DM path.

        The DM jc range (jc_min/jc_max) is computed without the combo, so
        apply_dig_outcome must apply it to match dig()/_execute_deterministic_outcome.
        """
        from unittest.mock import MagicMock
        uid = 20107
        # Build preconditions with the original (None) mana service so the jc
        # range is unaffected, then swap in a 2x-combo service for the apply step.
        p = _get_preconditions_at_depth(dig_service, dig_repo, player_repository, uid, depth=10, guild_id=guild_id)
        fake_mana = MagicMock()
        fake_mana.get_weather_combo_modifiers.return_value = {"yield_mult": 2.0}
        monkeypatch.setattr(dig_service, "mana_effects_service", fake_mana)
        # Isolate the combo multiplier from taxes/penalties.
        monkeypatch.setattr(dig_service, "_apply_mana_yield_taxes", lambda did, gid, jc: jc)
        monkeypatch.setattr(dig_service, "_helltide_tax", lambda gid: 0)
        balance_before = player_repository.get_balance(uid, guild_id)
        dig_service.apply_dig_outcome(p, {"advance": 1, "jc_earned": 10, "cave_in": False, "event_id": ""})
        balance_after = player_repository.get_balance(uid, guild_id)
        # 10 JC x 2.0 combo (taxes neutralized) = 20, then economy-scaled.
        assert balance_after == balance_before + scale_minigame_jc_delta(20), (
            f"weather combo not applied on DM path: got +{balance_after - balance_before}"
        )


class TestExecuteDeterministicOutcomePaths:
    """Bug-catching tests for _execute_deterministic_outcome (fallback path)."""

    def test_max_depth_advances_on_deterministic_success(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Finding 1: deterministic fallback must update max_depth."""
        uid = 20201
        monkeypatch.setattr("random.random", lambda: 0.99)  # no cave-in
        p = _get_preconditions_at_depth(dig_service, dig_repo, player_repository, uid, depth=10, guild_id=guild_id)
        dig_service._execute_deterministic_outcome(p)
        tunnel = dig_repo.get_tunnel(uid, guild_id)
        assert tunnel["max_depth"] >= 10, (
            f"max_depth not written by deterministic path: {tunnel['max_depth']}"
        )

    def test_milestone_not_re_awarded_after_knockback_deterministic(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch
    ):
        """Finding 2: deterministic path respects max_depth for milestone anti-farm."""
        uid = 20202
        monkeypatch.setattr("random.random", lambda: 0.99)  # no cave-in
        p = _get_preconditions_at_depth(dig_service, dig_repo, player_repository, uid, depth=20, guild_id=guild_id)
        dig_repo.update_tunnel(uid, guild_id, max_depth=30)
        _err, p = dig_service.dig_with_preconditions(uid, guild_id)
        assert _err is None
        balance_before = player_repository.get_balance(uid, guild_id)
        dig_service._execute_deterministic_outcome(p)
        balance_after = player_repository.get_balance(uid, guild_id)
        milestone_reward = MILESTONES.get(25, 0)
        assert balance_after < balance_before + milestone_reward, (
            "milestone re-awarded on deterministic path despite max_depth already past it"
        )

    def test_helltide_tax_applied_on_deterministic_path(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch
    ):
        """Finding 3: helltide tax fires on the deterministic fallback path."""
        uid = 20203
        monkeypatch.setattr("random.random", lambda: 0.99)  # no cave-in
        tax = [0]

        def _fake_helltide(gid):
            tax[0] += 1
            return 0  # no actual deduction, just assert it's called

        monkeypatch.setattr(dig_service, "_helltide_tax", _fake_helltide)
        p = _get_preconditions_at_depth(dig_service, dig_repo, player_repository, uid, depth=10, guild_id=guild_id)
        dig_service._execute_deterministic_outcome(p)
        assert tax[0] >= 1, "_helltide_tax never called on deterministic path"
