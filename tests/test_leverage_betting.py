"""
Tests for leverage betting, negative balances, and garnishment.
"""

import time

import pytest

from infrastructure.schema_manager import SchemaManager
from repositories.bet_repository import BetRepository
from repositories.player_repository import PlayerRepository
from services.betting_service import BettingService
from services.garnishment_service import GarnishmentService

TEST_GUILD_ID = 12345


@pytest.fixture
def temp_db(tmp_path):
    """Create a temporary database with schema."""
    db_path = str(tmp_path / "test.db")
    schema = SchemaManager(db_path)
    schema.initialize()
    return db_path


@pytest.fixture
def player_repo(temp_db):
    return PlayerRepository(temp_db)


@pytest.fixture
def bet_repo(temp_db):
    return BetRepository(temp_db)


@pytest.fixture
def betting_service(bet_repo, player_repo):
    return BettingService(
        bet_repo,
        player_repo,
        leverage_tiers=[2, 3, 5],
        max_debt=500,
    )


class TestLeverageBetting:
    """Tests for leverage betting functionality."""

    def test_leverage_bet_deducts_effective_amount(self, bet_repo, player_repo):
        """Betting 10 at 3x leverage should deduct 30."""
        # Setup player with 100 balance
        player_repo.add(
            discord_id=1001,
            discord_username="TestPlayer",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1500,
            preferred_roles=["1", "2"],
        )
        player_repo.update_balance(1001, TEST_GUILD_ID, 100)

        now_ts = int(time.time())
        bet_repo.place_bet_atomic(
            guild_id=TEST_GUILD_ID,
            discord_id=1001,
            team="radiant",
            amount=10,
            bet_time=now_ts,
            since_ts=now_ts - 1,
            leverage=3,
            max_debt=500,
        )

        # Should have deducted 30 (10 * 3)
        balance = player_repo.get_balance(1001, TEST_GUILD_ID)
        assert balance == 70  # 100 - 30

    def test_leverage_win_pays_based_on_effective_bet(self, bet_repo, player_repo):
        """Winning 10 at 3x on house mode should return 60 (30 + 30)."""
        # Setup player
        player_repo.add(
            discord_id=1002,
            discord_username="Winner",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1500,
            preferred_roles=["1"],
        )
        player_repo.update_balance(1002, TEST_GUILD_ID, 100)

        now_ts = int(time.time())
        bet_repo.place_bet_atomic(
            guild_id=TEST_GUILD_ID,
            discord_id=1002,
            team="radiant",
            amount=10,
            bet_time=now_ts,
            since_ts=now_ts - 1,
            leverage=3,
            max_debt=500,
        )

        # Balance is now 70 after betting 30
        assert player_repo.get_balance(1002, TEST_GUILD_ID) == 70

        # Settle as Radiant wins
        distributions = bet_repo.settle_pending_bets_atomic(
            match_id=100,
            guild_id=TEST_GUILD_ID,
            since_ts=now_ts - 1,
            winning_team="radiant",
            house_payout_multiplier=1.0,
            betting_mode="house",
        )

        # Winner should get 60 (30 effective bet * 2 for 1:1 payout)
        balance = player_repo.get_balance(1002, TEST_GUILD_ID)
        assert balance == 130  # 70 + 60

        # Check distribution details
        assert len(distributions["winners"]) == 1
        assert distributions["winners"][0]["payout"] == 60
        assert distributions["winners"][0]["leverage"] == 3
        assert distributions["winners"][0]["effective_bet"] == 30

    def test_leverage_loss_pushes_into_debt(self, bet_repo, player_repo):
        """Losing a leverage bet can push balance negative."""
        # Setup player with 20 balance
        player_repo.add(
            discord_id=1003,
            discord_username="Debtor",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1500,
            preferred_roles=["1"],
        )
        player_repo.update_balance(1003, TEST_GUILD_ID, 20)

        now_ts = int(time.time())
        # Bet 10 at 5x = 50 effective, but only have 20
        bet_repo.place_bet_atomic(
            guild_id=TEST_GUILD_ID,
            discord_id=1003,
            team="radiant",
            amount=10,
            bet_time=now_ts,
            since_ts=now_ts - 1,
            leverage=5,
            max_debt=500,
        )

        # Balance should be -30 (20 - 50)
        balance = player_repo.get_balance(1003, TEST_GUILD_ID)
        assert balance == -30

    def test_max_debt_blocks_excessive_leverage(self, bet_repo, player_repo):
        """Cannot place bet that would exceed max debt."""
        # Setup player with 0 balance
        player_repo.add(
            discord_id=1004,
            discord_username="Blocked",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1500,
            preferred_roles=["1"],
        )
        player_repo.update_balance(1004, TEST_GUILD_ID, 0)

        now_ts = int(time.time())
        # Trying to bet 200 at 5x = 1000 effective, but max debt is 500
        with pytest.raises(ValueError, match="exceed maximum debt limit"):
            bet_repo.place_bet_atomic(
                guild_id=TEST_GUILD_ID,
                discord_id=1004,
                team="radiant",
                amount=200,
                bet_time=now_ts,
                since_ts=now_ts - 1,
                leverage=5,
                max_debt=500,
            )

    def test_invalid_leverage_tier_rejected(self, betting_service, player_repo):
        """Leverage of 4x (not in [2,3,5]) should be rejected."""
        player_repo.add(
            discord_id=1005,
            discord_username="InvalidLev",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1500,
            preferred_roles=["1"],
        )
        player_repo.update_balance(1005, TEST_GUILD_ID, 100)

        now_ts = int(time.time())
        pending_state = {
            "shuffle_timestamp": now_ts - 1,
            "bet_lock_until": now_ts + 600,
            "radiant_team_ids": [],
            "dire_team_ids": [],
        }

        with pytest.raises(ValueError, match="Invalid leverage"):
            betting_service.place_bet(
                guild_id=TEST_GUILD_ID,
                discord_id=1005,
                team="radiant",
                amount=10,
                pending_state=pending_state,
                leverage=4,  # Not in [2, 3, 5]
            )


class TestGarnishment:
    """Tests for garnishment on debt repayment."""

    def test_garnishment_applied_when_in_debt(self, player_repo):
        """50% of winnings should go to debt repayment."""
        # Setup player with -100 balance (in debt)
        player_repo.add(
            discord_id=2001,
            discord_username="InDebt",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1500,
            preferred_roles=["1"],
        )
        player_repo.update_balance(2001, TEST_GUILD_ID, -100)

        # Apply 20 income with 50% garnishment
        result = player_repo.add_balance_with_garnishment(2001, TEST_GUILD_ID, 20, 0.5)

        assert result["gross"] == 20
        assert result["garnished"] == 10
        assert result["net"] == 10

        # Balance should be -80 (debt reduced by full 20)
        balance = player_repo.get_balance(2001, TEST_GUILD_ID)
        assert balance == -80

    def test_no_garnishment_when_positive_balance(self, player_repo):
        """No garnishment for players without debt."""
        # Setup player with positive balance
        player_repo.add(
            discord_id=2002,
            discord_username="Positive",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1500,
            preferred_roles=["1"],
        )
        player_repo.update_balance(2002, TEST_GUILD_ID, 100)

        # Apply 20 income
        result = player_repo.add_balance_with_garnishment(2002, TEST_GUILD_ID, 20, 0.5)

        assert result["gross"] == 20
        assert result["garnished"] == 0
        assert result["net"] == 20

        # Balance should be 120
        balance = player_repo.get_balance(2002, TEST_GUILD_ID)
        assert balance == 120

    def test_garnishment_service_add_income(self, player_repo):
        """Test GarnishmentService.add_income()."""
        # Setup player with debt
        player_repo.add(
            discord_id=2003,
            discord_username="Debtor",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1500,
            preferred_roles=["1"],
        )
        player_repo.update_balance(2003, TEST_GUILD_ID, -50)

        service = GarnishmentService(player_repo, garnishment_rate=0.5)
        result = service.add_income(2003, 10, guild_id=TEST_GUILD_ID)

        assert result["gross"] == 10
        assert result["garnished"] == 5
        assert result["net"] == 5

        # Debt reduced from -50 to -40
        assert player_repo.get_balance(2003, TEST_GUILD_ID) == -40


class TestLeverageIntegration:
    """Integration tests for full leverage betting flow."""

    def test_full_leverage_betting_flow(self, bet_repo, player_repo):
        """E2E: bet with leverage, go into debt, cannot bet again until out of debt."""
        # Setup player with 50 balance
        player_repo.add(
            discord_id=4001,
            discord_username="Gambler",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1500,
            preferred_roles=["1"],
        )
        player_repo.update_balance(4001, TEST_GUILD_ID, 50)

        now_ts = int(time.time())

        # First bet: 20 at 5x leverage = 100 effective
        bet_repo.place_bet_atomic(
            guild_id=TEST_GUILD_ID,
            discord_id=4001,
            team="radiant",
            amount=20,
            bet_time=now_ts,
            since_ts=now_ts - 1,
            leverage=5,
            max_debt=500,
        )
        assert player_repo.get_balance(4001, TEST_GUILD_ID) == -50  # 50 - 100 = -50 debt

        # Lose the bet (Dire wins)
        bet_repo.settle_pending_bets_atomic(
            match_id=100,
            guild_id=TEST_GUILD_ID,
            since_ts=now_ts - 1,
            winning_team="dire",
            house_payout_multiplier=1.0,
            betting_mode="house",
        )
        assert player_repo.get_balance(4001, TEST_GUILD_ID) == -50  # Still -50, lost bet

        # Once in debt, cannot place ANY bets (even leverage)
        now_ts2 = now_ts + 100
        with pytest.raises(ValueError, match="cannot place bets while in debt"):
            bet_repo.place_bet_atomic(
                guild_id=TEST_GUILD_ID,
                discord_id=4001,
                team="radiant",
                amount=10,
                bet_time=now_ts2,
                since_ts=now_ts2 - 1,
                leverage=2,
                max_debt=500,
            )

        # User must win games to pay off debt via garnishment
        # Simulate getting participation reward (garnished to pay debt)
        player_repo.add_balance(4001, TEST_GUILD_ID, 10)  # e.g., from garnished winnings
        assert player_repo.get_balance(4001, TEST_GUILD_ID) == -40

        # Add more until positive
        player_repo.add_balance(4001, TEST_GUILD_ID, 50)
        assert player_repo.get_balance(4001, TEST_GUILD_ID) == 10  # Back in positive!

        # Now can bet again
        now_ts3 = now_ts + 200
        bet_repo.place_bet_atomic(
            guild_id=TEST_GUILD_ID,
            discord_id=4001,
            team="radiant",
            amount=5,
            bet_time=now_ts3,
            since_ts=now_ts3 - 1,
            leverage=1,
            max_debt=500,
        )
        assert player_repo.get_balance(4001, TEST_GUILD_ID) == 5  # 10 - 5 = 5

    def test_leverage_pool_mode_proportional_payout(self, bet_repo, player_repo):
        """Pool mode with leverage calculates correct proportional payout."""
        # Setup two players
        player_repo.add(
            discord_id=4002,
            discord_username="Player1",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1500,
            preferred_roles=["1"],
        )
        player_repo.update_balance(4002, TEST_GUILD_ID, 100)

        player_repo.add(
            discord_id=4003,
            discord_username="Player2",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1500,
            preferred_roles=["1"],
        )
        player_repo.update_balance(4003, TEST_GUILD_ID, 100)

        now_ts = int(time.time())

        # Player1: 10 at 3x leverage = 30 effective on radiant
        bet_repo.place_bet_atomic(
            guild_id=TEST_GUILD_ID,
            discord_id=4002,
            team="radiant",
            amount=10,
            bet_time=now_ts,
            since_ts=now_ts - 1,
            leverage=3,
            max_debt=500,
        )

        # Player2: 10 at 1x = 10 effective on dire
        bet_repo.place_bet_atomic(
            guild_id=TEST_GUILD_ID,
            discord_id=4003,
            team="dire",
            amount=10,
            bet_time=now_ts,
            since_ts=now_ts - 1,
            leverage=1,
            max_debt=500,
        )

        # Total pool: 30 + 10 = 40
        # Radiant wins, winner pool = 30
        # Player1 gets: (30/30) * 40 = 40
        distributions = bet_repo.settle_pending_bets_atomic(
            match_id=102,
            guild_id=TEST_GUILD_ID,
            since_ts=now_ts - 1,
            winning_team="radiant",
            house_payout_multiplier=1.0,
            betting_mode="pool",
        )

        assert len(distributions["winners"]) == 1
        assert distributions["winners"][0]["payout"] == 40
        assert distributions["winners"][0]["discord_id"] == 4002

        # Player1 balance: 100 - 30 + 40 = 110
        assert player_repo.get_balance(4002, TEST_GUILD_ID) == 110

        # Player2 balance: 100 - 10 = 90 (lost)
        assert player_repo.get_balance(4003, TEST_GUILD_ID) == 90

    def test_leverage_pool_mode_multiple_winners(self, bet_repo, player_repo):
        """Pool mode splits proportionally among multiple winners with different leverage."""
        # Setup three players
        for _i, (pid, name) in enumerate([(5001, "Low"), (5002, "High"), (5003, "Loser")]):
            player_repo.add(
                discord_id=pid,
                discord_username=name,
                guild_id=TEST_GUILD_ID,
                initial_mmr=1500,
                preferred_roles=["1"],
            )
            player_repo.update_balance(pid, TEST_GUILD_ID, 100)

        now_ts = int(time.time())

        # Player 5001: 10 at 1x = 10 effective on radiant
        bet_repo.place_bet_atomic(
            guild_id=TEST_GUILD_ID,
            discord_id=5001,
            team="radiant",
            amount=10,
            bet_time=now_ts,
            since_ts=now_ts - 1,
            leverage=1,
            max_debt=500,
        )

        # Player 5002: 10 at 2x = 20 effective on radiant
        bet_repo.place_bet_atomic(
            guild_id=TEST_GUILD_ID,
            discord_id=5002,
            team="radiant",
            amount=10,
            bet_time=now_ts,
            since_ts=now_ts - 1,
            leverage=2,
            max_debt=500,
        )

        # Player 5003: 30 at 1x = 30 effective on dire
        bet_repo.place_bet_atomic(
            guild_id=TEST_GUILD_ID,
            discord_id=5003,
            team="dire",
            amount=30,
            bet_time=now_ts,
            since_ts=now_ts - 1,
            leverage=1,
            max_debt=500,
        )

        # Total pool: 10 + 20 + 30 = 60
        # Radiant wins, winner pool = 10 + 20 = 30
        # Player 5001 gets: (10/30) * 60 = 20
        # Player 5002 gets: (20/30) * 60 = 40
        distributions = bet_repo.settle_pending_bets_atomic(
            match_id=103,
            guild_id=TEST_GUILD_ID,
            since_ts=now_ts - 1,
            winning_team="radiant",
            house_payout_multiplier=1.0,
            betting_mode="pool",
        )

        assert len(distributions["winners"]) == 2
        payouts = {w["discord_id"]: w["payout"] for w in distributions["winners"]}
        assert payouts[5001] == 20  # 1x leverage: (10/30) * 60 = 20
        assert payouts[5002] == 40  # 2x leverage: (20/30) * 60 = 40

        # Player 5001: 100 - 10 + 20 = 110
        assert player_repo.get_balance(5001, TEST_GUILD_ID) == 110
        # Player 5002: 100 - 20 + 40 = 120
        assert player_repo.get_balance(5002, TEST_GUILD_ID) == 120
        # Player 5003: 100 - 30 = 70 (lost)
        assert player_repo.get_balance(5003, TEST_GUILD_ID) == 70


class TestPayDebt:
    """Tests for debt payment functionality."""

    def test_pay_own_debt(self, player_repo):
        """Player can pay their own debt if they have both debt and positive funds elsewhere."""
        # This doesn't make sense for self-payment since you'd need positive balance
        # But let's test the atomic transfer with two players instead

        # Setup player with positive balance
        player_repo.add(
            discord_id=6001,
            discord_username="Helper",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1500,
            preferred_roles=["1"],
        )
        player_repo.update_balance(6001, TEST_GUILD_ID, 50)

        # Setup player in debt
        player_repo.add(
            discord_id=6002,
            discord_username="Debtor",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1500,
            preferred_roles=["1"],
        )
        player_repo.update_balance(6002, TEST_GUILD_ID, -30)

        # Helper pays debtor's debt
        result = player_repo.pay_debt_atomic(
            from_discord_id=6001,
            to_discord_id=6002,
            guild_id=TEST_GUILD_ID,
            amount=20,
        )

        assert result["amount_paid"] == 20
        assert result["from_new_balance"] == 30  # 50 - 20
        assert result["to_new_balance"] == -10  # -30 + 20

        assert player_repo.get_balance(6001, TEST_GUILD_ID) == 30
        assert player_repo.get_balance(6002, TEST_GUILD_ID) == -10

    def test_pay_debt_caps_at_debt_amount(self, player_repo):
        """Payment is capped at the debt amount (can't overpay)."""
        player_repo.add(
            discord_id=6003,
            discord_username="Rich",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1500,
            preferred_roles=["1"],
        )
        player_repo.update_balance(6003, TEST_GUILD_ID, 100)

        player_repo.add(
            discord_id=6004,
            discord_username="SmallDebt",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1500,
            preferred_roles=["1"],
        )
        player_repo.update_balance(6004, TEST_GUILD_ID, -10)

        # Try to pay 50, but only 10 debt exists
        result = player_repo.pay_debt_atomic(
            from_discord_id=6003,
            to_discord_id=6004,
            guild_id=TEST_GUILD_ID,
            amount=50,
        )

        assert result["amount_paid"] == 10  # Capped at debt
        assert result["from_new_balance"] == 90
        assert result["to_new_balance"] == 0  # Debt fully paid

    def test_pay_debt_fails_no_debt(self, player_repo):
        """Cannot pay debt if recipient has no debt."""
        player_repo.add(
            discord_id=6005,
            discord_username="Payer",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1500,
            preferred_roles=["1"],
        )
        player_repo.update_balance(6005, TEST_GUILD_ID, 100)

        player_repo.add(
            discord_id=6006,
            discord_username="NoDebt",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1500,
            preferred_roles=["1"],
        )
        player_repo.update_balance(6006, TEST_GUILD_ID, 50)

        with pytest.raises(ValueError, match="no debt"):
            player_repo.pay_debt_atomic(
                from_discord_id=6005,
                to_discord_id=6006,
                guild_id=TEST_GUILD_ID,
                amount=10,
            )

    def test_pay_debt_fails_insufficient_funds(self, player_repo):
        """Cannot pay more than your balance."""
        player_repo.add(
            discord_id=6007,
            discord_username="Poor",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1500,
            preferred_roles=["1"],
        )
        player_repo.update_balance(6007, TEST_GUILD_ID, 5)

        player_repo.add(
            discord_id=6008,
            discord_username="InDebt",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1500,
            preferred_roles=["1"],
        )
        player_repo.update_balance(6008, TEST_GUILD_ID, -100)

        with pytest.raises(ValueError, match="Insufficient"):
            player_repo.pay_debt_atomic(
                from_discord_id=6007,
                to_discord_id=6008,
                guild_id=TEST_GUILD_ID,
                amount=10,
            )
