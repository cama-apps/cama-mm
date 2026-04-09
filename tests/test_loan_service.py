"""
Tests for the loan system with deferred repayment.

Loans work as follows:
1. Player takes loan -> receives full amount, outstanding debt tracked
2. Player plays in a match -> loan is repaid (deducted from balance)
3. Fee goes to nonprofit fund at repayment time
"""

import time
from unittest.mock import patch

import pytest

from database import Database
from repositories.loan_repository import LoanRepository
from repositories.player_repository import PlayerRepository
from services import error_codes
from services.loan_service import LoanApproval, LoanResult, LoanService, RepaymentResult
from tests.conftest import TEST_GUILD_ID


@pytest.fixture
def db_and_repos(repo_db_path):
    """Create test database and repositories using centralized fast fixture."""
    db = Database(repo_db_path)
    player_repo = PlayerRepository(repo_db_path)
    loan_repo = LoanRepository(repo_db_path)

    yield {
        "db": db,
        "player_repo": player_repo,
        "loan_repo": loan_repo,
        "db_path": repo_db_path,
    }


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


def create_test_player(player_repo, discord_id, balance=3, guild_id=TEST_GUILD_ID):
    """Helper to create a test player with specified balance."""
    player_repo.add(
        discord_id=discord_id,
        discord_username=f"Player{discord_id}",
        guild_id=guild_id,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    if balance != 3:  # default is 3
        player_repo.update_balance(discord_id, guild_id, balance)
    return discord_id


class TestLoanEligibility:
    """Tests for loan eligibility checks."""

    def test_can_take_loan_with_positive_balance(self, db_and_repos, loan_service):
        """Players with positive balance can take loans."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 1001, balance=50)

        result = loan_service.validate_loan(pid, 50, TEST_GUILD_ID)
        assert result.success
        assert result.value.amount == 50
        assert result.value.fee == 10  # 20% of 50
        assert result.value.total_owed == 60
        # With deferred repayment, new_balance is current + amount (not minus fee)
        assert result.value.new_balance == 100  # 50 + 50

    def test_cannot_take_loan_exceeding_max(self, db_and_repos, loan_service):
        """Cannot borrow more than max loan amount."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 1002, balance=100)

        result = loan_service.validate_loan(pid, 150, TEST_GUILD_ID)
        assert result.error
        assert result.error_code == "loan_amount_exceeded"

    def test_cannot_take_loan_with_outstanding_loan(self, db_and_repos, loan_service):
        """Cannot take another loan while one is outstanding."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 1003, balance=100)

        # Take first loan
        result = loan_service.execute_loan(pid, 50, TEST_GUILD_ID)
        assert result.success

        # Try to take another - should fail because outstanding loan exists
        result = loan_service.validate_loan(pid, 50, TEST_GUILD_ID)
        assert result.error
        assert result.error_code == "loan_already_exists"

    def test_cannot_take_invalid_amount(self, db_and_repos, loan_service):
        """Cannot take loan with zero or negative amount."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 1004, balance=50)

        result = loan_service.validate_loan(pid, 0, TEST_GUILD_ID)
        assert result.error
        assert result.error_code == "validation_error"

        result = loan_service.validate_loan(pid, -10, TEST_GUILD_ID)
        assert result.error
        assert result.error_code == "validation_error"


class TestLoanCooldown:
    """Tests for loan cooldown."""

    def test_cooldown_checked_after_repayment(self, db_and_repos):
        """Players on cooldown cannot take loans after repaying."""
        player_repo = db_and_repos["player_repo"]
        loan_repo = db_and_repos["loan_repo"]

        loan_service = LoanService(
            loan_repo=loan_repo,
            player_repo=player_repo,
            cooldown_seconds=259200,  # 3 days
            max_amount=100,
            fee_rate=0.20,
            max_debt=500,
        )

        pid = create_test_player(player_repo, 2001, balance=200)

        # Take first loan
        result = loan_service.execute_loan(pid, 50, TEST_GUILD_ID)
        assert result.success

        # Repay the loan
        repay_result = loan_service.execute_repayment(pid, guild_id=TEST_GUILD_ID)
        assert repay_result.success

        # Now try to take another - should be on cooldown
        result = loan_service.validate_loan(pid, 50, TEST_GUILD_ID)
        assert result.error
        assert result.error_code == "cooldown_active"

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

        pid = create_test_player(player_repo, 2002, balance=200)

        # Take first loan and repay
        now = time.time()
        with patch("services.loan_service.time") as mock_time:
            mock_time.time.return_value = now
            loan_service.execute_loan(pid, 20, TEST_GUILD_ID)
            loan_service.execute_repayment(pid, TEST_GUILD_ID)

            # Simulate cooldown expiration (advance 2 seconds past 1-second cooldown)
            mock_time.time.return_value = now + 2
            result = loan_service.validate_loan(pid, 20, TEST_GUILD_ID)
        assert result.success


class TestLoanExecution:
    """Tests for loan execution with deferred repayment."""

    def test_take_loan_credits_full_amount(self, db_and_repos, loan_service):
        """Taking a loan credits the full amount (deferred repayment)."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 3001, balance=50)

        result = loan_service.execute_loan(pid, 100, guild_id=12345)

        assert result.success
        assert result.value.amount == 100
        assert result.value.fee == 20  # 20% of 100
        assert result.value.total_owed == 120

        # Balance should be: 50 + 100 = 150 (NO deduction yet)
        new_balance = player_repo.get_balance(pid, TEST_GUILD_ID)
        assert new_balance == 150

    def test_take_loan_tracks_outstanding(self, db_and_repos, loan_service):
        """Taking a loan tracks the outstanding amount."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 3002, balance=50)

        loan_service.execute_loan(pid, 100, TEST_GUILD_ID)

        state = loan_service.get_state(pid, TEST_GUILD_ID)
        assert state.outstanding_principal == 100
        assert state.outstanding_fee == 20
        assert state.outstanding_total == 120
        assert state.has_outstanding_loan is True

    def test_take_loan_updates_state(self, db_and_repos, loan_service):
        """Taking a loan updates loan state tracking."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 3003, balance=100)

        result = loan_service.execute_loan(pid, 50, TEST_GUILD_ID)
        assert result.success
        assert result.value.total_loans_taken == 1

        state = loan_service.get_state(pid, TEST_GUILD_ID)
        assert state.total_loans_taken == 1
        # Fee is NOT paid yet - deferred until repayment
        assert state.total_fees_paid == 0
        assert state.last_loan_at is not None


class TestLoanRepayment:
    """Tests for loan repayment."""

    def test_repay_loan_deducts_full_amount(self, db_and_repos, loan_service):
        """Repaying a loan deducts principal + fee."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 4001, balance=50)

        # Take loan: balance becomes 150
        loan_service.execute_loan(pid, 100, TEST_GUILD_ID)
        assert player_repo.get_balance(pid, TEST_GUILD_ID) == 150

        # Repay loan: balance -= 120 (100 + 20 fee)
        result = loan_service.execute_repayment(pid, guild_id=TEST_GUILD_ID)

        assert result.success
        assert result.value.principal == 100
        assert result.value.fee == 20
        assert result.value.total_repaid == 120
        assert result.value.balance_before == 150
        assert result.value.new_balance == 30

        # Verify balance
        assert player_repo.get_balance(pid, TEST_GUILD_ID) == 30

    def test_repay_loan_clears_outstanding(self, db_and_repos, loan_service):
        """Repaying a loan clears the outstanding amounts."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 4002, balance=200)

        loan_service.execute_loan(pid, 50, TEST_GUILD_ID)
        assert loan_service.get_state(pid, TEST_GUILD_ID).has_outstanding_loan is True

        loan_service.execute_repayment(pid, TEST_GUILD_ID)

        state = loan_service.get_state(pid, TEST_GUILD_ID)
        assert state.outstanding_principal == 0
        assert state.outstanding_fee == 0
        assert state.has_outstanding_loan is False

    def test_repay_loan_adds_fee_to_nonprofit(self, db_and_repos, loan_service):
        """Repaying a loan adds fee to nonprofit fund."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 4003, balance=200)

        # Initially empty
        assert loan_service.get_nonprofit_fund(guild_id=TEST_GUILD_ID) == 0

        # Take and repay loan
        loan_service.execute_loan(pid, 100, guild_id=TEST_GUILD_ID)
        # Fee not in nonprofit yet
        assert loan_service.get_nonprofit_fund(guild_id=TEST_GUILD_ID) == 0

        loan_service.execute_repayment(pid, guild_id=TEST_GUILD_ID)
        # Now fee is in nonprofit
        assert loan_service.get_nonprofit_fund(guild_id=TEST_GUILD_ID) == 20

    def test_repay_loan_updates_fees_paid(self, db_and_repos, loan_service):
        """Repaying a loan updates total_fees_paid in state."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 4004, balance=200)

        loan_service.execute_loan(pid, 50, TEST_GUILD_ID)
        # Fee not counted yet
        assert loan_service.get_state(pid, TEST_GUILD_ID).total_fees_paid == 0

        loan_service.execute_repayment(pid, TEST_GUILD_ID)
        # Now fee is counted
        assert loan_service.get_state(pid, TEST_GUILD_ID).total_fees_paid == 10

    def test_repay_loan_can_push_into_debt(self, db_and_repos, loan_service):
        """Repaying a loan can push player into debt."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 4005, balance=0)

        # Take loan: balance becomes 100
        loan_service.execute_loan(pid, 100, TEST_GUILD_ID)
        assert player_repo.get_balance(pid, TEST_GUILD_ID) == 100

        # Spend all the money (simulate betting loss)
        player_repo.update_balance(pid, TEST_GUILD_ID, 0)

        # Repay loan: 0 - 120 = -120
        result = loan_service.execute_repayment(pid, TEST_GUILD_ID)
        assert result.success
        assert result.value.new_balance == -120

    def test_repay_no_outstanding_loan(self, db_and_repos, loan_service):
        """Repaying with no outstanding loan returns failure."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 4006, balance=100)

        result = loan_service.execute_repayment(pid, TEST_GUILD_ID)
        assert result.error
        assert result.error_code == "no_outstanding_loan"


class TestLoanState:
    """Tests for loan state retrieval."""

    def test_get_state_no_loans(self, db_and_repos, loan_service):
        """Getting state for player with no loans returns defaults."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 6001)

        state = loan_service.get_state(pid, TEST_GUILD_ID)
        assert state.discord_id == pid
        assert state.total_loans_taken == 0
        assert state.total_fees_paid == 0
        assert state.is_on_cooldown is False
        assert state.last_loan_at is None
        assert state.outstanding_principal == 0
        assert state.outstanding_fee == 0
        assert state.has_outstanding_loan is False

    def test_get_state_with_outstanding_loan(self, db_and_repos, loan_service):
        """Getting state after loan shows outstanding amounts."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 6002, balance=100)

        loan_service.execute_loan(pid, 50, TEST_GUILD_ID)

        state = loan_service.get_state(pid, TEST_GUILD_ID)
        assert state.total_loans_taken == 1
        assert state.outstanding_principal == 50
        assert state.outstanding_fee == 10
        assert state.has_outstanding_loan is True
        # Fee not paid yet
        assert state.total_fees_paid == 0

    def test_get_state_after_repayment(self, db_and_repos, loan_service):
        """Getting state after repayment shows cleared outstanding."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 6003, balance=200)

        loan_service.execute_loan(pid, 50, TEST_GUILD_ID)
        loan_service.execute_repayment(pid, TEST_GUILD_ID)

        state = loan_service.get_state(pid, TEST_GUILD_ID)
        assert state.total_loans_taken == 1
        assert state.total_fees_paid == 10  # Fee now counted
        assert state.outstanding_principal == 0
        assert state.outstanding_fee == 0
        assert state.has_outstanding_loan is False
        assert state.is_on_cooldown is True


class TestNegativeLoans:
    """Tests for peak degen behavior: taking loans while already in debt."""

    def test_loan_while_negative_flagged(self, db_and_repos):
        """Taking a loan while negative balance is flagged."""
        player_repo = db_and_repos["player_repo"]
        loan_repo = db_and_repos["loan_repo"]

        loan_service = LoanService(
            loan_repo=loan_repo,
            player_repo=player_repo,
            cooldown_seconds=0,
            max_amount=100,
            fee_rate=0.20,
            max_debt=500,
        )

        # Start with negative balance
        pid = create_test_player(player_repo, 7001, balance=-100)

        result = loan_service.execute_loan(pid, 50, TEST_GUILD_ID)
        assert result.success
        assert result.value.was_negative_loan is True

    def test_loan_while_positive_not_flagged(self, db_and_repos, loan_service):
        """Taking a loan with positive balance is not flagged as negative loan."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 7002, balance=100)

        result = loan_service.execute_loan(pid, 50, TEST_GUILD_ID)
        assert result.success
        assert result.value.was_negative_loan is False

    def test_negative_loans_counted(self, db_and_repos):
        """Negative loans are counted in state."""
        player_repo = db_and_repos["player_repo"]
        loan_repo = db_and_repos["loan_repo"]

        loan_service = LoanService(
            loan_repo=loan_repo,
            player_repo=player_repo,
            cooldown_seconds=0,
            max_amount=100,
            fee_rate=0.20,
            max_debt=500,
        )

        pid = create_test_player(player_repo, 7003, balance=-50)

        # Take loan while negative, then repay to allow second loan
        loan_service.execute_loan(pid, 20, TEST_GUILD_ID)
        loan_service.execute_repayment(pid, TEST_GUILD_ID)

        # Still negative, take another
        player_repo.update_balance(pid, TEST_GUILD_ID, -50)
        loan_service.execute_loan(pid, 20, TEST_GUILD_ID)

        state = loan_service.get_state(pid, TEST_GUILD_ID)
        assert state.negative_loans_taken == 2
        assert state.total_loans_taken == 2


class TestFullLoanCycle:
    """Integration tests for the full loan lifecycle."""

    def test_full_loan_cycle(self, db_and_repos):
        """Test complete loan -> bet -> repay cycle."""
        player_repo = db_and_repos["player_repo"]
        loan_repo = db_and_repos["loan_repo"]

        loan_service = LoanService(
            loan_repo=loan_repo,
            player_repo=player_repo,
            cooldown_seconds=0,
            max_amount=100,
            fee_rate=0.20,
            max_debt=500,
        )

        pid = create_test_player(player_repo, 8001, balance=0)

        # 1. Take loan of 100
        result = loan_service.execute_loan(pid, 100, guild_id=TEST_GUILD_ID)
        assert result.success
        assert player_repo.get_balance(pid, TEST_GUILD_ID) == 100  # Got the money

        # 2. Simulate betting and winning
        player_repo.add_balance(pid, TEST_GUILD_ID, 50)  # Won 50
        assert player_repo.get_balance(pid, TEST_GUILD_ID) == 150

        # 3. Match ends, loan repaid
        repay = loan_service.execute_repayment(pid, guild_id=TEST_GUILD_ID)
        assert repay.success
        assert repay.value.total_repaid == 120  # 100 + 20 fee

        # 4. Final balance: 150 - 120 = 30
        assert player_repo.get_balance(pid, TEST_GUILD_ID) == 30

        # 5. Nonprofit got the fee
        assert loan_service.get_nonprofit_fund(guild_id=TEST_GUILD_ID) == 20

    def test_loan_then_lose_everything(self, db_and_repos):
        """Test loan where player loses all money before repayment."""
        player_repo = db_and_repos["player_repo"]
        loan_repo = db_and_repos["loan_repo"]

        loan_service = LoanService(
            loan_repo=loan_repo,
            player_repo=player_repo,
            cooldown_seconds=0,
            max_amount=100,
            fee_rate=0.20,
            max_debt=500,
        )

        pid = create_test_player(player_repo, 8002, balance=0)

        # Take loan
        loan_service.execute_loan(pid, 100, TEST_GUILD_ID)
        assert player_repo.get_balance(pid, TEST_GUILD_ID) == 100

        # Lose everything (bad bet)
        player_repo.update_balance(pid, TEST_GUILD_ID, 0)

        # Match ends, must repay
        repay = loan_service.execute_repayment(pid, TEST_GUILD_ID)
        assert repay.success

        # Now in debt: 0 - 120 = -120
        assert player_repo.get_balance(pid, TEST_GUILD_ID) == -120


# =============================================================================
# RESULT-RETURNING METHOD TESTS (from test_loan_service_result.py)
# =============================================================================


@pytest.fixture
def result_services(repo_db_path):
    """Create loan service with dependencies using centralized fast fixture."""
    player_repo = PlayerRepository(repo_db_path)
    loan_repo = LoanRepository(repo_db_path)
    loan_service = LoanService(
        loan_repo=loan_repo,
        player_repo=player_repo,
        cooldown_seconds=3600,  # 1 hour
        max_amount=100,
        fee_rate=0.20,
        max_debt=500,
    )
    return {
        "loan_service": loan_service,
        "player_repo": player_repo,
        "loan_repo": loan_repo,
    }


@pytest.fixture
def registered_player(result_services):
    """Create a registered player with starting balance."""
    player_repo = result_services["player_repo"]
    discord_id = 12345
    player_repo.add(
        discord_id=discord_id,
        discord_username="TestPlayer",
        guild_id=TEST_GUILD_ID,
        glicko_rating=1500,
        glicko_rd=350,
    )
    player_repo.update_balance(discord_id, TEST_GUILD_ID, 10)
    return discord_id


class TestValidateLoan:
    """Tests for validate_loan Result method."""

    def test_valid_loan_returns_approval(self, result_services, registered_player):
        """Valid loan request returns LoanApproval."""
        loan_service = result_services["loan_service"]

        result = loan_service.validate_loan(registered_player, 50, TEST_GUILD_ID)

        assert result.success is True
        assert isinstance(result.value, LoanApproval)
        assert result.value.amount == 50
        assert result.value.fee == 10  # 20% of 50
        assert result.value.total_owed == 60
        assert result.value.new_balance == 60  # 10 + 50

    def test_outstanding_loan_fails(self, result_services, registered_player):
        """Can't take loan with outstanding loan."""
        loan_service = result_services["loan_service"]
        loan_repo = result_services["loan_repo"]

        # Create outstanding loan
        loan_repo.upsert_state(
            discord_id=registered_player,
            outstanding_principal=50,
            outstanding_fee=10,
            total_loans_taken=1,
            total_fees_paid=0,
        )

        result = loan_service.validate_loan(registered_player, 25)

        assert result.success is False
        assert result.error_code == error_codes.LOAN_ALREADY_EXISTS
        assert "outstanding loan" in result.error.lower()

    def test_cooldown_fails(self, result_services, registered_player):
        """Can't take loan during cooldown."""
        loan_service = result_services["loan_service"]
        loan_repo = result_services["loan_repo"]

        # Set recent loan time
        recent_time = int(time.time()) - 60  # 1 minute ago
        loan_repo.upsert_state(
            discord_id=registered_player,
            last_loan_at=recent_time,
            total_loans_taken=1,
            total_fees_paid=0,
        )

        result = loan_service.validate_loan(registered_player, 25)

        assert result.success is False
        assert result.error_code == error_codes.COOLDOWN_ACTIVE
        assert "cooldown" in result.error.lower()

    def test_invalid_amount_fails(self, result_services, registered_player):
        """Negative/zero amount fails."""
        loan_service = result_services["loan_service"]

        result = loan_service.validate_loan(registered_player, 0)

        assert result.success is False
        assert result.error_code == error_codes.VALIDATION_ERROR

    def test_exceeds_max_fails(self, result_services, registered_player):
        """Amount over max fails."""
        loan_service = result_services["loan_service"]

        result = loan_service.validate_loan(registered_player, 200)  # max is 100

        assert result.success is False
        assert result.error_code == error_codes.LOAN_AMOUNT_EXCEEDED


class TestExecuteLoanResult:
    """Tests for execute_loan Result method."""

    def test_successful_loan(self, result_services, registered_player):
        """Successful loan returns LoanResult."""
        loan_service = result_services["loan_service"]
        player_repo = result_services["player_repo"]

        initial_balance = player_repo.get_balance(registered_player, TEST_GUILD_ID)

        result = loan_service.execute_loan(registered_player, 50, TEST_GUILD_ID)

        assert result.success is True
        assert isinstance(result.value, LoanResult)
        assert result.value.amount == 50
        assert result.value.fee == 10
        assert result.value.new_balance == initial_balance + 50
        assert result.value.was_negative_loan is False

    def test_loan_updates_balance(self, result_services, registered_player):
        """Loan credits player's balance."""
        loan_service = result_services["loan_service"]
        player_repo = result_services["player_repo"]

        loan_service.execute_loan(registered_player, 50, TEST_GUILD_ID)

        # Balance should be increased by loan amount
        assert player_repo.get_balance(registered_player, TEST_GUILD_ID) == 60  # 10 + 50

    def test_loan_creates_outstanding(self, result_services, registered_player):
        """Loan creates outstanding debt record."""
        loan_service = result_services["loan_service"]

        loan_service.execute_loan(registered_player, 50, TEST_GUILD_ID)

        state = loan_service.get_state(registered_player, TEST_GUILD_ID)
        assert state.has_outstanding_loan is True
        assert state.outstanding_principal == 50
        assert state.outstanding_fee == 10

    def test_negative_loan_tracked(self, result_services, registered_player):
        """Loan while in debt is tracked."""
        loan_service = result_services["loan_service"]
        player_repo = result_services["player_repo"]

        # Put player in debt
        player_repo.add_balance(registered_player, TEST_GUILD_ID, -15)  # -5 balance

        result = loan_service.execute_loan(registered_player, 50, TEST_GUILD_ID)

        assert result.value.was_negative_loan is True
        state = loan_service.get_state(registered_player, TEST_GUILD_ID)
        assert state.negative_loans_taken == 1


class TestExecuteRepaymentResult:
    """Tests for execute_repayment Result method."""

    def test_successful_repayment(self, result_services, registered_player):
        """Successful repayment returns RepaymentResult."""
        loan_service = result_services["loan_service"]

        # Take a loan first
        loan_service.execute_loan(registered_player, 50, TEST_GUILD_ID)

        result = loan_service.execute_repayment(registered_player, TEST_GUILD_ID)

        assert result.success is True
        assert isinstance(result.value, RepaymentResult)
        assert result.value.principal == 50
        assert result.value.fee == 10
        assert result.value.total_repaid == 60

    def test_repayment_clears_outstanding(self, result_services, registered_player):
        """Repayment clears outstanding loan."""
        loan_service = result_services["loan_service"]

        loan_service.execute_loan(registered_player, 50, TEST_GUILD_ID)
        loan_service.execute_repayment(registered_player, TEST_GUILD_ID)

        state = loan_service.get_state(registered_player, TEST_GUILD_ID)
        assert state.has_outstanding_loan is False
        assert state.outstanding_principal == 0
        assert state.outstanding_fee == 0

    def test_repayment_adds_to_nonprofit(self, result_services, registered_player):
        """Repayment fee goes to nonprofit fund."""
        loan_service = result_services["loan_service"]

        loan_service.execute_loan(registered_player, 50, TEST_GUILD_ID)
        result = loan_service.execute_repayment(registered_player, TEST_GUILD_ID)

        # Fee (10) should be in nonprofit fund
        assert result.value.nonprofit_total >= 10

    def test_no_outstanding_loan_fails(self, result_services, registered_player):
        """Can't repay without outstanding loan."""
        loan_service = result_services["loan_service"]

        result = loan_service.execute_repayment(registered_player, TEST_GUILD_ID)

        assert result.success is False
        assert result.error_code == error_codes.NO_OUTSTANDING_LOAN


class TestResultChaining:
    """Test Result API usage patterns."""

    def test_boolean_context(self, result_services, registered_player):
        """Result works in if statements."""
        loan_service = result_services["loan_service"]

        result = loan_service.validate_loan(registered_player, 50)

        if result:
            # Should enter this branch
            assert result.value.amount == 50
        else:
            pytest.fail("Result should be truthy")

    def test_unwrap_on_success(self, result_services, registered_player):
        """unwrap() returns value on success."""
        loan_service = result_services["loan_service"]

        result = loan_service.validate_loan(registered_player, 50)
        approval = result.unwrap()

        assert approval.amount == 50

    def test_unwrap_or_on_failure(self, result_services, registered_player):
        """unwrap_or() returns default on failure."""
        loan_service = result_services["loan_service"]

        result = loan_service.validate_loan(registered_player, -10)  # invalid
        approval = result.unwrap_or(None)

        assert approval is None
