"""
Tests for the loan system.
"""

import os
import tempfile
import time

import pytest

from database import Database
from repositories.player_repository import PlayerRepository
from services.loan_service import LoanRepository, LoanService


@pytest.fixture
def db_and_repos():
    """Create test database and repositories."""
    fd, db_path = tempfile.mkstemp(suffix=".db")
    os.close(fd)

    db = Database(db_path)
    player_repo = PlayerRepository(db_path)
    loan_repo = LoanRepository(db_path)

    yield {
        "db": db,
        "player_repo": player_repo,
        "loan_repo": loan_repo,
        "db_path": db_path,
    }

    try:
        time.sleep(0.1)  # Windows file locking
        os.unlink(db_path)
    except OSError:
        pass


@pytest.fixture
def loan_service(db_and_repos):
    """Create loan service with test settings."""
    return LoanService(
        loan_repo=db_and_repos["loan_repo"],
        player_repo=db_and_repos["player_repo"],
        cooldown_seconds=259200,  # 3 days
        max_amount=100,
        fee_rate=0.20,
        max_debt=500,
    )


def create_test_player(player_repo, discord_id, balance=3):
    """Helper to create a test player with specified balance."""
    player_repo.add(
        discord_id=discord_id,
        discord_username=f"Player{discord_id}",
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    if balance != 3:  # default is 3
        player_repo.update_balance(discord_id, balance)
    return discord_id


class TestLoanEligibility:
    """Tests for loan eligibility checks."""

    def test_can_take_loan_with_positive_balance(self, db_and_repos, loan_service):
        """Players with positive balance can take loans."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 1001, balance=50)

        result = loan_service.can_take_loan(pid, 50)
        assert result["allowed"] is True
        assert result["amount"] == 50
        assert result["fee"] == 10  # 20% of 50
        assert result["total_owed"] == 60

    def test_cannot_take_loan_exceeding_max(self, db_and_repos, loan_service):
        """Cannot borrow more than max loan amount."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 1002, balance=100)

        result = loan_service.can_take_loan(pid, 150)
        assert result["allowed"] is False
        assert result["reason"] == "exceeds_max"
        assert result["max_amount"] == 100

    def test_cannot_take_loan_exceeding_debt_limit(self, db_and_repos, loan_service):
        """Cannot take loan if it would exceed max debt."""
        player_repo = db_and_repos["player_repo"]
        # Balance -490, loan 100 with 20% fee = 120 owed
        # Net change: +100 - 120 = -20
        # New balance: -490 + (-20) = -510, which exceeds -500 max debt
        pid = create_test_player(player_repo, 1003, balance=-490)

        result = loan_service.can_take_loan(pid, 100)
        assert result["allowed"] is False
        assert result["reason"] == "exceeds_debt_limit"

    def test_cannot_take_invalid_amount(self, db_and_repos, loan_service):
        """Cannot take loan with zero or negative amount."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 1004, balance=50)

        result = loan_service.can_take_loan(pid, 0)
        assert result["allowed"] is False
        assert result["reason"] == "invalid_amount"

        result = loan_service.can_take_loan(pid, -10)
        assert result["allowed"] is False
        assert result["reason"] == "invalid_amount"


class TestLoanCooldown:
    """Tests for loan cooldown."""

    def test_cannot_take_loan_on_cooldown(self, db_and_repos, loan_service):
        """Players on cooldown cannot take loans."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 2001, balance=100)

        # Take first loan
        result = loan_service.take_loan(pid, 50)
        assert result["success"] is True

        # Try to take another immediately
        result = loan_service.can_take_loan(pid, 50)
        assert result["allowed"] is False
        assert result["reason"] == "on_cooldown"
        assert result["cooldown_ends_at"] is not None

    def test_cooldown_expires(self, db_and_repos):
        """Cooldown expires after configured time."""
        player_repo = db_and_repos["player_repo"]
        loan_repo = db_and_repos["loan_repo"]

        # Create service with 1-second cooldown for testing
        loan_service = LoanService(
            loan_repo=loan_repo,
            player_repo=player_repo,
            cooldown_seconds=1,
            max_amount=100,
            fee_rate=0.20,
            max_debt=500,
        )

        pid = create_test_player(player_repo, 2002, balance=100)

        # Take first loan
        result = loan_service.take_loan(pid, 20)
        assert result["success"] is True

        # Wait for cooldown to expire
        time.sleep(1.1)

        # Should be able to take another loan
        result = loan_service.can_take_loan(pid, 20)
        assert result["allowed"] is True


class TestLoanExecution:
    """Tests for loan execution."""

    def test_take_loan_success(self, db_and_repos, loan_service):
        """Successfully taking a loan updates balance correctly."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 3001, balance=50)

        result = loan_service.take_loan(pid, 100, guild_id=12345)

        assert result["success"] is True
        assert result["amount"] == 100
        assert result["fee"] == 20  # 20% of 100
        assert result["total_owed"] == 120

        # Balance should be: 50 + 100 - 120 = 30
        new_balance = player_repo.get_balance(pid)
        assert new_balance == 30

    def test_take_loan_into_debt(self, db_and_repos, loan_service):
        """Taking a loan can put player into debt."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 3002, balance=10)

        result = loan_service.take_loan(pid, 100)

        assert result["success"] is True
        # Balance: 10 + 100 - 120 = -10
        new_balance = player_repo.get_balance(pid)
        assert new_balance == -10

    def test_take_loan_updates_state(self, db_and_repos, loan_service):
        """Taking a loan updates loan state tracking."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 3003, balance=100)

        # Take first loan
        result = loan_service.take_loan(pid, 50)
        assert result["success"] is True
        assert result["total_loans_taken"] == 1

        state = loan_service.get_state(pid)
        assert state.total_loans_taken == 1
        assert state.total_fees_paid == 10  # 20% of 50
        assert state.last_loan_at is not None


class TestNonprofitFund:
    """Tests for the nonprofit fund."""

    def test_loan_fee_goes_to_nonprofit(self, db_and_repos, loan_service):
        """Loan fees are added to nonprofit fund."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 4001, balance=100)

        # Initially empty
        assert loan_service.get_nonprofit_fund(guild_id=99) == 0

        # Take a loan
        result = loan_service.take_loan(pid, 100, guild_id=99)
        assert result["fee"] == 20

        # Nonprofit fund should have the fee
        assert loan_service.get_nonprofit_fund(guild_id=99) == 20

    def test_nonprofit_accumulates(self, db_and_repos):
        """Multiple loans accumulate in nonprofit fund."""
        player_repo = db_and_repos["player_repo"]
        loan_repo = db_and_repos["loan_repo"]

        # Create service with no cooldown for testing
        loan_service = LoanService(
            loan_repo=loan_repo,
            player_repo=player_repo,
            cooldown_seconds=0,
            max_amount=100,
            fee_rate=0.20,
            max_debt=500,
        )

        pid1 = create_test_player(player_repo, 4002, balance=100)
        pid2 = create_test_player(player_repo, 4003, balance=100)

        # Both take loans
        loan_service.take_loan(pid1, 50, guild_id=88)  # Fee: 10
        loan_service.take_loan(pid2, 100, guild_id=88)  # Fee: 20

        # Total should be 30
        assert loan_service.get_nonprofit_fund(guild_id=88) == 30


class TestLowestBalanceTracking:
    """Tests for lowest balance tracking."""

    def test_lowest_balance_tracked_on_loan(self, db_and_repos, loan_service):
        """Taking a loan that goes into debt tracks lowest balance."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 5001, balance=10)

        # Take loan: 10 + 100 - 120 = -10
        loan_service.take_loan(pid, 100)

        lowest = player_repo.get_lowest_balance(pid)
        assert lowest == -10

    def test_lowest_balance_updates_correctly(self, db_and_repos, loan_service):
        """Lowest balance only updates when going lower."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 5002, balance=-50)

        # Update balance to -100 (lower)
        player_repo.add_balance(pid, -50)
        lowest = player_repo.get_lowest_balance(pid)
        assert lowest == -100

        # Update balance to -80 (not lower)
        player_repo.add_balance(pid, 20)
        lowest = player_repo.get_lowest_balance(pid)
        assert lowest == -100  # Should still be -100


class TestLoanState:
    """Tests for loan state retrieval."""

    def test_get_state_no_loans(self, db_and_repos, loan_service):
        """Getting state for player with no loans returns defaults."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 6001)

        state = loan_service.get_state(pid)
        assert state.discord_id == pid
        assert state.total_loans_taken == 0
        assert state.total_fees_paid == 0
        assert state.is_on_cooldown is False
        assert state.last_loan_at is None

    def test_get_state_with_loan(self, db_and_repos, loan_service):
        """Getting state after loan shows correct values."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 6002, balance=100)

        loan_service.take_loan(pid, 50)

        state = loan_service.get_state(pid)
        assert state.total_loans_taken == 1
        assert state.total_fees_paid == 10
        assert state.is_on_cooldown is True
        assert state.last_loan_at is not None
