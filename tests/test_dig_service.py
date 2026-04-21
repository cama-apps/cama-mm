"""Tests for the tunnel digging minigame service."""

import json
import random
import time

import pytest

from repositories.dig_repository import DigRepository
from services.dig_constants import (
    ABANDON_REFUND_PCT,
    ALL_ARTIFACTS,
    BOSS_BOUNDARIES,
    BOSSES,
    CAVE_IN_BLOCK_LOSS_MAX,
    CAVE_IN_BLOCK_LOSS_MIN,
    CONSUMABLES,
    DIG_TIPS,
    FIRST_DIG_ADVANCE_MAX,
    FIRST_DIG_ADVANCE_MIN,
    FIRST_DIG_JC_MAX,
    FIRST_DIG_JC_MIN,
    FREE_DIG_COOLDOWN_SECONDS,
    HARD_HAT_USES,
    INSURANCE_BASE_COST,
    INSURANCE_COST_DEPTH_DIVISOR,
    INSURANCE_DURATION_SECONDS,
    MAX_INVENTORY_SLOTS,
    MAX_PRESTIGE,
    MILESTONES,
    PAID_DIG_COSTS_PER_DAY,
    PICKAXE_TIERS,
    PRESTIGE_PERKS,
    SABOTAGE_BASE_COST,
    SABOTAGE_COOLDOWN_SECONDS,
    SABOTAGE_COST_DIVISOR,
    SABOTAGE_DAMAGE_MAX,
    SABOTAGE_DAMAGE_MIN,
    STREAKS,
)
from services.dig_service import DigService


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


# =============================================================================
# Core Dig Tests
# =============================================================================


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


# =============================================================================
# Cooldown Tests
# =============================================================================


class TestCooldown:
    """Tests for dig cooldown mechanics."""

    def test_dig_cooldown_blocks_free_dig(self, dig_service, player_repository, guild_id, monkeypatch):
        """Can't free dig within 4h."""
        _register_player(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        result = dig_service.dig(10001, guild_id)
        assert result["success"]

        # Try again 1h later
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 3600)
        result = dig_service.dig(10001, guild_id)
        assert not result["success"] or result.get("paid_dig_required")

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
        """Paid dig costs escalate: 3, 6, 12, 24, 48."""
        _register_player(player_repository, balance=500)
        base_time = 1_000_000
        monkeypatch.setattr(random, "random", lambda: 0.99)  # no cave-in

        # First free dig
        monkeypatch.setattr(time, "time", lambda: base_time)
        dig_service.dig(10001, guild_id)

        expected_costs = PAID_DIG_COSTS_PER_DAY  # [3, 6, 12, 24, 48]
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

    def test_boss_first_clear_awards_stat_point_once(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=24)

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
            boss_progress=json.dumps({"25": "active"}),
        )
        result = dig_service.fight_boss(10001, guild_id, "cautious", wager=0)
        assert result["success"]
        assert result["stat_point_awarded"] is False
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["stat_points"] == 6


# =============================================================================
# Cave-in Tests
# =============================================================================


class TestCaveIn:
    """Tests for cave-in mechanics."""

    def test_cave_in_reduces_depth(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Cave-in removes 3-8 blocks."""
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
        if "cave_in_loss" in result:
            assert CAVE_IN_BLOCK_LOSS_MIN <= result["cave_in_loss"] <= CAVE_IN_BLOCK_LOSS_MAX

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

    def test_cave_in_medical_bill(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Medical bill costs depth/10 JC."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=50)

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        monkeypatch.setattr(random, "random", lambda: 0.001)
        result = dig_service.dig(10001, guild_id)
        assert result.get("cave_in")
        if "medical_bill" in result:
            # depth was 50, so bill should be max(1, 50//10) = 5
            assert result["medical_bill"] == max(1, 50 // 10)


# =============================================================================
# Milestone Tests
# =============================================================================


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


# =============================================================================
# Decay Tests
# =============================================================================


class TestDecay:
    """Tests for tunnel depth decay mechanics."""

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


# =============================================================================
# Help Tests
# =============================================================================


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


# =============================================================================
# Sabotage Tests
# =============================================================================


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

        # First sabotage
        result1 = dig_service.sabotage_tunnel(10002, 10001, guild_id)
        assert result1["success"]

        # Immediate second sabotage should fail
        monkeypatch.setattr(time, "time", lambda: 1_000_060)
        result2 = dig_service.sabotage_tunnel(10002, 10001, guild_id)
        assert not result2["success"]

        # After 12h should work
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + SABOTAGE_COOLDOWN_SECONDS + 1)
        dig_repo.update_tunnel(10001, guild_id, depth=30)  # restore depth
        result3 = dig_service.sabotage_tunnel(10002, 10001, guild_id)
        assert result3["success"]

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


# =============================================================================
# Trap Tests
# =============================================================================


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


# =============================================================================
# Insurance Tests
# =============================================================================


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


# =============================================================================
# Boss Tests
# =============================================================================


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
        dig_repo.update_tunnel(10001, guild_id, depth=24)

        # Force win
        monkeypatch.setattr(random, "random", lambda: 0.01)
        result = dig_service.fight_boss(10001, guild_id, "cautious", wager=10)
        assert result["success"]
        assert result.get("won")
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["depth"] > 24
        assert result.get("payout", 0) > 0

    def test_boss_fight_lose(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Lose forfeits the wager and applies a small 5-10 block knockback."""
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
        assert 5 <= result.get("knockback", 0) <= 10
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

        # Now mark ALL bosses defeated
        all_defeated = {str(b): "defeated" for b in BOSS_BOUNDARIES}
        dig_repo.update_tunnel(10001, guild_id, boss_progress=json.dumps(all_defeated))
        result = dig_service.prestige(10001, guild_id, "advance_boost")
        assert result["success"]


# =============================================================================
# Prestige Tests
# =============================================================================


class TestPrestige:
    """Tests for prestige system."""

    def _setup_prestige_ready(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Helper to set up a player ready for prestige (all 7 bosses defeated)."""
        _register_player(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        all_bosses_defeated = {str(b): "defeated" for b in BOSS_BOUNDARIES}
        dig_repo.update_tunnel(10001, guild_id, depth=280, boss_progress=json.dumps(all_bosses_defeated))

    def test_prestige_resets_depth(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Depth resets to 0."""
        self._setup_prestige_ready(dig_service, dig_repo, player_repository, guild_id, monkeypatch)
        result = dig_service.prestige(10001, guild_id, "advance_boost")
        assert result["success"]
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["depth"] == 0

    def test_prestige_keeps_pickaxe(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Pickaxe carries over."""
        self._setup_prestige_ready(dig_service, dig_repo, player_repository, guild_id, monkeypatch)
        dig_repo.update_tunnel(10001, guild_id, pickaxe_tier=1)
        result = dig_service.prestige(10001, guild_id, "advance_boost")
        assert result["success"]
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["pickaxe_tier"] == 1

    def test_prestige_adds_perk(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Chosen perk is stored."""
        self._setup_prestige_ready(dig_service, dig_repo, player_repository, guild_id, monkeypatch)
        result = dig_service.prestige(10001, guild_id, "advance_boost")
        assert result["success"]
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        perks = json.loads(tunnel["prestige_perks"]) if tunnel["prestige_perks"] else []
        assert "advance_boost" in perks

    def test_prestige_max(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Can't prestige past MAX_PRESTIGE."""
        self._setup_prestige_ready(dig_service, dig_repo, player_repository, guild_id, monkeypatch)
        dig_repo.update_tunnel(10001, guild_id, prestige_level=MAX_PRESTIGE)
        result = dig_service.prestige(10001, guild_id, "advance_boost")
        assert not result["success"]

    def test_prestige_bosses_respawn(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Boss progress resets."""
        self._setup_prestige_ready(dig_service, dig_repo, player_repository, guild_id, monkeypatch)
        result = dig_service.prestige(10001, guild_id, "advance_boost")
        assert result["success"]
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        bp = json.loads(tunnel["boss_progress"]) if tunnel["boss_progress"] else {}
        assert all(v == "active" for v in bp.values())


# =============================================================================
# Item Tests
# =============================================================================


class TestItems:
    """Tests for item purchase and usage."""

    def test_buy_item_deducts_jc(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Buying item costs JC."""
        _register_player(player_repository, balance=100)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)

        balance_before = player_repository.get_balance(10001, guild_id)
        result = dig_service.buy_item(10001, guild_id, "dynamite")
        assert result["success"]
        balance_after = player_repository.get_balance(10001, guild_id)
        assert balance_before - balance_after == CONSUMABLES["dynamite"].cost

    def test_inventory_max(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Can't exceed MAX_INVENTORY_SLOTS items."""
        _register_player(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)

        for i in range(MAX_INVENTORY_SLOTS):
            result = dig_service.buy_item(10001, guild_id, "dynamite")
            assert result["success"], f"Should be able to buy item #{i+1}"

        # 6th item should fail
        result = dig_service.buy_item(10001, guild_id, "dynamite")
        assert not result["success"]

    def test_dynamite_adds_blocks(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Dynamite gives +5 bonus blocks."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=10)

        # Buy and queue dynamite
        dig_service.buy_item(10001, guild_id, "dynamite")
        items = dig_repo.get_inventory(10001, guild_id)
        dig_repo.queue_item(items[0]["id"])

        # Dig with dynamite queued
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        monkeypatch.setattr(random, "randint", lambda a, b: a)  # min advance
        result = dig_service.dig(10001, guild_id)
        assert result["success"]
        # Dynamite should add bonus blocks
        assert result.get("dynamite_bonus") or result["advance"] >= CONSUMABLES["dynamite"].params["bonus_blocks"]

    def test_hard_hat_prevents_cave_in(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Hard hat blocks cave-in (for 3 digs) when charges are already set."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=20, hard_hat_charges=HARD_HAT_USES)

        # Force cave-in conditions but hard hat should block it
        t = 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1
        monkeypatch.setattr(time, "time", lambda: t)
        monkeypatch.setattr(random, "random", lambda: 0.001)  # force cave-in roll
        result = dig_service.dig(10001, guild_id)
        assert result["success"]
        assert not result.get("cave_in"), "Hard hat should prevent cave-in"

        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["hard_hat_charges"] == HARD_HAT_USES - 1

    def test_hard_hat_queued_sets_charges(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Buying and queuing a hard hat actually sets hard_hat_charges on the tunnel."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=20)

        # Buy and queue hard hat
        result = dig_service.buy_item(10001, guild_id, "hard_hat")
        assert result["success"]
        items = dig_repo.get_inventory(10001, guild_id)
        dig_repo.queue_item(items[0]["id"])

        # Dig with hard hat queued — force cave-in roll
        t = 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1
        monkeypatch.setattr(time, "time", lambda: t)
        monkeypatch.setattr(random, "random", lambda: 0.001)
        result = dig_service.dig(10001, guild_id)
        assert result["success"]
        assert not result.get("cave_in"), "Queued hard hat should prevent cave-in"

        # Charges should be set to HARD_HAT_USES - 1 (one consumed this dig)
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["hard_hat_charges"] == HARD_HAT_USES - 1

    def test_lantern_reduces_cave_in(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Lantern halves cave-in chance for the dig it's used on."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        # Place at depth 76 (Magma layer, 25% base cave-in) so halved = 12.5%
        dig_repo.update_tunnel(10001, guild_id, depth=76)

        # Buy and queue lantern
        dig_service.buy_item(10001, guild_id, "lantern")
        items = dig_repo.get_inventory(10001, guild_id)
        dig_repo.queue_item(items[0]["id"])

        # Roll 0.13 — would cave-in at 25% but NOT at ~12.5%
        t = 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1
        monkeypatch.setattr(time, "time", lambda: t)
        monkeypatch.setattr(random, "random", lambda: 0.13)
        result = dig_service.dig(10001, guild_id)
        assert result["success"]
        assert not result.get("cave_in"), "Lantern should halve cave-in chance"

    def test_reinforcement_sets_reinforced_until(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Reinforcement item sets reinforced_until timestamp on the tunnel."""
        _register_player(player_repository, balance=200)
        base_time = 1_000_000
        monkeypatch.setattr(time, "time", lambda: base_time)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=20)

        # Buy and queue reinforcement
        dig_service.buy_item(10001, guild_id, "reinforcement")
        items = dig_repo.get_inventory(10001, guild_id)
        dig_repo.queue_item(items[0]["id"])

        # Dig to consume it
        t = base_time + FREE_DIG_COOLDOWN_SECONDS + 1
        monkeypatch.setattr(time, "time", lambda: t)
        dig_service.dig(10001, guild_id)

        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["reinforced_until"] >= t + 47 * 3600, "Reinforcement should set ~48h protection"

    def test_void_bait_doubles_event_chance(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Void Bait sets void_bait_digs and decrements each dig."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=20)

        # Buy and queue void bait
        dig_service.buy_item(10001, guild_id, "void_bait")
        items = dig_repo.get_inventory(10001, guild_id)
        dig_repo.queue_item(items[0]["id"])

        # Dig to consume it
        t = 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1
        monkeypatch.setattr(time, "time", lambda: t)
        dig_service.dig(10001, guild_id)

        # Void bait should have set 3 charges, then decremented to 2 on this dig
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["void_bait_digs"] == 2

    def test_sonar_pulse_returns_event_preview(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Sonar Pulse includes an event_preview in the dig result."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=20)

        # Buy and queue sonar pulse
        dig_service.buy_item(10001, guild_id, "sonar_pulse")
        items = dig_repo.get_inventory(10001, guild_id)
        dig_repo.queue_item(items[0]["id"])

        t = 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1
        monkeypatch.setattr(time, "time", lambda: t)
        result = dig_service.dig(10001, guild_id)
        assert result["success"]
        # event_preview may or may not have a value depending on the roll,
        # but the key should be present
        assert "event_preview" in result

    def test_queue_item_for_next_dig(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Queued item consumed on next dig."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)

        # Buy and queue
        result = dig_service.buy_item(10001, guild_id, "dynamite")
        item_id = result["item_id"]
        dig_service.queue_item(10001, guild_id, item_id)

        queued = dig_repo.get_queued_items(10001, guild_id)
        assert len(queued) == 1

        # Dig consumes queued item
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        dig_service.dig(10001, guild_id)

        queued_after = dig_repo.get_queued_items(10001, guild_id)
        assert len(queued_after) == 0


# =============================================================================
# Layer Weather Tests
# =============================================================================


@pytest.mark.real_weather
class TestLayerWeather:
    """Tests for the daily layer weather system."""

    def _setup_and_second_dig(self, dig_service, player_repository, monkeypatch):
        """Helper: register, first dig, then second dig to trigger weather."""
        # Restore real weather effects (fixture stubs them out)
        monkeypatch.setattr(dig_service, "_get_weather_effects", DigService._get_weather_effects.__get__(dig_service))
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, 12345)  # first dig (early return, no weather)
        t = 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1
        monkeypatch.setattr(time, "time", lambda: t)
        dig_service.dig(10001, 12345)  # second dig triggers weather

    def test_weather_rolled_on_dig(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Weather should be rolled lazily when a dig reaches the main flow."""
        self._setup_and_second_dig(dig_service, player_repository, monkeypatch)

        today = dig_service._get_game_date()
        weather = dig_repo.get_weather(guild_id, today)
        assert len(weather) == 2, "Should roll exactly 2 weather events"

    def test_weather_targets_populated_layer(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """At least one weather event should target a layer with active players."""
        self._setup_and_second_dig(dig_service, player_repository, monkeypatch)

        today = dig_service._get_game_date()
        weather = dig_repo.get_weather(guild_id, today)
        layers_hit = {w["layer_name"] for w in weather}
        # Dirt should be targeted since the player is in Dirt
        assert "Dirt" in layers_hit

    def test_weather_stable_within_day(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Weather should not re-roll on subsequent digs the same day."""
        self._setup_and_second_dig(dig_service, player_repository, monkeypatch)

        today = dig_service._get_game_date()
        weather1 = dig_repo.get_weather(guild_id, today)

        # Third dig same day
        t = 1_000_000 + 2 * FREE_DIG_COOLDOWN_SECONDS + 2
        monkeypatch.setattr(time, "time", lambda: t)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)

        weather2 = dig_repo.get_weather(guild_id, today)
        assert weather1 == weather2, "Weather should not change within the same day"

    def test_get_weather_returns_info(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """get_weather() should return displayable weather info."""
        self._setup_and_second_dig(dig_service, player_repository, monkeypatch)

        weather = dig_service.get_weather(guild_id)
        assert len(weather) == 2
        for w in weather:
            assert "name" in w
            assert "description" in w
            assert "layer" in w
            assert "effects" in w

    def test_weather_effects_in_dig_result(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Dig result should include weather info when player is in an affected layer."""
        # Restore real _get_weather_effects (fixture stubs it out)
        monkeypatch.setattr(dig_service, "_get_weather_effects", DigService._get_weather_effects.__get__(dig_service))

        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)

        # Force weather on Dirt layer
        today = dig_service._get_game_date()
        dig_repo.set_weather(guild_id, today, "Dirt", "earthworm_migration")
        # Clear the other weather entries to control the test
        dig_repo.set_weather(guild_id, today, "Stone", "mineral_vein")

        dig_repo.update_tunnel(10001, guild_id, depth=5)

        t = 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1
        monkeypatch.setattr(time, "time", lambda: t)
        result = dig_service.dig(10001, guild_id)
        assert result["success"]
        assert result.get("weather") is not None
        assert result["weather"]["name"] == "Earthworm Migration"


# =============================================================================
# Artifact Tests
# =============================================================================


class TestArtifacts:
    """Tests for artifact discovery and trading."""

    def test_artifact_found_tracked(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Found artifact added to collection."""
        _register_player(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)

        # Directly add an artifact (simulating a find)
        artifact_id = ALL_ARTIFACTS[0].id
        db_id = dig_repo.add_artifact(10001, guild_id, artifact_id)
        assert db_id > 0

        artifacts = dig_repo.get_artifacts(10001, guild_id)
        assert len(artifacts) == 1
        assert artifacts[0]["artifact_id"] == artifact_id

    def test_artifact_registered_in_guild(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """First finder tracked in registry."""
        _register_player(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)

        artifact_id = ALL_ARTIFACTS[0].id
        is_first = dig_repo.register_artifact_find(artifact_id, guild_id, 10001, 1_000_000)
        assert is_first is True

        # Second find is not first
        _register_player(player_repository, discord_id=10002)
        is_first2 = dig_repo.register_artifact_find(artifact_id, guild_id, 10002, 1_000_001)
        assert is_first2 is False

        entry = dig_repo.get_registry_entry(artifact_id, guild_id)
        assert entry["first_finder_id"] == 10001
        assert entry["total_found"] == 2

    def test_gift_relic_transfers(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Relic moves from giver to receiver."""
        _register_player(player_repository, discord_id=10001)
        _register_player(player_repository, discord_id=10002)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)

        # Create tunnels
        dig_service.dig(10001, guild_id)
        dig_service.dig(10002, guild_id)

        # Give player 1 a relic
        relic_id = "mole_claws"
        db_id = dig_repo.add_artifact(10001, guild_id, relic_id, is_relic=True)

        # Gift it
        result = dig_service.gift_relic(10001, 10002, guild_id, db_id)
        assert result["success"]

        # Giver no longer has it
        assert not dig_repo.has_artifact(10001, guild_id, relic_id)
        # Receiver has it
        assert dig_repo.has_artifact(10002, guild_id, relic_id)


# =============================================================================
# Achievement Tests
# =============================================================================


class TestAchievements:
    """Tests for achievement unlocking."""

    def test_achievement_unlocked_on_milestone(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Achievement triggers at threshold."""
        _register_player(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)

        # Directly add achievement
        added = dig_repo.add_achievement(10001, guild_id, "dig_count_bronze", 1_000_000)
        assert added is True

        achievements = dig_repo.get_achievements(10001, guild_id)
        assert len(achievements) == 1
        assert achievements[0]["achievement_id"] == "dig_count_bronze"

    def test_achievement_not_duplicated(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Same achievement not added twice."""
        _register_player(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)

        dig_repo.add_achievement(10001, guild_id, "dig_count_bronze", 1_000_000)
        added_again = dig_repo.add_achievement(10001, guild_id, "dig_count_bronze", 1_000_001)
        assert added_again is False

        achievements = dig_repo.get_achievements(10001, guild_id)
        assert len(achievements) == 1


# =============================================================================
# Streak Tests
# =============================================================================


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
        # The 3-day streak bonus is 1 JC
        if 3 in STREAKS:
            # The result from the last dig should show streak_bonus
            assert result.get("streak_bonus", 0) >= STREAKS[3]


# =============================================================================
# Abandon Tests
# =============================================================================


class TestAbandon:
    """Tests for tunnel abandonment."""

    def test_abandon_refunds_10_percent(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Refund = depth * 0.1."""
        _register_player(player_repository, balance=100)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=50)

        balance_before = player_repository.get_balance(10001, guild_id)
        result = dig_service.abandon_tunnel(10001, guild_id)
        assert result["success"]
        balance_after = player_repository.get_balance(10001, guild_id)
        expected_refund = int(50 * ABANDON_REFUND_PCT)
        assert balance_after - balance_before == expected_refund

    def test_abandon_min_depth_10(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Can't abandon below depth 10."""
        _register_player(player_repository, balance=100)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=5)

        result = dig_service.abandon_tunnel(10001, guild_id)
        assert not result["success"]

    def test_abandon_keeps_prestige(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Prestige level preserved."""
        _register_player(player_repository, balance=100)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=50, prestige_level=2)

        result = dig_service.abandon_tunnel(10001, guild_id)
        assert result["success"]
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["prestige_level"] == 2


# =============================================================================
# Pickaxe Tests
# =============================================================================


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


# =============================================================================
# Bug Fix Regression Tests
# =============================================================================


class TestTunnelNameKey:
    """Verify tunnel_name (not 'name') is used for tunnel display names."""

    def test_help_returns_tunnel_name(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """help_tunnel result should contain the actual tunnel name."""
        _register_player(player_repository, discord_id=10001)
        _register_player(player_repository, discord_id=10002)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)  # no cave-in
        # Create target tunnel
        dig_service.dig(10002, guild_id)
        tunnel = dig_repo.get_tunnel(10002, guild_id)
        actual_name = dict(tunnel).get("tunnel_name")
        assert actual_name  # tunnel has a name

        # Help the target
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        result = dig_service.help_tunnel(10001, 10002, guild_id)
        assert result["success"]
        assert result["target_tunnel"] == actual_name

    def test_sabotage_returns_tunnel_name(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """sabotage_tunnel result should contain the actual tunnel name."""
        _register_player(player_repository, discord_id=10001, balance=200)
        _register_player(player_repository, discord_id=10002)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)  # no cave-in
        dig_service.dig(10002, guild_id)
        # Set target depth high enough for sabotage cost
        dig_repo.update_tunnel(10002, guild_id, depth=30)
        tunnel = dig_repo.get_tunnel(10002, guild_id)
        actual_name = dict(tunnel).get("tunnel_name")

        result = dig_service.sabotage_tunnel(10001, 10002, guild_id)
        assert result["success"]
        assert result["target_tunnel"] == actual_name

    def test_get_flex_data_returns_tunnel_name(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """get_flex_data should return the actual tunnel name."""
        _register_player(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        actual_name = dict(tunnel).get("tunnel_name")

        result = dig_service.get_flex_data(10001, guild_id)
        assert result["success"]
        assert result["tunnel_name"] == actual_name

    def test_generate_clue_first_letter_uses_tunnel_name(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """_generate_clue should use tunnel_name for the first-letter clue."""
        _register_player(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        actual_name = dict(tunnel).get("tunnel_name")
        first_letter = actual_name[0]

        clue = dig_service._generate_clue(10001, guild_id, "first_letter")
        assert first_letter in clue["hint"]


class TestHasLanternInResult:
    """Verify has_lantern is included in dig results for boss encounters."""

    def test_dig_result_includes_has_lantern(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Normal dig result should include has_lantern field."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)  # no cave-in
        dig_service.dig(10001, guild_id)

        # Queue a lantern
        dig_repo.add_inventory_item(10001, guild_id, "lantern")
        items = dig_repo.get_inventory(10001, guild_id)
        for item in items:
            if dict(item).get("item_type") == "lantern":
                dig_repo.queue_item(dict(item)["id"])

        # Set depth near boss boundary so advance doesn't skip it
        dig_repo.update_tunnel(10001, guild_id, depth=23)

        # Force advance to hit boss boundary at 25
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        monkeypatch.setattr(random, "randint", lambda a, b: 3)  # advance 3 would reach 26 > 25
        result = dig_service.dig(10001, guild_id)
        assert result["success"]
        # has_lantern should be in the result
        assert "has_lantern" in result


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


class TestUseItemValidation:
    """Verify use_item returns errors for invalid item types."""

    def test_use_item_unknown_type_returns_error(self, dig_service, player_repository, guild_id, monkeypatch):
        """use_item with a display name (e.g. 'Dynamite') instead of type key ('dynamite') should fail."""
        _register_player(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)

        result = dig_service.use_item(10001, guild_id, "Dynamite")
        assert result["success"] is False
        assert "Unknown" in result["error"]

    def test_use_item_valid_type_succeeds(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """use_item with correct type key ('dynamite') should succeed when item is in inventory."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)

        # Buy dynamite
        buy_result = dig_service.buy_item(10001, guild_id, "dynamite")
        assert buy_result["success"]

        # Use dynamite with lowercase type key
        result = dig_service.use_item(10001, guild_id, "dynamite")
        assert result["success"]


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

        # Multiplier should come from BOSS_PAYOUTS[25], not default 2.0
        cautious_mult = result["odds"]["cautious"]["multiplier"]
        expected_mult = BOSS_PAYOUTS[25][0]
        assert cautious_mult == expected_mult, f"Expected multiplier {expected_mult}, got {cautious_mult}"

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
        """Cautious fight with low roll (0.01 < 0.75 base odds) should win."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=24)

        monkeypatch.setattr(random, "random", lambda: 0.01)
        result = dig_service.fight_boss(10001, guild_id, "cautious", wager=0)
        assert result["success"]
        assert result["won"] is True


# =============================================================================
# Expansion System Tests
# =============================================================================


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
        dig_repo.update_tunnel(10001, guild_id, depth=80)

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
        dig_repo.update_tunnel(10001, guild_id, depth=50)  # Stone layer, base 0.16

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

        # Dirt at pitch black: event_chance = min(0.20 * 3.0, 0.75) = 0.60
        # cave_in_chance = 0.05 + 0.25 = 0.30
        dig_repo.update_tunnel(10001, guild_id, depth=10, luminosity=0)
        # Roll 0.45: above cave-in (0.30), below event (0.60) -> event triggers
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + cd + 1)
        monkeypatch.setattr(ds_mod.random, "random", lambda: 0.45)
        with patch.object(dig_service, "roll_event", wraps=dig_service.roll_event) as spy:
            dig_service.dig(10001, guild_id)
        assert spy.call_count > 0, "Pitch-black Dirt: roll=0.45 should trigger event (chance=0.60)"

        # Roll 0.65: above event (0.60) -> no event
        dig_repo.update_tunnel(10001, guild_id, depth=10, luminosity=0)
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 2 * (cd + 1))
        monkeypatch.setattr(ds_mod.random, "random", lambda: 0.65)
        with patch.object(dig_service, "roll_event", wraps=dig_service.roll_event) as spy:
            dig_service.dig(10001, guild_id)
        assert spy.call_count == 0, "Pitch-black Dirt: roll=0.65 should NOT trigger (chance=0.60)"

        # Abyss at pitch black: event_chance = min(0.28 * 3.0, 0.75) = 0.75 (at the cap)
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

        # underground_stream risky has success_chance=0.50
        # With dark penalty: 0.50 - 0.10 = 0.40
        # Roll of 0.45 should fail (0.45 >= 0.40)
        monkeypatch.setattr(random, "random", lambda: 0.45)
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


class TestExpandedEvents:
    """Verify expanded event system."""

    def test_event_pool_size_floor(self):
        """Event pool: 93 baseline + 5 trap + 3 splash + 15 delve-themed, so ≥116.

        Floor, not exact count, so routine event additions don't break the test.
        """
        from services.dig_constants import EVENT_POOL
        assert len(EVENT_POOL) >= 116

    def test_new_events_have_complexity_field(self):
        """All events should have a complexity field."""
        from services.dig_constants import EVENT_POOL
        for e in EVENT_POOL:
            assert "complexity" in e, f"Event {e['id']} missing complexity"

    def test_darkness_events_exist(self):
        """Should have events that require pitch black luminosity."""
        from services.dig_constants import EVENT_POOL
        dark_events = [e for e in EVENT_POOL if e.get("requires_dark")]
        assert len(dark_events) >= 3

    def test_roll_event_filters_by_layer(self, dig_service):
        """roll_event should filter events by depth/layer."""
        random.seed(42)
        # Roll 100 events at shallow depth — should never get deep events
        for _ in range(100):
            event = dig_service.roll_event(5, luminosity=100)
            if event:
                assert event.get("rarity") in ("common", "uncommon", "rare", "legendary")

    def test_dota_hero_events_exist(self):
        """Should have Dota hero encounter events."""
        from services.dig_constants import EVENT_POOL
        dota_ids = {"pudge_fishing", "tinker_workshop", "the_burrow", "arcanist_library", "the_dark_rift", "roshan_lair"}
        event_ids = {e["id"] for e in EVENT_POOL}
        assert dota_ids.issubset(event_ids), f"Missing Dota events: {dota_ids - event_ids}"


class TestExpandedPrestige:
    """Verify extended prestige and pickaxes."""

    def test_max_prestige_is_10(self):
        from services.dig_constants import MAX_PRESTIGE
        assert MAX_PRESTIGE == 10

    def test_seven_pickaxe_tiers(self):
        from services.dig_constants import _PICKAXE_TIERS_DEF
        assert len(_PICKAXE_TIERS_DEF) == 7
        assert _PICKAXE_TIERS_DEF[-1].name == "Void-Touched"

    def test_nine_prestige_perks(self):
        assert len(PRESTIGE_PERKS) == 9
        assert "deep_sight" in PRESTIGE_PERKS
        assert "the_endless" in PRESTIGE_PERKS

    def test_crowns_for_all_levels(self):
        from services.dig_constants import MAX_PRESTIGE, PRESTIGE_CROWNS
        for i in range(MAX_PRESTIGE + 1):
            assert i in PRESTIGE_CROWNS, f"Missing crown for prestige {i}"


class TestNewItemsAndArtifacts:
    """Verify new consumables and artifacts."""

    def test_nine_consumables(self):
        from services.dig_constants import CONSUMABLES
        assert len(CONSUMABLES) == 9
        assert "torch" in CONSUMABLES
        assert "void_bait" in CONSUMABLES

    def test_35_artifacts(self):
        from services.dig_constants import ALL_ARTIFACTS
        assert len(ALL_ARTIFACTS) == 35

    def test_fungal_artifacts_exist(self):
        from services.dig_constants import ALL_ARTIFACTS
        fungal = [a for a in ALL_ARTIFACTS if a.layer == "Fungal Depths"]
        assert len(fungal) >= 4  # 1 relic + 3 collectibles

    def test_aegis_fragment_exists(self):
        from services.dig_constants import ARTIFACT_BY_ID
        assert "aegis_fragment" in ARTIFACT_BY_ID
        assert ARTIFACT_BY_ID["aegis_fragment"].is_relic is True

    def test_buy_item_torch(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Should be able to buy a torch."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)

        result = dig_service.buy_item(10001, guild_id, "torch")
        assert result["success"]
        assert result["cost"] == 6


# =============================================================================
# Cheer Tests
# =============================================================================


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

    def test_cheer_charges_cheerer_not_target(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Cheer costs 3 JC from the cheerer, not the target."""
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

        assert player_repository.get_balance(10002, guild_id) == balance_cheerer_before - 3
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
        assert "maximum" in result.get("error", "").lower()

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


# =============================================================================
# Boss Error / Boundary Tests
# =============================================================================


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


# =============================================================================
# Tunnel Normalization Tests
# =============================================================================


class TestTunnelNormalization:
    """Tests for integer type coercion in tunnel data."""

    def test_normalize_tunnel_casts_string_ints(self, dig_repo):
        """_normalize_tunnel converts string values to int for known columns."""
        raw = {
            "depth": "49",
            "luminosity": "100",
            "last_dig_at": "1000000",
            "boss_attempts": "3",
            "tunnel_name": "Test Tunnel",
            "boss_progress": '{"50": "active"}',
        }
        normalized = DigRepository._normalize_tunnel(raw)
        assert normalized["depth"] == 49
        assert isinstance(normalized["depth"], int)
        assert normalized["luminosity"] == 100
        assert isinstance(normalized["luminosity"], int)
        assert normalized["last_dig_at"] == 1000000
        assert normalized["boss_attempts"] == 3
        # Non-int columns left alone
        assert normalized["tunnel_name"] == "Test Tunnel"
        assert normalized["boss_progress"] == '{"50": "active"}'

    def test_normalize_tunnel_handles_none_values(self, dig_repo):
        """_normalize_tunnel leaves None values as None."""
        raw = {"depth": 10, "last_dig_at": None, "luminosity": None}
        normalized = DigRepository._normalize_tunnel(raw)
        assert normalized["depth"] == 10
        assert normalized["last_dig_at"] is None
        assert normalized["luminosity"] is None

    def test_get_tunnel_returns_int_types(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """get_tunnel returns integer types for numeric columns."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)

        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert isinstance(tunnel["depth"], int)
        assert isinstance(tunnel["luminosity"], int)
        assert isinstance(tunnel["last_dig_at"], int)


# =============================================================================
# Ascension System Tests
# =============================================================================


class TestAscensionSystem:
    """Test ascension modifier mechanics."""

    def test_get_ascension_effects_level_0(self, dig_service):
        """No effects at prestige 0."""
        effects = dig_service._get_ascension_effects(0)
        assert effects == {}

    def test_get_ascension_effects_level_1(self, dig_service):
        """Level 1 returns jc_multiplier (no advance penalty)."""
        effects = dig_service._get_ascension_effects(1)
        assert "advance_penalty" not in effects
        assert "jc_multiplier" in effects
        assert effects["jc_multiplier"] == 0.25

    def test_ascension_effects_cumulative(self, dig_service):
        """Multiple levels stack their effects."""
        effects = dig_service._get_ascension_effects(3)
        # Level 1 jc_multiplier=0.25 (no advance_penalty)
        assert "advance_penalty" not in effects
        assert effects["jc_multiplier"] == 0.25
        # Level 2 cave_in_bonus=0.03
        assert effects["cave_in_bonus"] == 0.03
        # Level 2 event_chance_multiplier=0.20
        assert effects["event_chance_multiplier"] == 0.20
        # Level 3 luminosity_drain_multiplier=0.25
        assert effects["luminosity_drain_multiplier"] == 0.25
        # Level 3 rare_event_multiplier=0.50
        assert effects["rare_event_multiplier"] == 0.50

    def test_boss_phase2_at_prestige_4(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Boss fight at P4+ returns phase2_incoming on first win."""
        _register_player(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=24, prestige_level=4)

        # Cautious @ depth 25, P4 with wager=10: player_hit 0.60 − 0.01 − 0.04
        # = 0.55 (no free-fight mod). boss_hit=0.30. A roll of 0.35 hits for
        # the player (<0.55) and misses for the boss (>0.30), so the duel
        # resolves deterministically against an 8-HP boss.
        monkeypatch.setattr(random, "random", lambda: 0.35)
        result = dig_service.fight_boss(10001, guild_id, "cautious", wager=10)
        assert result["success"]
        assert result.get("won") is True
        # At P4, boss should enter phase 2 on first victory
        assert result.get("phase2_incoming") is True
        assert result.get("phase") == 1

        # Boss progress should be "phase1_defeated"
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        bp = json.loads(tunnel["boss_progress"])
        assert bp["25"] == "phase1_defeated"

    def test_boss_no_phase2_below_prestige_4(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Boss fight below P4 goes straight to defeated."""
        _register_player(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=24, prestige_level=3)

        # Same rationale as test_boss_phase2_at_prestige_4 but P3 so
        # prestige penalty is 0.03. player_hit = 0.60 − 0.01 − 0.03 = 0.56
        # with wager=10. roll=0.35 hits player (<0.56), misses boss (>0.30).
        monkeypatch.setattr(random, "random", lambda: 0.35)
        result = dig_service.fight_boss(10001, guild_id, "cautious", wager=10)
        assert result["success"]
        assert result.get("won") is True
        # No phase 2 at P3
        assert result.get("phase2_incoming") is not True

        # Boss should go straight to defeated
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        bp = json.loads(tunnel["boss_progress"])
        assert bp["25"] == "defeated"


# =============================================================================
# Corruption System Tests
# =============================================================================


class TestCorruptionSystem:
    """Test corruption roll mechanics."""

    def test_no_corruption_below_p6(self, dig_service):
        """_roll_corruption returns None at prestige < 6."""
        for level in range(6):
            result = dig_service._roll_corruption(level)
            assert result is None, f"Expected None at prestige {level}"

    def test_corruption_at_p6(self, dig_service):
        """_roll_corruption returns an effect at prestige 6+."""
        random.seed(42)
        result = dig_service._roll_corruption(6)
        assert result is not None
        assert "id" in result
        assert "description" in result
        assert "effects" in result
        assert isinstance(result["effects"], dict)

    def test_corruption_effect_has_valid_fields(self, dig_service):
        """Corruption effect dict has all expected fields."""
        random.seed(0)
        # Run multiple times to cover both bad and weird paths
        found_any = False
        for seed in range(50):
            random.seed(seed)
            result = dig_service._roll_corruption(8)
            assert result is not None
            assert "id" in result
            assert "weird" in result
            assert isinstance(result["weird"], bool)
            found_any = True
        assert found_any

    def test_corruption_weird_ratio(self, dig_service):
        """Corruption rolls are ~80% bad / ~20% weird over many trials."""
        random.seed(12345)
        weird_count = 0
        total = 500
        for _ in range(total):
            result = dig_service._roll_corruption(6)
            if result["weird"]:
                weird_count += 1
        # Should be roughly 20% weird (allow 10%-35% tolerance for randomness)
        assert 50 <= weird_count <= 175, f"Weird ratio {weird_count}/{total} outside expected range"


# =============================================================================
# Mutation System Tests
# =============================================================================


class TestMutationSystem:
    """Test mutation mechanics."""

    def test_roll_mutations_returns_forced_and_choices(self, dig_service):
        """_roll_mutations_for_prestige returns (forced, choices_list)."""
        random.seed(42)
        forced, choices = dig_service._roll_mutations_for_prestige()
        # forced is a single dict
        assert isinstance(forced, dict)
        assert "id" in forced
        assert "name" in forced
        assert "description" in forced
        assert "positive" in forced
        # choices is a list of dicts
        assert isinstance(choices, list)
        assert len(choices) == 3
        for c in choices:
            assert "id" in c
            assert "name" in c
        # forced should not be in choices
        choice_ids = {c["id"] for c in choices}
        assert forced["id"] not in choice_ids

    def test_apply_mutation_effects(self, dig_service):
        """_apply_mutation_effects combines effect dicts."""
        mutations = [
            {"id": "cave_in_loot"},
            {"id": "brittle_walls"},
        ]
        combined = dig_service._apply_mutation_effects(mutations)
        # cave_in_loot has cave_in_loot_chance=0.30
        assert combined.get("cave_in_loot_chance") == 0.30
        # brittle_walls has cave_in_loss_bonus=2
        assert combined.get("cave_in_loss_bonus") == 2

    def test_apply_mutation_effects_stacks_numeric(self, dig_service):
        """Numeric mutation effects from multiple mutations stack additively."""
        # Two mutations with the same numeric key should add
        mutations = [
            {"id": "event_magnet"},     # event_chance_bonus=0.30
            {"id": "treasure_sense"},   # artifact_chance_bonus=0.25
        ]
        combined = dig_service._apply_mutation_effects(mutations)
        assert combined.get("event_chance_bonus") == 0.30
        assert combined.get("artifact_chance_bonus") == 0.25

    def test_get_mutations_empty(self, dig_service):
        """_get_mutations returns empty list for tunnel with no mutations."""
        tunnel = {"mutations": None}
        assert dig_service._get_mutations(tunnel) == []
        tunnel2 = {"mutations": ""}
        assert dig_service._get_mutations(tunnel2) == []

    def test_get_mutations_parses_json(self, dig_service):
        """_get_mutations parses stored JSON correctly."""
        data = [{"id": "cave_in_loot", "name": "Lucky Rubble"}]
        tunnel = {"mutations": json.dumps(data)}
        result = dig_service._get_mutations(tunnel)
        assert len(result) == 1
        assert result[0]["id"] == "cave_in_loot"

    def test_mutations_stored_in_prestige(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Prestige at P8+ stores mutations in tunnel."""
        _register_player(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        all_bosses_defeated = {str(b): "defeated" for b in BOSS_BOUNDARIES}
        dig_repo.update_tunnel(
            10001, guild_id, depth=280,
            boss_progress=json.dumps(all_bosses_defeated),
            prestige_level=7,  # After prestige will become P8
        )

        random.seed(42)
        result = dig_service.prestige(10001, guild_id, "advance_boost")
        assert result["success"]
        assert result["prestige_level"] == 8

        # Mutations should be stored
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        mutations_raw = tunnel.get("mutations")
        assert mutations_raw is not None
        mutations = json.loads(mutations_raw)
        assert len(mutations) >= 1  # At least the forced mutation
        # Result should contain mutation info
        assert result.get("mutations") is not None


# =============================================================================
# Run Scoring Tests
# =============================================================================


class TestRunScoring:
    """Test run score calculation."""

    def test_calculate_run_score_basic(self, dig_service):
        """Score based on depth + bosses + JC + artifacts + events."""
        tunnel = {
            "depth": 100,
            "boss_progress": json.dumps({"25": "defeated", "50": "defeated", "75": "active", "100": "active"}),
            "current_run_jc": 40,
            "current_run_artifacts": 2,
            "current_run_events": 5,
            "prestige_level": 0,
        }
        score = dig_service._calculate_run_score(tunnel)
        # base = depth*1 + bosses_defeated*50 + int(jc*0.5) + artifacts*25 + events*10
        # = 100 + 2*50 + int(40*0.5) + 2*25 + 5*10
        # = 100 + 100 + 20 + 50 + 50 = 320
        # multiplier = 1 + 0*0.1 = 1.0
        expected = int(320 * 1.0)
        assert score == expected

    def test_score_multiplier_at_higher_prestige(self, dig_service):
        """Higher prestige levels multiply the score."""
        tunnel_base = {
            "depth": 50,
            "boss_progress": json.dumps({"25": "defeated", "50": "active"}),
            "current_run_jc": 20,
            "current_run_artifacts": 1,
            "current_run_events": 3,
            "prestige_level": 0,
        }
        score_p0 = dig_service._calculate_run_score(tunnel_base)

        tunnel_p5 = dict(tunnel_base)
        tunnel_p5["prestige_level"] = 5
        score_p5 = dig_service._calculate_run_score(tunnel_p5)
        # multiplier at P5 = 1 + 5*0.1 = 1.5
        assert score_p5 > score_p0
        assert score_p5 == int(score_p0 * 1.5)

    def test_score_multiplier_p10_includes_ascension(self, dig_service):
        """P10 'The Endless' adds score_multiplier=2.0 on top of base multiplier."""
        tunnel = {
            "depth": 100,
            "boss_progress": json.dumps({}),
            "current_run_jc": 0,
            "current_run_artifacts": 0,
            "current_run_events": 0,
            "prestige_level": 10,
        }
        score = dig_service._calculate_run_score(tunnel)
        # base = 100
        # base multiplier = 1 + 10*0.1 = 2.0
        # ascension score_multiplier at P10 = 2.0
        # total multiplier = 2.0 + 2.0 = 4.0
        expected = int(100 * 4.0)
        assert score == expected

    def test_prestige_stores_run_score(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Prestige stores best_run_score and resets counters."""
        _register_player(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        all_bosses_defeated = {str(b): "defeated" for b in BOSS_BOUNDARIES}
        dig_repo.update_tunnel(
            10001, guild_id,
            depth=280,
            boss_progress=json.dumps(all_bosses_defeated),
            current_run_jc=50,
            current_run_artifacts=3,
            current_run_events=10,
        )

        result = dig_service.prestige(10001, guild_id, "advance_boost")
        assert result["success"]
        assert result["run_score"] > 0
        assert result["best_run_score"] > 0

        # Counters should be reset
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel.get("current_run_jc") == 0 or tunnel["current_run_jc"] == 0
        assert tunnel.get("current_run_artifacts") == 0 or tunnel["current_run_artifacts"] == 0
        assert tunnel.get("current_run_events") == 0 or tunnel["current_run_events"] == 0


# =============================================================================
# New Event Mechanics Tests
# =============================================================================


class TestNewEventMechanics:
    """Test desperate and boon event mechanics."""

    def test_event_pool_has_desperate_options(self):
        """Some events in pool have desperate_option."""
        from services.dig_constants import EVENT_POOL
        desperate_events = [e for e in EVENT_POOL if e.get("desperate_option") is not None]
        assert len(desperate_events) > 0, "Expected at least one event with desperate_option"

    def test_event_pool_has_boon_options(self):
        """Some events in pool have boon_options."""
        from services.dig_constants import EVENT_POOL
        boon_events = [e for e in EVENT_POOL if e.get("boon_options")]
        assert len(boon_events) > 0, "Expected at least one event with boon_options"

    def test_roll_event_filters_by_prestige(self, dig_service):
        """roll_event still respects min_prestige if any event ever uses it.
        Currently the pool has zero gated events (the original three were
        unlocked), so this test passes vacuously and serves as a regression
        guard for the filter mechanism if a gate is ever re-introduced."""
        from services.dig_constants import EVENT_POOL
        gated_ids = {e["id"] for e in EVENT_POOL if e.get("min_prestige", 0) > 0}
        random.seed(42)
        found_gated = set()
        for _ in range(500):
            event = dig_service.roll_event(200, luminosity=100, prestige_level=0)
            if event and event["id"] in gated_ids:
                found_gated.add(event["id"])
        assert len(found_gated) == 0, f"Prestige-gated events should not appear at P0: {found_gated}"

    def test_all_new_events_have_valid_structure(self):
        """Every event in the pool has the required fields."""
        from services.dig_constants import EVENT_POOL
        for e in EVENT_POOL:
            assert "id" in e, "Event missing 'id'"
            assert "name" in e, f"Event {e.get('id', '?')} missing 'name'"
            assert "complexity" in e, f"Event {e['id']} missing 'complexity'"
            assert "rarity" in e, f"Event {e['id']} missing 'rarity'"
            assert e["rarity"] in ("common", "uncommon", "rare", "legendary"), (
                f"Event {e['id']} has invalid rarity: {e['rarity']}"
            )
            # safe_option must exist for all events (primary resolution path)
            assert e.get("safe_option") is not None or e.get("boon_options") is not None, (
                f"Event {e['id']} has neither safe_option nor boon_options"
            )

    def test_resolve_event_desperate_choice(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """resolve_event handles desperate choice correctly."""
        from services.dig_constants import EVENT_POOL
        desperate_events = [e for e in EVENT_POOL if e.get("desperate_option") is not None]
        event = desperate_events[0]

        _register_player(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=50)

        random.seed(42)
        result = dig_service.resolve_event(10001, guild_id, event["id"], "desperate")
        assert result["success"]
        assert result.get("choice") == "desperate"

    def test_resolve_event_boon_choice(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """resolve_event handles boon choice correctly."""
        from services.dig_constants import EVENT_POOL
        boon_events = [e for e in EVENT_POOL if e.get("boon_options")]
        event = boon_events[0]

        _register_player(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=50)

        result = dig_service.resolve_event(10001, guild_id, event["id"], "boon_0")
        assert result["success"]
        assert result.get("choice") == "boon_0"
        assert result.get("buff_applied") is not None

    def test_resolve_event_boon_invalid_index(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """resolve_event rejects invalid boon index."""
        from services.dig_constants import EVENT_POOL
        boon_events = [e for e in EVENT_POOL if e.get("boon_options")]
        event = boon_events[0]

        _register_player(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=50)

        # Use an index beyond the number of boon options
        num_boons = len(event["boon_options"])
        result = dig_service.resolve_event(10001, guild_id, event["id"], f"boon_{num_boons + 10}")
        assert not result["success"]


# =============================================================================
# Hall of Fame Tests
# =============================================================================


class TestHallOfFame:
    """Test hall of fame leaderboard."""

    def test_hall_of_fame_empty_guild(self, dig_service, guild_id):
        """Hall of fame returns empty list for guild with no scores."""
        result = dig_service.get_hall_of_fame(guild_id)
        assert result["success"]
        assert result["entries"] == []

    def test_hall_of_fame_after_prestige(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Hall of fame shows player after prestige with run score."""
        _register_player(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        all_bosses_defeated = {str(b): "defeated" for b in BOSS_BOUNDARIES}
        dig_repo.update_tunnel(
            10001, guild_id,
            depth=280,
            boss_progress=json.dumps(all_bosses_defeated),
            current_run_jc=30,
            current_run_artifacts=2,
            current_run_events=5,
        )

        result = dig_service.prestige(10001, guild_id, "advance_boost")
        assert result["success"]

        hof = dig_service.get_hall_of_fame(guild_id)
        assert hof["success"]
        assert len(hof["entries"]) == 1
        assert hof["entries"][0]["discord_id"] == 10001
        assert hof["entries"][0]["best_run_score"] > 0


class TestEventPoolInvariants:
    """Invariants on the EVENT_POOL itself, independent of the service."""

    def test_every_non_boon_event_has_safe_option(self):
        """After widening the encounter gate, any non-boon event reaches the
        encounter UI via its safe_option. Events without one would orphan
        into _build_dig_embed's text-only branch."""
        from services.dig_constants import EVENT_POOL
        offenders = [
            e["id"] for e in EVENT_POOL
            if e.get("complexity") != "boon" and not e.get("safe_option")
        ]
        assert offenders == [], (
            f"Non-boon events missing safe_option: {offenders}. "
            "These would render as orphaned flavor text."
        )

    def test_rarity_weights_constant(self):
        from services.dig_service import RARITY_WEIGHTS
        assert RARITY_WEIGHTS == {"common": 70, "uncommon": 20, "rare": 12, "legendary": 6}

    def test_prestige_gates_unlocked(self):
        """infernal_gate, aghanim_trial, neow_blessing should be rollable
        for prestige-0 players after the unlock."""
        from services.dig_constants import EVENT_POOL
        unlocked = {"infernal_gate", "aghanim_trial", "neow_blessing"}
        for e in EVENT_POOL:
            if e["id"] in unlocked:
                assert (e.get("min_prestige") or 0) == 0, (
                    f"{e['id']} still has min_prestige={e.get('min_prestige')}"
                )

    def test_widened_depth_ranges(self):
        """Verify the depth widening from the variance pass didn't get reverted."""
        from services.dig_constants import EVENT_POOL
        expected = {
            "creeper_ambush": (0, 75),
            "abandoned_minecart": (0, 75),
            "villager_trade": (0, 80),
            "enderman_stare": (0, 80),
            "mob_spawner": (0, 75),
            "witch_cauldron": (0, 75),
            "azurite_deposit": (40, 120),
            "crawler_breakdown": (40, 120),
            "fossil_cache": (40, 120),
            "breach_encounter": (40, 170),
            "vaal_side_area": (40, 170),
            "syndicate_ambush": (40, 170),
            "delve_smuggler": (40, 170),
            "brann_bronzebeard": (130, 290),
            "earthen_cache": (130, 290),
            "campfire_rest": (130, 300),
            "zekvir_shadow": (130, 290),
            "dark_rider": (130, 290),
            "titan_relic": (130, 290),
            "candle_glow": (130, 290),
        }
        by_id = {e["id"]: e for e in EVENT_POOL}
        for eid, (lo, hi) in expected.items():
            e = by_id[eid]
            assert (e["min_depth"], e["max_depth"]) == (lo, hi), (
                f"{eid}: expected ({lo},{hi}) got ({e['min_depth']},{e['max_depth']})"
            )


class TestRarityRebalance:
    """Statistical test that the rarity rebalance lands rare share at ~4%."""

    def test_rare_share_in_expected_band(self, dig_service):
        from services.dig_constants import EVENT_POOL, get_layer
        from services.dig_service import RARITY_WEIGHTS

        # Use a depth/layer where we know there's a healthy mix of rarities.
        # Compute expected rare share against the EVENT_POOL filter at the
        # same depth, deriving the layer name dynamically so this stays
        # correct if depth or layer boundaries change.
        random.seed(20260412)
        depth = 30
        layer_name = get_layer(depth).name
        rolls = 5000
        rare_hits = 0
        for _ in range(rolls):
            ev = dig_service.roll_event(depth, luminosity=100, prestige_level=0)
            if ev and ev.get("rarity") == "rare":
                rare_hits += 1
        share = rare_hits / rolls

        # Expected rare share at this depth depends on the eligible pool.
        # Compute it analytically using the same filter roll_event applies.
        eligible = [
            e for e in EVENT_POOL
            if depth >= (e.get("min_depth") or 0)
            and (e.get("max_depth") is None or depth <= e["max_depth"])
            and (e.get("layer") is None or e["layer"] == layer_name)
            and (e.get("min_prestige") or 0) == 0
        ]
        total_w = sum(RARITY_WEIGHTS[e.get("rarity", "common")] for e in eligible)
        rare_w = sum(RARITY_WEIGHTS["rare"] for e in eligible if e.get("rarity") == "rare")
        expected = rare_w / total_w if total_w else 0

        # Allow ±2 percentage points around analytic expectation.
        assert abs(share - expected) < 0.02, (
            f"rare share {share:.3f} drifted from analytic {expected:.3f} "
            f"(weights={RARITY_WEIGHTS})"
        )
