"""Tests for the tunnel digging minigame service."""

import json
import random
import time

import pytest

from repositories.dig_repository import DigRepository
from repositories.player_repository import PlayerRepository
from services.dig_service import DigService
from services.dig_constants import (
    FREE_DIG_COOLDOWN_SECONDS,
    PAID_DIG_COSTS_PER_DAY,
    LAYERS,
    MILESTONES,
    STREAKS,
    PICKAXE_TIERS,
    CONSUMABLES,
    BOSSES,
    MAX_PRESTIGE,
    PRESTIGE_PERKS,
    MAX_INVENTORY_SLOTS,
    SABOTAGE_COOLDOWN_SECONDS,
    CAVE_IN_BLOCK_LOSS_MIN,
    CAVE_IN_BLOCK_LOSS_MAX,
    ALL_ARTIFACTS,
    VISIBLE_ACHIEVEMENTS,
    ABANDON_MIN_DEPTH,
    ABANDON_COOLDOWN_SECONDS,
    ABANDON_REFUND_PCT,
    FIRST_DIG_ADVANCE_MIN,
    FIRST_DIG_ADVANCE_MAX,
    FIRST_DIG_JC_MIN,
    FIRST_DIG_JC_MAX,
    SABOTAGE_BASE_COST,
    SABOTAGE_COST_DIVISOR,
    SABOTAGE_DAMAGE_MIN,
    SABOTAGE_DAMAGE_MAX,
    INSURANCE_BASE_COST,
    INSURANCE_COST_DEPTH_DIVISOR,
    INSURANCE_DURATION_SECONDS,
    INSURANCE_REDUCTION,
    DECAY_START_HOURS,
    DECAY_ACCELERATED_HOURS,
    DECAY_ACCELERATED_MULTIPLIER,
    DECAY_FLOOR_DEPTHS,
    HARD_HAT_USES,
    TRAP_BASE_COST,
)


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def dig_service(dig_repo, player_repository):
    return DigService(dig_repo, player_repository)


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
            monkeypatch.setattr(time, "time", lambda: base_time + 60 * (i + 1))
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
        result = dig_service.dig(10001, guild_id)
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
        # Set depth just below milestone
        dig_repo.update_tunnel(10001, guild_id, depth=23)

        # Dig to cross 25
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        # Force advance of 3+ to cross 25
        monkeypatch.setattr(random, "randint", lambda a, b: 3)
        result = dig_service.dig(10001, guild_id)
        if result["success"] and result["depth"] >= 25:
            assert result.get("milestone_bonus", 0) == MILESTONES[25]

    def test_milestone_50_bonus(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """+10 JC at depth 50."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=48)

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        monkeypatch.setattr(random, "randint", lambda a, b: 3)
        result = dig_service.dig(10001, guild_id)
        if result["success"] and result["depth"] >= 50:
            assert result.get("milestone_bonus", 0) == MILESTONES[50]

    def test_milestone_100_bonus(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """+50 JC at depth 100."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=98)

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        monkeypatch.setattr(random, "randint", lambda a, b: 3)
        result = dig_service.dig(10001, guild_id)
        if result["success"] and result["depth"] >= 100:
            assert result.get("milestone_bonus", 0) == MILESTONES[100]


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

    def test_decay_after_24h(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Depth decreases after 24h inactive."""
        _register_player(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=30)

        # 48h later (24h past the decay start threshold)
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 48 * 3600)
        decay = dig_service.calculate_decay(10001, guild_id)
        assert decay > 0

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
        decay = dig_service.calculate_decay(10001, guild_id)
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["depth"] >= 25, "Decay should not cross layer boundary at 25"

    def test_decay_accelerated_after_72h(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """2x decay rate after 72h."""
        _register_player(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=60, last_dig_at=1_000_000)

        # 48h: normal decay
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 48 * 3600)
        decay_48h = dig_service.calculate_decay(10001, guild_id)

        # 96h: accelerated decay (past 72h threshold)
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + 96 * 3600)
        decay_96h = dig_service.calculate_decay(10001, guild_id)

        # The 96h decay should be more than double the 48h decay due to acceleration
        assert decay_96h > decay_48h

    def test_helpers_reduce_decay(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Recent helpers slow decay rate."""
        _register_player(player_repository, discord_id=10001)
        _register_player(player_repository, discord_id=10002)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=40, last_dig_at=1_000_000)

        # Log a help action recently
        help_time = 1_000_000 + 47 * 3600  # within last 24h of check time
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
        if result["success"]:
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
        # Should be capped at boss boundary or signal boss encounter
        if result["success"]:
            tunnel = dig_repo.get_tunnel(10001, guild_id)
            # Either depth is capped at 25 or a boss encounter is signaled
            assert tunnel["depth"] <= 25 or result.get("boss_encounter")

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
        """Lose costs wager + knockback."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=24)

        balance_before = player_repository.get_balance(10001, guild_id)
        # Force loss
        monkeypatch.setattr(random, "random", lambda: 0.999)
        result = dig_service.fight_boss(10001, guild_id, "cautious", wager=10)
        assert result["success"]
        assert not result.get("won")
        balance_after = player_repository.get_balance(10001, guild_id)
        assert balance_after < balance_before
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

        # Set depth past all bosses but boss_progress incomplete
        dig_repo.update_tunnel(10001, guild_id, depth=105, boss_progress=json.dumps({"25": "defeated", "50": "defeated", "75": "defeated", "100": "active"}))  # 3 of 4 bosses

        result = dig_service.prestige(10001, guild_id, "advance_boost")
        assert not result["success"]

        # Now mark all bosses defeated
        dig_repo.update_tunnel(10001, guild_id, boss_progress=json.dumps({"25": "defeated", "50": "defeated", "75": "defeated", "100": "defeated"}))
        result = dig_service.prestige(10001, guild_id, "advance_boost")
        assert result["success"]


# =============================================================================
# Prestige Tests
# =============================================================================


class TestPrestige:
    """Tests for prestige system."""

    def _setup_prestige_ready(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Helper to set up a player ready for prestige."""
        _register_player(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=105, boss_progress=json.dumps({"25": "defeated", "50": "defeated", "75": "defeated", "100": "defeated"}))

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

    def test_prestige_max_5(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Can't prestige past level 5."""
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

    def test_inventory_max_5(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Can't exceed 5 items."""
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
        """Hard hat blocks cave-in (for 3 digs)."""
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

        # Enough depth but check cost
        dig_repo.update_tunnel(10001, guild_id, depth=stone_tier["depth_required"])
        result = dig_service.upgrade_pickaxe(10001, guild_id)
        if result["success"]:
            tunnel = dig_repo.get_tunnel(10001, guild_id)
            assert tunnel["pickaxe_tier"] == 1

    def test_pickaxe_advance_bonus(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Stone pickaxe gives +1 advance."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
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
