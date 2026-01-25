"""Tests for LoanService Result-returning methods."""

import pytest
import time
import tempfile
import os

from services.loan_service import (
    LoanService,
    LoanRepository,
    LoanApproval,
    LoanResult,
    RepaymentResult,
)
from services.result import Result
from services import error_codes
from repositories.player_repository import PlayerRepository
from infrastructure.schema_manager import SchemaManager


@pytest.fixture
def temp_db():
    """Create a temporary database with schema."""
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    schema_manager = SchemaManager(path)
    schema_manager.initialize()
    yield path
    time.sleep(0.1)  # Windows file locking
    try:
        os.unlink(path)
    except Exception:
        pass


@pytest.fixture
def services(temp_db):
    """Create loan service with dependencies."""
    player_repo = PlayerRepository(temp_db)
    loan_repo = LoanRepository(temp_db)
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
def registered_player(services):
    """Create a registered player with starting balance."""
    player_repo = services["player_repo"]
    discord_id = 12345
    player_repo.add(
        discord_id=discord_id,
        discord_username="TestPlayer",
        glicko_rating=1500,
        glicko_rd=350,
    )
    player_repo.update_balance(discord_id, 10)
    return discord_id


class TestValidateLoan:
    """Tests for validate_loan Result method."""

    def test_valid_loan_returns_approval(self, services, registered_player):
        """Valid loan request returns LoanApproval."""
        loan_service = services["loan_service"]

        result = loan_service.validate_loan(registered_player, 50)

        assert result.success is True
        assert isinstance(result.value, LoanApproval)
        assert result.value.amount == 50
        assert result.value.fee == 10  # 20% of 50
        assert result.value.total_owed == 60
        assert result.value.new_balance == 60  # 10 + 50

    def test_outstanding_loan_fails(self, services, registered_player):
        """Can't take loan with outstanding loan."""
        loan_service = services["loan_service"]
        loan_repo = services["loan_repo"]

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

    def test_cooldown_fails(self, services, registered_player):
        """Can't take loan during cooldown."""
        loan_service = services["loan_service"]
        loan_repo = services["loan_repo"]

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

    def test_invalid_amount_fails(self, services, registered_player):
        """Negative/zero amount fails."""
        loan_service = services["loan_service"]

        result = loan_service.validate_loan(registered_player, 0)

        assert result.success is False
        assert result.error_code == error_codes.VALIDATION_ERROR

    def test_exceeds_max_fails(self, services, registered_player):
        """Amount over max fails."""
        loan_service = services["loan_service"]

        result = loan_service.validate_loan(registered_player, 200)  # max is 100

        assert result.success is False
        assert result.error_code == error_codes.LOAN_AMOUNT_EXCEEDED


class TestExecuteLoan:
    """Tests for execute_loan Result method."""

    def test_successful_loan(self, services, registered_player):
        """Successful loan returns LoanResult."""
        loan_service = services["loan_service"]
        player_repo = services["player_repo"]

        initial_balance = player_repo.get_balance(registered_player)

        result = loan_service.execute_loan(registered_player, 50)

        assert result.success is True
        assert isinstance(result.value, LoanResult)
        assert result.value.amount == 50
        assert result.value.fee == 10
        assert result.value.new_balance == initial_balance + 50
        assert result.value.was_negative_loan is False

    def test_loan_updates_balance(self, services, registered_player):
        """Loan credits player's balance."""
        loan_service = services["loan_service"]
        player_repo = services["player_repo"]

        loan_service.execute_loan(registered_player, 50)

        # Balance should be increased by loan amount
        assert player_repo.get_balance(registered_player) == 60  # 10 + 50

    def test_loan_creates_outstanding(self, services, registered_player):
        """Loan creates outstanding debt record."""
        loan_service = services["loan_service"]

        loan_service.execute_loan(registered_player, 50)

        state = loan_service.get_state(registered_player)
        assert state.has_outstanding_loan is True
        assert state.outstanding_principal == 50
        assert state.outstanding_fee == 10

    def test_negative_loan_tracked(self, services, registered_player):
        """Loan while in debt is tracked."""
        loan_service = services["loan_service"]
        player_repo = services["player_repo"]

        # Put player in debt
        player_repo.add_balance(registered_player, -15)  # -5 balance

        result = loan_service.execute_loan(registered_player, 50)

        assert result.value.was_negative_loan is True
        state = loan_service.get_state(registered_player)
        assert state.negative_loans_taken == 1


class TestExecuteRepayment:
    """Tests for execute_repayment Result method."""

    def test_successful_repayment(self, services, registered_player):
        """Successful repayment returns RepaymentResult."""
        loan_service = services["loan_service"]

        # Take a loan first
        loan_service.execute_loan(registered_player, 50)

        result = loan_service.execute_repayment(registered_player)

        assert result.success is True
        assert isinstance(result.value, RepaymentResult)
        assert result.value.principal == 50
        assert result.value.fee == 10
        assert result.value.total_repaid == 60

    def test_repayment_clears_outstanding(self, services, registered_player):
        """Repayment clears outstanding loan."""
        loan_service = services["loan_service"]

        loan_service.execute_loan(registered_player, 50)
        loan_service.execute_repayment(registered_player)

        state = loan_service.get_state(registered_player)
        assert state.has_outstanding_loan is False
        assert state.outstanding_principal == 0
        assert state.outstanding_fee == 0

    def test_repayment_adds_to_nonprofit(self, services, registered_player):
        """Repayment fee goes to nonprofit fund."""
        loan_service = services["loan_service"]

        loan_service.execute_loan(registered_player, 50)
        result = loan_service.execute_repayment(registered_player)

        # Fee (10) should be in nonprofit fund
        assert result.value.nonprofit_total >= 10

    def test_no_outstanding_loan_fails(self, services, registered_player):
        """Can't repay without outstanding loan."""
        loan_service = services["loan_service"]

        result = loan_service.execute_repayment(registered_player)

        assert result.success is False
        assert result.error_code == error_codes.NO_OUTSTANDING_LOAN


class TestResultChaining:
    """Test Result API usage patterns."""

    def test_boolean_context(self, services, registered_player):
        """Result works in if statements."""
        loan_service = services["loan_service"]

        result = loan_service.validate_loan(registered_player, 50)

        if result:
            # Should enter this branch
            assert result.value.amount == 50
        else:
            pytest.fail("Result should be truthy")

    def test_unwrap_on_success(self, services, registered_player):
        """unwrap() returns value on success."""
        loan_service = services["loan_service"]

        result = loan_service.validate_loan(registered_player, 50)
        approval = result.unwrap()

        assert approval.amount == 50

    def test_unwrap_or_on_failure(self, services, registered_player):
        """unwrap_or() returns default on failure."""
        loan_service = services["loan_service"]

        result = loan_service.validate_loan(registered_player, -10)  # invalid
        approval = result.unwrap_or(None)

        assert approval is None
