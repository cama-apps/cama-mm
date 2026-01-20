"""
Tests for the SpectatorPoolService.
"""

import pytest
import time

from infrastructure.schema_manager import SchemaManager
from repositories.player_repository import PlayerRepository
from repositories.spectator_bet_repository import SpectatorBetRepository
from services.spectator_pool_service import SpectatorPoolConfig, SpectatorPoolService


@pytest.fixture
def spectator_db(tmp_path):
    """Create a temporary database with schema for spectator pool tests."""
    db_path = str(tmp_path / "test_spectator.db")
    schema_manager = SchemaManager(db_path)
    schema_manager.initialize()
    return db_path


@pytest.fixture
def player_repo(spectator_db):
    """Create a PlayerRepository instance."""
    return PlayerRepository(spectator_db)


@pytest.fixture
def spectator_bet_repo(spectator_db):
    """Create a SpectatorBetRepository instance."""
    return SpectatorBetRepository(spectator_db)


@pytest.fixture
def spectator_pool_service(spectator_bet_repo, player_repo):
    """Create a SpectatorPoolService instance."""
    config = SpectatorPoolConfig(enabled=True, player_cut=0.10)
    return SpectatorPoolService(spectator_bet_repo, player_repo, config)


@pytest.fixture
def sample_players(player_repo):
    """Create sample players for testing."""
    players = []
    for i in range(1, 6):
        player_repo.add(
            discord_id=1000 + i,
            discord_username=f"Player{i}",
            initial_mmr=3000,
            glicko_rating=1500.0,
            glicko_rd=100.0,
            glicko_volatility=0.06,
        )
        # Give each player some balance
        player_repo.update_balance(1000 + i, 100)
        players.append(1000 + i)
    return players


@pytest.fixture
def pending_state():
    """Create a sample pending state."""
    return {
        "shuffle_timestamp": int(time.time()) - 10,
        "radiant_ids": [1001, 1002, 1003, 1004, 1005],
        "dire_ids": [2001, 2002, 2003, 2004, 2005],
        "is_draft": True,
    }


class TestSpectatorPoolService:
    """Tests for SpectatorPoolService."""

    def test_place_bet_success(self, spectator_pool_service, sample_players, pending_state):
        """Test placing a spectator bet successfully."""
        # Create a spectator (not in the match)
        spectator_id = sample_players[0]  # 1001 is in radiant_ids, so use another
        # Actually, let's create a new player as spectator
        spectator_pool_service.player_repo.add(
            discord_id=9999,
            discord_username="Spectator",
            initial_mmr=3000,
            glicko_rating=1500.0,
            glicko_rd=100.0,
            glicko_volatility=0.06,
        )
        spectator_pool_service.player_repo.update_balance(9999, 100)

        result = spectator_pool_service.place_bet(
            guild_id=1,
            discord_id=9999,
            team="radiant",
            amount=10,
            pending_state=pending_state,
        )

        assert result["success"] is True
        assert result["team"] == "radiant"
        assert result["amount"] == 10
        assert "bet_id" in result

    def test_place_bet_insufficient_balance(self, spectator_pool_service, pending_state):
        """Test placing a bet with insufficient balance."""
        # Create a spectator with low balance
        spectator_pool_service.player_repo.add(
            discord_id=8888,
            discord_username="BrokeSpectator",
            initial_mmr=3000,
        )
        spectator_pool_service.player_repo.update_balance(8888, 5)

        result = spectator_pool_service.place_bet(
            guild_id=1,
            discord_id=8888,
            team="dire",
            amount=100,
            pending_state=pending_state,
        )

        assert result["success"] is False
        assert "Insufficient balance" in result["error"]

    def test_place_bet_duplicate(self, spectator_pool_service, pending_state):
        """Test placing duplicate bets."""
        spectator_pool_service.player_repo.add(
            discord_id=7777,
            discord_username="DuplicateBettor",
            initial_mmr=3000,
        )
        spectator_pool_service.player_repo.update_balance(7777, 100)

        # First bet should succeed
        result1 = spectator_pool_service.place_bet(
            guild_id=1,
            discord_id=7777,
            team="radiant",
            amount=10,
            pending_state=pending_state,
        )
        assert result1["success"] is True

        # Second bet should fail
        result2 = spectator_pool_service.place_bet(
            guild_id=1,
            discord_id=7777,
            team="dire",
            amount=10,
            pending_state=pending_state,
        )
        assert result2["success"] is False
        assert "already have a bet" in result2["error"]

    def test_settle_bets_radiant_wins(self, spectator_pool_service, pending_state):
        """Test settling bets when radiant wins."""
        # Create two spectators
        for i in [6666, 7777]:
            spectator_pool_service.player_repo.add(
                discord_id=i,
                discord_username=f"Spec{i}",
                initial_mmr=3000,
            )
            spectator_pool_service.player_repo.update_balance(i, 100)

        # Place bets
        spectator_pool_service.place_bet(1, 6666, "radiant", 20, pending_state)
        spectator_pool_service.place_bet(1, 7777, "dire", 30, pending_state)

        # Create winning players
        winning_player_ids = [1001, 1002, 1003, 1004, 1005]
        for pid in winning_player_ids:
            try:
                spectator_pool_service.player_repo.add(
                    discord_id=pid,
                    discord_username=f"Winner{pid}",
                    initial_mmr=3000,
                )
            except Exception:
                pass  # Player might already exist
            spectator_pool_service.player_repo.update_balance(pid, 0)

        # Settle bets
        result = spectator_pool_service.settle_bets(
            match_id=1,
            guild_id=1,
            winning_team="radiant",
            winning_player_ids=winning_player_ids,
            pending_state=pending_state,
        )

        assert result["enabled"] is True
        assert result["winning_team"] == "radiant"
        assert result["total_pool"] == 50  # 20 + 30
        assert len(result["winners"]) == 1  # Only the radiant bettor
        assert len(result["losers"]) == 1  # The dire bettor

        # 90% of pool goes to bettors, 10% to players
        assert result["player_bonus"] == 5  # 10% of 50

    def test_settle_bets_empty_pool(self, spectator_pool_service, pending_state):
        """Test settling when no bets were placed."""
        result = spectator_pool_service.settle_bets(
            match_id=1,
            guild_id=1,
            winning_team="radiant",
            winning_player_ids=[1001],
            pending_state=pending_state,
        )

        assert result["enabled"] is True
        assert result["total_pool"] == 0
        assert result["player_bonus"] == 0

    def test_refund_bets(self, spectator_pool_service, pending_state):
        """Test refunding spectator bets on match abort."""
        spectator_pool_service.player_repo.add(
            discord_id=5555,
            discord_username="RefundMe",
            initial_mmr=3000,
        )
        spectator_pool_service.player_repo.update_balance(5555, 100)

        # Place a bet
        spectator_pool_service.place_bet(1, 5555, "radiant", 25, pending_state)

        # Verify balance was deducted
        balance_after_bet = spectator_pool_service.player_repo.get_balance(5555)
        assert balance_after_bet == 75

        # Refund
        result = spectator_pool_service.refund_bets(1, pending_state)

        assert result["enabled"] is True
        assert result["refunded"] == 1
        assert result["total_amount"] == 25

        # Verify balance was restored
        balance_after_refund = spectator_pool_service.player_repo.get_balance(5555)
        assert balance_after_refund == 100

    def test_get_pool_info(self, spectator_pool_service, pending_state):
        """Test getting pool info for display."""
        spectator_pool_service.player_repo.add(
            discord_id=4444,
            discord_username="InfoTest",
            initial_mmr=3000,
        )
        spectator_pool_service.player_repo.update_balance(4444, 100)

        # Place a bet
        spectator_pool_service.place_bet(1, 4444, "dire", 40, pending_state)

        info = spectator_pool_service.get_pool_info(1, pending_state)

        assert info["enabled"] is True
        assert info["dire_total"] == 40
        assert info["radiant_total"] == 0
        assert info["total_pool"] == 40

    def test_disabled_service(self, spectator_bet_repo, player_repo, pending_state):
        """Test that disabled service returns appropriate responses."""
        config = SpectatorPoolConfig(enabled=False, player_cut=0.10)
        service = SpectatorPoolService(spectator_bet_repo, player_repo, config)

        result = service.place_bet(1, 1234, "radiant", 10, pending_state)
        assert result["success"] is False
        assert "disabled" in result["error"].lower()


class TestSpectatorBetRepository:
    """Tests for SpectatorBetRepository."""

    def test_create_bet(self, spectator_bet_repo, player_repo):
        """Test creating a spectator bet."""
        player_repo.add(
            discord_id=3333,
            discord_username="RepoTest",
            initial_mmr=3000,
        )
        player_repo.update_balance(3333, 100)

        bet_id = spectator_bet_repo.create_bet(
            guild_id=1,
            discord_id=3333,
            team="radiant",
            amount=15,
            bet_time=int(time.time()),
        )

        assert bet_id is not None
        assert bet_id > 0

        # Verify balance was deducted
        assert player_repo.get_balance(3333) == 85

    def test_get_pool_totals(self, spectator_bet_repo, player_repo):
        """Test getting pool totals by team."""
        now = int(time.time())

        for i in [2222, 3333]:
            player_repo.add(discord_id=i, discord_username=f"P{i}", initial_mmr=3000)
            player_repo.update_balance(i, 100)

        spectator_bet_repo.create_bet(1, 2222, "radiant", 20, now)
        spectator_bet_repo.create_bet(1, 3333, "dire", 35, now)

        totals = spectator_bet_repo.get_pool_totals(1, now - 10)

        assert totals["radiant"] == 20
        assert totals["dire"] == 35
        assert totals["total"] == 55

    def test_settle_bets_atomic(self, spectator_bet_repo, player_repo):
        """Test atomic bet settlement."""
        now = int(time.time())

        player_repo.add(discord_id=1111, discord_username="Winner", initial_mmr=3000)
        player_repo.update_balance(1111, 100)

        spectator_bet_repo.create_bet(1, 1111, "radiant", 50, now)

        # Settle with 2x multiplier (from parimutuel calculation)
        result = spectator_bet_repo.settle_bets_atomic(
            match_id=1,
            guild_id=1,
            since_ts=now - 10,
            winning_team="radiant",
            payout_multiplier=2.0,
        )

        assert len(result["winners"]) == 1
        assert result["winners"][0]["payout"] == 100  # 50 * 2.0
        assert result["total_payout"] == 100

        # Verify balance was updated
        # Started with 100, bet 50, won 100 back = 150
        assert player_repo.get_balance(1111) == 150
