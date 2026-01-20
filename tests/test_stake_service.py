"""
Tests for the player stake pool service (draft mode dual pool betting).

The dual pool system consists of:
1. Player Pool: Auto-liquidity (50 JC Glicko-weighted) + optional player bets
2. Spectator Pool: Separate parimutuel for non-participants (tested separately)
"""

import os
import tempfile
import time

import pytest

from database import Database

# Default jopacoin balance for new players (from schema)
INITIAL_JOPACOIN = 3
from repositories.player_repository import PlayerRepository
from repositories.stake_repository import StakeRepository
from repositories.player_pool_bet_repository import PlayerPoolBetRepository
from services.stake_service import StakePoolConfig, StakeService, PoolState


@pytest.fixture
def services():
    """Create test services with a temporary database."""
    fd, db_path = tempfile.mkstemp(suffix=".db")
    os.close(fd)

    Database(db_path)
    player_repo = PlayerRepository(db_path)
    stake_repo = StakeRepository(db_path)
    player_pool_bet_repo = PlayerPoolBetRepository(db_path)
    stake_config = StakePoolConfig(
        pool_size=50,  # New default: 5 JC per player × 10 players
        stake_per_player=5,
        enabled=True,
        win_prob_min=0.10,
        win_prob_max=0.90,
    )
    stake_service = StakeService(stake_repo, player_repo, player_pool_bet_repo, stake_config)

    yield {
        "stake_service": stake_service,
        "stake_repo": stake_repo,
        "player_repo": player_repo,
        "player_pool_bet_repo": player_pool_bet_repo,
        "db_path": db_path,
    }

    # Cleanup
    try:
        os.unlink(db_path)
    except OSError:
        pass


def _create_players(player_repo, player_ids, balance=INITIAL_JOPACOIN):
    """Helper to create test players."""
    for pid in player_ids:
        player_repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            dotabuff_url=f"https://dotabuff.com/players/{pid}",
            initial_mmr=1500,
            glicko_rating=1500.0 + (pid % 10) * 50,  # Varied ratings
            glicko_rd=100.0,
            glicko_volatility=0.06,
        )
        if balance != INITIAL_JOPACOIN:
            # Set balance to the specified amount
            player_repo.update_balance(pid, balance)


class TestStakeService:
    """Test stake service functionality."""

    def test_calculate_auto_liquidity_equal_teams(self, services):
        """Test auto-liquidity distribution with equal team odds (50/50)."""
        stake_service = services["stake_service"]

        # 50% win probability -> equal split
        radiant_auto, dire_auto = stake_service.calculate_auto_liquidity(0.5)
        assert radiant_auto == 25.0  # 50 × 0.50
        assert dire_auto == 25.0  # 50 × 0.50

    def test_calculate_auto_liquidity_favored_team(self, services):
        """Test auto-liquidity when radiant is favored (more liquidity on underdog)."""
        stake_service = services["stake_service"]

        # 55% radiant favored -> more liquidity on dire (underdog)
        radiant_auto, dire_auto = stake_service.calculate_auto_liquidity(0.55)
        assert radiant_auto == pytest.approx(22.5)  # 50 × 0.45 (dire_win_prob)
        assert dire_auto == pytest.approx(27.5)  # 50 × 0.55 (radiant_win_prob)

    def test_calculate_auto_liquidity_underdog_team(self, services):
        """Test auto-liquidity when radiant is underdog."""
        stake_service = services["stake_service"]

        # 40% radiant (underdog) -> more liquidity on radiant
        radiant_auto, dire_auto = stake_service.calculate_auto_liquidity(0.40)
        assert radiant_auto == 30.0  # 50 × 0.60
        assert dire_auto == 20.0  # 50 × 0.40

    def test_calculate_excluded_payout_equal_teams(self, services):
        """Test excluded player payout calculation with equal odds."""
        stake_service = services["stake_service"]

        # 50% win probability -> 5 / 0.50 = 10 JC per excluded player
        payout = stake_service.calculate_excluded_payout(0.5, "radiant")
        assert payout == 10

        payout = stake_service.calculate_excluded_payout(0.5, "dire")
        assert payout == 10

    def test_calculate_excluded_payout_favored_team(self, services):
        """Test excluded payout when favored team wins (less reward)."""
        stake_service = services["stake_service"]

        # 55% favorite wins -> 5 / 0.55 = ~9 JC per excluded player
        payout = stake_service.calculate_excluded_payout(0.55, "radiant")
        assert payout == 9  # rounded

        # 45% underdog wins -> 5 / 0.45 = ~11 JC per excluded player
        payout = stake_service.calculate_excluded_payout(0.55, "dire")
        assert payout == 11  # rounded

    def test_calculate_excluded_payout_clamps_extreme_odds(self, services):
        """Test that extreme odds are clamped to min/max probabilities."""
        stake_service = services["stake_service"]

        # Very low probability (5%) should be clamped to 10%
        # 5 / 0.10 = 50 JC
        payout = stake_service.calculate_excluded_payout(0.05, "radiant")
        assert payout == 50

        # Very high probability (95%) should be clamped to 90%
        # 5 / 0.90 = ~6 JC
        payout = stake_service.calculate_excluded_payout(0.95, "radiant")
        assert payout == 6  # rounded

    def test_create_stakes_for_draft(self, services):
        """Test creating stakes for a draft."""
        stake_service = services["stake_service"]
        player_repo = services["player_repo"]

        radiant_ids = [1001, 1002, 1003, 1004, 1005]
        dire_ids = [1006, 1007, 1008, 1009, 1010]
        excluded_ids = [1011, 1012]

        all_ids = radiant_ids + dire_ids + excluded_ids
        _create_players(player_repo, all_ids)

        stake_time = int(time.time())
        result = stake_service.create_stakes_for_draft(
            guild_id=1,
            radiant_ids=radiant_ids,
            dire_ids=dire_ids,
            excluded_ids=excluded_ids,
            radiant_win_prob=0.55,
            stake_time=stake_time,
        )

        assert result["enabled"] is True
        assert result["created"] == 12  # 5 + 5 + 2
        assert result["radiant_count"] == 5
        assert result["dire_count"] == 5
        assert result["excluded_count"] == 2
        assert result["radiant_win_prob"] == 0.55
        assert result["dire_win_prob"] == pytest.approx(0.45)
        # Auto-liquidity distribution
        assert result["radiant_auto"] == pytest.approx(22.5)  # 50 × 0.45
        assert result["dire_auto"] == pytest.approx(27.5)  # 50 × 0.55
        assert result["pool_size"] == 50
        assert result["stake_per_player"] == 5
        # Initial multipliers (no player bets yet)
        assert result["initial_radiant_multiplier"] == pytest.approx(2.22, rel=0.01)
        assert result["initial_dire_multiplier"] == pytest.approx(1.82, rel=0.01)

    def test_place_player_bet(self, services):
        """Test placing a player pool bet."""
        stake_service = services["stake_service"]
        player_repo = services["player_repo"]

        radiant_ids = [1001, 1002, 1003, 1004, 1005]
        dire_ids = [1006, 1007, 1008, 1009, 1010]

        all_ids = radiant_ids + dire_ids
        _create_players(player_repo, all_ids, balance=100)

        stake_time = int(time.time())
        stake_service.create_stakes_for_draft(
            guild_id=1,
            radiant_ids=radiant_ids,
            dire_ids=dire_ids,
            excluded_ids=[],
            radiant_win_prob=0.55,
            stake_time=stake_time,
        )

        pending_state = {
            "stake_radiant_win_prob": 0.55,
            "shuffle_timestamp": stake_time,
        }

        # Player on radiant bets on radiant
        result = stake_service.place_player_bet(
            guild_id=1,
            discord_id=1001,
            team="radiant",
            amount=20,
            pending_state=pending_state,
        )

        assert result["success"] is True
        assert result["team"] == "radiant"
        assert result["amount"] == 20
        assert result["new_balance"] == 80  # 100 - 20

        # Verify balance deducted
        assert player_repo.get_balance(1001) == 80

    def test_place_player_bet_duplicate_fails(self, services):
        """Test that duplicate bets fail."""
        stake_service = services["stake_service"]
        player_repo = services["player_repo"]

        radiant_ids = [1001, 1002, 1003, 1004, 1005]
        dire_ids = [1006, 1007, 1008, 1009, 1010]

        all_ids = radiant_ids + dire_ids
        _create_players(player_repo, all_ids, balance=100)

        stake_time = int(time.time())
        stake_service.create_stakes_for_draft(
            guild_id=1,
            radiant_ids=radiant_ids,
            dire_ids=dire_ids,
            excluded_ids=[],
            radiant_win_prob=0.55,
            stake_time=stake_time,
        )

        pending_state = {
            "stake_radiant_win_prob": 0.55,
            "shuffle_timestamp": stake_time,
        }

        # First bet succeeds
        result1 = stake_service.place_player_bet(1, 1001, "radiant", 20, pending_state)
        assert result1["success"] is True

        # Second bet fails
        result2 = stake_service.place_player_bet(1, 1001, "radiant", 10, pending_state)
        assert result2["success"] is False
        assert "already have a bet" in result2["error"]

    def test_get_pool_state(self, services):
        """Test getting current pool state."""
        stake_service = services["stake_service"]
        player_repo = services["player_repo"]

        radiant_ids = [1001, 1002, 1003, 1004, 1005]
        dire_ids = [1006, 1007, 1008, 1009, 1010]

        all_ids = radiant_ids + dire_ids
        _create_players(player_repo, all_ids, balance=100)

        stake_time = int(time.time())
        stake_service.create_stakes_for_draft(
            guild_id=1,
            radiant_ids=radiant_ids,
            dire_ids=dire_ids,
            excluded_ids=[],
            radiant_win_prob=0.55,
            stake_time=stake_time,
        )

        pending_state = {
            "stake_radiant_win_prob": 0.55,
            "shuffle_timestamp": stake_time,
        }

        # Add player bets
        stake_service.place_player_bet(1, 1001, "radiant", 10, pending_state)
        stake_service.place_player_bet(1, 1006, "dire", 15, pending_state)

        pool_state = stake_service.get_pool_state(1, pending_state)

        assert pool_state.radiant_auto == pytest.approx(22.5)
        assert pool_state.dire_auto == pytest.approx(27.5)
        assert pool_state.radiant_bets == 10
        assert pool_state.dire_bets == 15
        assert pool_state.radiant_total == pytest.approx(32.5)
        assert pool_state.dire_total == pytest.approx(42.5)
        assert pool_state.total_pool == pytest.approx(75.0)

    def test_settle_stakes_excluded_only(self, services):
        """Test settling stakes pays excluded players based on odds."""
        stake_service = services["stake_service"]
        player_repo = services["player_repo"]

        radiant_ids = [1001, 1002, 1003, 1004, 1005]
        dire_ids = [1006, 1007, 1008, 1009, 1010]
        excluded_ids = [1011, 1012]

        all_ids = radiant_ids + dire_ids + excluded_ids
        _create_players(player_repo, all_ids)

        stake_time = int(time.time())
        stake_service.create_stakes_for_draft(
            guild_id=1,
            radiant_ids=radiant_ids,
            dire_ids=dire_ids,
            excluded_ids=excluded_ids,
            radiant_win_prob=0.55,
            stake_time=stake_time,
        )

        pending_state = {
            "stake_radiant_win_prob": 0.55,
            "shuffle_timestamp": stake_time,
        }

        # Dire wins (underdog) -> excluded payout = 5 / 0.45 = 11 JC
        result = stake_service.settle_stakes(
            match_id=1,
            guild_id=1,
            winning_team="dire",
            pending_state=pending_state,
        )

        assert result["enabled"] is True
        assert result["winning_team"] == "dire"

        # Excluded players get minted payout
        excluded_result = result["excluded"]
        assert excluded_result["payout_per_player"] == 11
        assert len(excluded_result["winners"]) == 2
        assert excluded_result["total_payout"] == 22  # 2 × 11

        # Check excluded player balances
        for pid in excluded_ids:
            balance = player_repo.get_balance(pid)
            expected = INITIAL_JOPACOIN + 11
            assert balance == expected, f"Player {pid} should have {expected} JC"

    def test_settle_stakes_with_player_bets(self, services):
        """Test settling when players have placed bets."""
        stake_service = services["stake_service"]
        player_repo = services["player_repo"]

        radiant_ids = [1001, 1002, 1003, 1004, 1005]
        dire_ids = [1006, 1007, 1008, 1009, 1010]

        all_ids = radiant_ids + dire_ids
        _create_players(player_repo, all_ids, balance=100)

        stake_time = int(time.time())
        stake_service.create_stakes_for_draft(
            guild_id=1,
            radiant_ids=radiant_ids,
            dire_ids=dire_ids,
            excluded_ids=[],
            radiant_win_prob=0.55,
            stake_time=stake_time,
        )

        pending_state = {
            "stake_radiant_win_prob": 0.55,
            "shuffle_timestamp": stake_time,
        }

        # Place player bets
        stake_service.place_player_bet(1, 1001, "radiant", 20, pending_state)  # Radiant player bets 20
        stake_service.place_player_bet(1, 1006, "dire", 10, pending_state)  # Dire player bets 10

        # Pool state: radiant_total = 22.5 + 20 = 42.5, dire_total = 27.5 + 10 = 37.5
        # Total = 80

        # Radiant wins
        result = stake_service.settle_stakes(
            match_id=1,
            guild_id=1,
            winning_team="radiant",
            pending_state=pending_state,
        )

        player_bets = result["player_bets"]
        # Multiplier = 80 / 42.5 = 1.88
        assert player_bets["multiplier"] == pytest.approx(1.88, rel=0.01)
        assert len(player_bets["winners"]) == 1  # Only player 1001 bet
        assert len(player_bets["losers"]) == 1  # Player 1006 lost

        # Winning bettor payout: 20 × 1.88 = ~37
        winner = player_bets["winners"][0]
        assert winner["discord_id"] == 1001
        assert winner["payout"] == pytest.approx(37, abs=2)

    def test_clear_stakes_refunds_player_bets(self, services):
        """Test clearing stakes refunds player pool bets."""
        stake_service = services["stake_service"]
        player_repo = services["player_repo"]

        radiant_ids = [1001, 1002, 1003, 1004, 1005]
        dire_ids = [1006, 1007, 1008, 1009, 1010]

        all_ids = radiant_ids + dire_ids
        _create_players(player_repo, all_ids, balance=100)

        stake_time = int(time.time())
        stake_service.create_stakes_for_draft(
            guild_id=1,
            radiant_ids=radiant_ids,
            dire_ids=dire_ids,
            excluded_ids=[],
            radiant_win_prob=0.5,
            stake_time=stake_time,
        )

        pending_state = {"shuffle_timestamp": stake_time, "stake_radiant_win_prob": 0.5}

        # Place a player bet
        stake_service.place_player_bet(1, 1001, "radiant", 30, pending_state)
        assert player_repo.get_balance(1001) == 70  # 100 - 30

        # Clear stakes (abort)
        result = stake_service.clear_stakes(1, pending_state)

        assert result["deleted"] == 10  # Stakes deleted
        assert result["refunded_bets"] == 1  # Player bet refunded
        assert result["refund_amount"] == 30

        # Verify balance restored
        assert player_repo.get_balance(1001) == 100

    def test_disabled_service(self, services):
        """Test that disabled service returns appropriate responses."""
        stake_service = services["stake_service"]
        stake_service.config.enabled = False

        result = stake_service.create_stakes_for_draft(
            guild_id=1,
            radiant_ids=[1],
            dire_ids=[2],
            excluded_ids=[],
            radiant_win_prob=0.5,
            stake_time=int(time.time()),
        )

        assert result["enabled"] is False
        assert result["created"] == 0

    def test_format_stake_pool_info(self, services):
        """Test formatting stake pool info for embed display."""
        stake_service = services["stake_service"]

        # Radiant favored (55%)
        info = stake_service.format_stake_pool_info(0.55, excluded_count=2)
        assert info["favored"] == "radiant"
        # Auto-liquidity: radiant 22.5, dire 27.5
        assert info["radiant_auto"] == pytest.approx(22.5)
        assert info["dire_auto"] == pytest.approx(27.5)
        # Multipliers: radiant = 50/22.5 = 2.22, dire = 50/27.5 = 1.82
        assert info["radiant_multiplier"] == pytest.approx(2.22, rel=0.01)
        assert info["dire_multiplier"] == pytest.approx(1.82, rel=0.01)
        # Excluded payouts: 5/0.55 = 9, 5/0.45 = 11
        assert info["excluded_payout_if_radiant_wins"] == 9
        assert info["excluded_payout_if_dire_wins"] == 11
        assert info["excluded_count"] == 2

        # Even teams
        info = stake_service.format_stake_pool_info(0.50, excluded_count=0)
        assert info["favored"] == "even"
        assert info["radiant_multiplier"] == pytest.approx(2.0, rel=0.01)
        assert info["dire_multiplier"] == pytest.approx(2.0, rel=0.01)

        # Dire favored (45% radiant)
        info = stake_service.format_stake_pool_info(0.45, excluded_count=0)
        assert info["favored"] == "dire"

    def test_get_player_stats(self, services):
        """Test getting player stake statistics."""
        stake_service = services["stake_service"]
        player_repo = services["player_repo"]

        radiant_ids = [1001, 1002, 1003, 1004, 1005]
        dire_ids = [1006, 1007, 1008, 1009, 1010]

        all_ids = radiant_ids + dire_ids
        _create_players(player_repo, all_ids)

        stake_time = int(time.time())
        stake_service.create_stakes_for_draft(
            guild_id=1,
            radiant_ids=radiant_ids,
            dire_ids=dire_ids,
            excluded_ids=[],
            radiant_win_prob=0.5,
            stake_time=stake_time,
        )

        pending_state = {
            "stake_radiant_win_prob": 0.5,
            "shuffle_timestamp": stake_time,
        }

        stake_service.settle_stakes(
            match_id=1,
            guild_id=1,
            winning_team="radiant",
            pending_state=pending_state,
        )

        # Check stats for a winner (drafted player gets payout via excluded logic)
        # Note: In new system, drafted players don't get auto-payout unless they're excluded
        # Only excluded players get minted payouts
        stats = stake_service.get_player_stats(1001)
        assert stats["total_stakes"] == 1

        # Check stats for a loser
        stats = stake_service.get_player_stats(1006)
        assert stats["total_stakes"] == 1


class TestPoolState:
    """Test PoolState dataclass."""

    def test_pool_state_totals(self):
        """Test pool state total calculations."""
        state = PoolState(
            radiant_auto=22.5,
            dire_auto=27.5,
            radiant_bets=10,
            dire_bets=15,
            radiant_win_prob=0.55,
        )

        assert state.radiant_total == 32.5
        assert state.dire_total == 42.5
        assert state.total_pool == 75.0

    def test_pool_state_multiplier(self):
        """Test pool state multiplier calculation."""
        state = PoolState(
            radiant_auto=25.0,
            dire_auto=25.0,
            radiant_bets=0,
            dire_bets=0,
            radiant_win_prob=0.5,
        )

        assert state.get_multiplier("radiant") == 2.0
        assert state.get_multiplier("dire") == 2.0

    def test_pool_state_multiplier_with_bets(self):
        """Test multiplier shifts with player bets."""
        state = PoolState(
            radiant_auto=22.5,
            dire_auto=27.5,
            radiant_bets=20,  # Player bets on radiant
            dire_bets=0,
            radiant_win_prob=0.55,
        )

        # Total pool = 70, radiant_total = 42.5
        # Radiant multiplier = 70 / 42.5 = 1.65
        assert state.get_multiplier("radiant") == pytest.approx(1.65, rel=0.01)
        # Dire multiplier = 70 / 27.5 = 2.55
        assert state.get_multiplier("dire") == pytest.approx(2.55, rel=0.01)


class TestStakeRepository:
    """Test stake repository functionality."""

    def test_create_and_get_pending_stakes(self, services):
        """Test creating and retrieving pending stakes."""
        stake_repo = services["stake_repo"]
        player_repo = services["player_repo"]

        radiant_ids = [1001, 1002, 1003, 1004, 1005]
        dire_ids = [1006, 1007, 1008, 1009, 1010]
        excluded_ids = [1011]

        all_ids = radiant_ids + dire_ids + excluded_ids
        _create_players(player_repo, all_ids)

        stake_time = int(time.time())
        result = stake_repo.create_stakes(
            guild_id=1,
            radiant_ids=radiant_ids,
            dire_ids=dire_ids,
            excluded_ids=excluded_ids,
            stake_time=stake_time,
        )

        assert result["created"] == 11

        # Get pending stakes
        pending = stake_repo.get_pending_stakes(1, stake_time)
        assert len(pending) == 11

        # Verify team assignments
        radiant_stakes = [s for s in pending if s["team"] == "radiant"]
        dire_stakes = [s for s in pending if s["team"] == "dire"]
        excluded_stakes = [s for s in pending if s["team"] == "excluded"]

        assert len(radiant_stakes) == 5
        assert len(dire_stakes) == 5
        assert len(excluded_stakes) == 1

    def test_settle_stakes_atomic(self, services):
        """Test atomic stake settlement."""
        stake_repo = services["stake_repo"]
        player_repo = services["player_repo"]

        radiant_ids = [1001, 1002, 1003, 1004, 1005]
        dire_ids = [1006, 1007, 1008, 1009, 1010]
        excluded_ids = [1011]

        all_ids = radiant_ids + dire_ids + excluded_ids
        _create_players(player_repo, all_ids)

        stake_time = int(time.time())
        stake_repo.create_stakes(
            guild_id=1,
            radiant_ids=radiant_ids,
            dire_ids=dire_ids,
            excluded_ids=excluded_ids,
            stake_time=stake_time,
        )

        result = stake_repo.settle_stakes_atomic(
            match_id=1,
            guild_id=1,
            since_ts=stake_time,
            winning_team="radiant",
            payout_per_participant=8,  # Parimutuel payout for winning team
            payout_per_excluded=12,  # Odds-based payout for excluded
        )

        # Winners: radiant participants (5) + excluded (1) = 6
        assert len(result["winners"]) == 6
        assert len(result["losers"]) == 5
        # Total payout: 5 participants × 8 + 1 excluded × 12 = 40 + 12 = 52
        assert result["total_payout"] == 52

        # Verify pending stakes are now settled (have match_id)
        pending = stake_repo.get_pending_stakes(1, stake_time)
        assert len(pending) == 0

    def test_delete_stakes(self, services):
        """Test deleting pending stakes."""
        stake_repo = services["stake_repo"]
        player_repo = services["player_repo"]

        radiant_ids = [1001, 1002, 1003, 1004, 1005]
        dire_ids = [1006, 1007, 1008, 1009, 1010]

        all_ids = radiant_ids + dire_ids
        _create_players(player_repo, all_ids)

        stake_time = int(time.time())
        stake_repo.create_stakes(
            guild_id=1,
            radiant_ids=radiant_ids,
            dire_ids=dire_ids,
            excluded_ids=[],
            stake_time=stake_time,
        )

        deleted = stake_repo.delete_stakes(1, stake_time)
        assert deleted == 10

        pending = stake_repo.get_pending_stakes(1, stake_time)
        assert len(pending) == 0

    def test_guild_isolation(self, services):
        """Test that stakes are isolated by guild."""
        stake_repo = services["stake_repo"]
        player_repo = services["player_repo"]

        ids = [1001, 1002, 1003, 1004, 1005, 1006, 1007, 1008, 1009, 1010]
        _create_players(player_repo, ids)

        stake_time = int(time.time())

        # Create stakes for guild 1
        stake_repo.create_stakes(
            guild_id=1,
            radiant_ids=ids[:5],
            dire_ids=ids[5:],
            excluded_ids=[],
            stake_time=stake_time,
        )

        # Create stakes for guild 2
        stake_repo.create_stakes(
            guild_id=2,
            radiant_ids=ids[:5],
            dire_ids=ids[5:],
            excluded_ids=[],
            stake_time=stake_time,
        )

        # Each guild should have 10 stakes
        pending_1 = stake_repo.get_pending_stakes(1, stake_time)
        pending_2 = stake_repo.get_pending_stakes(2, stake_time)

        assert len(pending_1) == 10
        assert len(pending_2) == 10

        # Delete only guild 1 stakes
        deleted = stake_repo.delete_stakes(1, stake_time)
        assert deleted == 10

        # Guild 1 should be empty, guild 2 should still have stakes
        pending_1 = stake_repo.get_pending_stakes(1, stake_time)
        pending_2 = stake_repo.get_pending_stakes(2, stake_time)

        assert len(pending_1) == 0
        assert len(pending_2) == 10


class TestPlayerPoolBetRepository:
    """Test player pool bet repository functionality."""

    def test_create_bet_atomic(self, services):
        """Test creating a player pool bet."""
        player_pool_bet_repo = services["player_pool_bet_repo"]
        player_repo = services["player_repo"]

        _create_players(player_repo, [1001], balance=100)

        bet_time = int(time.time())
        result = player_pool_bet_repo.create_bet_atomic(
            guild_id=1,
            discord_id=1001,
            team="radiant",
            amount=25,
            bet_time=bet_time,
        )

        assert result["bet_id"] is not None
        assert result["discord_id"] == 1001
        assert result["team"] == "radiant"
        assert result["amount"] == 25
        assert result["new_balance"] == 75

        # Verify balance deducted
        assert player_repo.get_balance(1001) == 75

    def test_get_pool_totals(self, services):
        """Test getting pool totals by team."""
        player_pool_bet_repo = services["player_pool_bet_repo"]
        player_repo = services["player_repo"]

        _create_players(player_repo, [1001, 1002], balance=100)

        bet_time = int(time.time())
        player_pool_bet_repo.create_bet_atomic(1, 1001, "radiant", 20, bet_time)
        player_pool_bet_repo.create_bet_atomic(1, 1002, "dire", 35, bet_time)

        totals = player_pool_bet_repo.get_pool_totals(1, bet_time - 10)

        assert totals["radiant"] == 20
        assert totals["dire"] == 35
        assert totals["total"] == 55

    def test_refund_bets_atomic(self, services):
        """Test refunding player pool bets."""
        player_pool_bet_repo = services["player_pool_bet_repo"]
        player_repo = services["player_repo"]

        _create_players(player_repo, [1001], balance=100)

        bet_time = int(time.time())
        player_pool_bet_repo.create_bet_atomic(1, 1001, "radiant", 30, bet_time)
        assert player_repo.get_balance(1001) == 70

        result = player_pool_bet_repo.refund_bets_atomic(1, bet_time - 10)

        assert result["refunded"] == 1
        assert result["total_amount"] == 30
        assert player_repo.get_balance(1001) == 100
