"""Tests for charity service (reduced blind rate for /paydebt contributors)."""

import os
import tempfile
import time

import pytest

from database import Database
from repositories.bet_repository import BetRepository
from repositories.charity_repository import CharityRepository
from repositories.player_repository import PlayerRepository
from services.betting_service import BettingService
from services.charity_service import CharityService


@pytest.fixture
def charity_services():
    """Create test services with charity tracking."""
    fd, db_path = tempfile.mkstemp(suffix=".db")
    os.close(fd)

    Database(db_path)
    player_repo = PlayerRepository(db_path)
    bet_repo = BetRepository(db_path)
    charity_repo = CharityRepository(db_path)
    charity_service = CharityService(charity_repo)
    betting_service = BettingService(
        bet_repo,
        player_repo,
        charity_service=charity_service,
    )

    yield {
        "player_repo": player_repo,
        "bet_repo": bet_repo,
        "charity_repo": charity_repo,
        "charity_service": charity_service,
        "betting_service": betting_service,
        "db_path": db_path,
    }

    try:
        os.unlink(db_path)
    except OSError:
        pass


class TestCharityQualification:
    """Tests for charity qualification logic."""

    def test_charity_qualifies_clear_debt(self, charity_services):
        """Clearing target's full debt qualifies for charity bonus."""
        charity_service = charity_services["charity_service"]

        result = charity_service.check_paydebt_qualifies(
            from_id=1001,
            to_id=2001,
            amount_paid=50,
            target_debt_before=50,  # Debt was exactly 50, now cleared
            target_games_played=5,
        )

        assert result["qualifies"] is True
        assert result["reason"] is None

    def test_charity_qualifies_100_contribution(self, charity_services):
        """Contributing 100+ on large debt qualifies for charity bonus."""
        charity_service = charity_services["charity_service"]

        result = charity_service.check_paydebt_qualifies(
            from_id=1001,
            to_id=2001,
            amount_paid=100,
            target_debt_before=200,  # Debt still has 100 remaining
            target_games_played=10,
        )

        assert result["qualifies"] is True
        assert result["reason"] is None

    def test_charity_doesnt_qualify_small_amount(self, charity_services):
        """Paying less than threshold doesn't qualify."""
        charity_service = charity_services["charity_service"]

        result = charity_service.check_paydebt_qualifies(
            from_id=1001,
            to_id=2001,
            amount_paid=80,  # Less than 100
            target_debt_before=200,  # Debt still large
            target_games_played=10,
        )

        assert result["qualifies"] is False
        assert "80" in result["reason"]  # Should mention the amount paid
        assert "100" in result["reason"]  # Should mention the threshold

    def test_charity_min_target_debt(self, charity_services):
        """Target must have at least 50 debt to qualify."""
        charity_service = charity_services["charity_service"]

        result = charity_service.check_paydebt_qualifies(
            from_id=1001,
            to_id=2001,
            amount_paid=20,
            target_debt_before=20,  # Below 50 threshold
            target_games_played=10,
        )

        assert result["qualifies"] is False
        assert "below minimum" in result["reason"].lower()

    def test_charity_min_target_games(self, charity_services):
        """Target must have played at least 3 games to qualify."""
        charity_service = charity_services["charity_service"]

        result = charity_service.check_paydebt_qualifies(
            from_id=1001,
            to_id=2001,
            amount_paid=50,
            target_debt_before=50,
            target_games_played=2,  # Below 3 game minimum
        )

        assert result["qualifies"] is False
        assert "games" in result["reason"].lower()


class TestCharityRewards:
    """Tests for granting and using charity rewards."""

    def test_grant_charity_reward(self, charity_services):
        """Granting charity reward sets games remaining."""
        charity_service = charity_services["charity_service"]

        # Initially no reduced rate
        assert charity_service.has_reduced_rate(1001) is False
        assert charity_service.get_state(1001).reduced_rate_games_remaining == 0

        # Grant reward
        charity_service.grant_charity_reward(discord_id=1001, amount=100)

        # Now has reduced rate for 2 games
        assert charity_service.has_reduced_rate(1001) is True
        state = charity_service.get_state(1001)
        assert state.reduced_rate_games_remaining == 2
        assert state.total_charity_given == 100

    def test_charity_no_stacking(self, charity_services):
        """Charity games don't stack beyond the max (2)."""
        charity_service = charity_services["charity_service"]

        # Grant first reward
        charity_service.grant_charity_reward(discord_id=1001, amount=100)
        assert charity_service.get_state(1001).reduced_rate_games_remaining == 2

        # Grant second reward - should NOT increase beyond 2
        charity_service.grant_charity_reward(discord_id=1001, amount=200)
        assert charity_service.get_state(1001).reduced_rate_games_remaining == 2

        # But total charity given should accumulate
        assert charity_service.get_state(1001).total_charity_given == 300

    def test_games_remaining_decrements(self, charity_services):
        """Games remaining decrements after each blind bet."""
        charity_service = charity_services["charity_service"]

        charity_service.grant_charity_reward(discord_id=1001, amount=100)
        assert charity_service.get_state(1001).reduced_rate_games_remaining == 2

        # Decrement once
        remaining = charity_service.on_blind_bet_created(1001)
        assert remaining == 1
        assert charity_service.get_state(1001).reduced_rate_games_remaining == 1

        # Decrement again
        remaining = charity_service.on_blind_bet_created(1001)
        assert remaining == 0
        assert charity_service.get_state(1001).reduced_rate_games_remaining == 0

        # Should no longer have reduced rate
        assert charity_service.has_reduced_rate(1001) is False

    def test_get_blind_rate_for_player(self, charity_services):
        """Players with charity get reduced blind rate."""
        charity_service = charity_services["charity_service"]

        # Default rate (no charity)
        from config import AUTO_BLIND_PERCENTAGE, CHARITY_REDUCED_RATE

        rate = charity_service.get_blind_rate_for_player(1001)
        assert rate == AUTO_BLIND_PERCENTAGE  # 5%

        # Grant charity
        charity_service.grant_charity_reward(discord_id=1001, amount=100)

        # Now gets reduced rate
        rate = charity_service.get_blind_rate_for_player(1001)
        assert rate == CHARITY_REDUCED_RATE  # 1%


class TestBlindBetsWithCharity:
    """Tests for blind bet creation with charity rate."""

    def test_blind_bet_uses_reduced_rate(self, charity_services):
        """Blind bets use reduced rate for charitable players."""
        player_repo = charity_services["player_repo"]
        charity_service = charity_services["charity_service"]
        betting_service = charity_services["betting_service"]

        # Create players
        player_ids = list(range(20000, 20010))
        for pid in player_ids:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                dotabuff_url=f"https://dotabuff.com/players/{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )
            player_repo.add_balance(pid, 97)  # 100 total

        # Give first player charity bonus
        charitable_player = 20000
        charity_service.grant_charity_reward(discord_id=charitable_player, amount=100)

        shuffle_ts = int(time.time())
        result = betting_service.create_auto_blind_bets(
            guild_id=1,
            radiant_ids=player_ids[:5],
            dire_ids=player_ids[5:],
            shuffle_timestamp=shuffle_ts,
        )

        assert result["created"] == 10

        # Find the charitable player's bet
        charitable_bet = next(
            (b for b in result["bets"] if b["discord_id"] == charitable_player), None
        )
        assert charitable_bet is not None
        # 1% of 100 = 1 jopacoin (reduced rate)
        assert charitable_bet["amount"] == 1
        assert charitable_bet["is_reduced_rate"] is True

        # Other players should have normal rate (5% of 100 = 5)
        other_bets = [b for b in result["bets"] if b["discord_id"] != charitable_player]
        for bet in other_bets:
            assert bet["amount"] == 5
            assert bet["is_reduced_rate"] is False

    def test_blind_bet_decrements_charity_games(self, charity_services):
        """Creating a blind bet decrements charity games remaining."""
        player_repo = charity_services["player_repo"]
        charity_service = charity_services["charity_service"]
        betting_service = charity_services["betting_service"]

        player_id = 21000
        player_repo.add(
            discord_id=player_id,
            discord_username="CharitablePlayer",
            dotabuff_url=f"https://dotabuff.com/players/{player_id}",
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
        player_repo.add_balance(player_id, 97)  # 100 total

        # Grant charity (2 games)
        charity_service.grant_charity_reward(discord_id=player_id, amount=100)
        assert charity_service.get_state(player_id).reduced_rate_games_remaining == 2

        # First blind bet
        shuffle_ts = int(time.time())
        betting_service.create_auto_blind_bets(
            guild_id=1,
            radiant_ids=[player_id],
            dire_ids=[],
            shuffle_timestamp=shuffle_ts,
        )

        # Should have decremented
        assert charity_service.get_state(player_id).reduced_rate_games_remaining == 1

    def test_is_reduced_rate_stored_in_db(self, charity_services):
        """is_reduced_rate flag is stored in the bet record."""
        player_repo = charity_services["player_repo"]
        bet_repo = charity_services["bet_repo"]
        charity_service = charity_services["charity_service"]
        betting_service = charity_services["betting_service"]

        player_id = 22000
        player_repo.add(
            discord_id=player_id,
            discord_username="CharitablePlayer",
            dotabuff_url=f"https://dotabuff.com/players/{player_id}",
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
        player_repo.add_balance(player_id, 97)

        # Grant charity
        charity_service.grant_charity_reward(discord_id=player_id, amount=100)

        shuffle_ts = int(time.time())
        betting_service.create_auto_blind_bets(
            guild_id=1,
            radiant_ids=[player_id],
            dire_ids=[],
            shuffle_timestamp=shuffle_ts,
        )

        # Check the bet in the database
        bets = bet_repo.get_player_pending_bets(1, player_id, since_ts=shuffle_ts)
        assert len(bets) == 1
        assert bets[0]["is_blind"] == 1
        assert bets[0]["is_reduced_rate"] == 1


class TestPayDebtReturnsInfo:
    """Tests for pay_debt_atomic returning charity-relevant info."""

    def test_pay_debt_returns_target_info(self, charity_services):
        """pay_debt_atomic returns target's debt and games played."""
        player_repo = charity_services["player_repo"]

        # Create payer with balance
        player_repo.add(
            discord_id=30000,
            discord_username="Payer",
            dotabuff_url="https://dotabuff.com/players/30000",
        )
        player_repo.add_balance(30000, 97)  # 100 total

        # Create debtor with games played
        player_repo.add(
            discord_id=30001,
            discord_username="Debtor",
            dotabuff_url="https://dotabuff.com/players/30001",
        )
        # Put them in debt
        player_repo.add_balance(30001, -53)  # 3 - 53 = -50
        # Add some wins/losses for games played
        with player_repo.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "UPDATE players SET wins = 3, losses = 2 WHERE discord_id = ?",
                (30001,),
            )

        result = player_repo.pay_debt_atomic(
            from_discord_id=30000,
            to_discord_id=30001,
            amount=50,
        )

        assert result["amount_paid"] == 50
        assert result["target_debt_before"] == 50
        assert result["target_games_played"] == 5
