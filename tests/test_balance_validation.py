"""Tests for balance validation utilities."""

import pytest
import tempfile
import os
import time

from services.balance_validation import (
    validate_can_spend,
    validate_positive_balance,
    validate_has_amount,
)
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
def player_repo(temp_db):
    """Create a player repository."""
    return PlayerRepository(temp_db)


@pytest.fixture
def player_with_balance(player_repo):
    """Create a player with 100 balance."""
    discord_id = 12345
    player_repo.add(
        discord_id=discord_id,
        discord_username="TestPlayer",
        glicko_rating=1500,
        glicko_rd=350,
        jopacoin_balance=100,
    )
    return discord_id


@pytest.fixture
def player_in_debt(player_repo):
    """Create a player with -50 balance."""
    discord_id = 67890
    player_repo.add(
        discord_id=discord_id,
        discord_username="DebtPlayer",
        glicko_rating=1500,
        glicko_rd=350,
        jopacoin_balance=-50,
    )
    return discord_id


class TestValidateCanSpend:
    """Tests for validate_can_spend."""

    def test_can_spend_within_balance(self, player_repo, player_with_balance):
        """Can spend when amount is within balance."""
        result = validate_can_spend(player_repo, player_with_balance, 50)

        assert result.success is True
        assert result.value == 50  # New balance: 100 - 50

    def test_can_spend_entire_balance(self, player_repo, player_with_balance):
        """Can spend entire balance."""
        result = validate_can_spend(player_repo, player_with_balance, 100)

        assert result.success is True
        assert result.value == 0

    def test_can_spend_into_debt(self, player_repo, player_with_balance):
        """Can spend into debt within max_debt limit."""
        result = validate_can_spend(player_repo, player_with_balance, 150, max_debt=100)

        assert result.success is True
        assert result.value == -50  # 100 - 150

    def test_cannot_exceed_max_debt(self, player_repo, player_with_balance):
        """Cannot spend beyond max_debt."""
        result = validate_can_spend(player_repo, player_with_balance, 250, max_debt=100)

        assert result.success is False
        assert result.error_code == error_codes.MAX_DEBT_EXCEEDED

    def test_uses_default_max_debt(self, player_repo, player_with_balance):
        """Uses config.MAX_DEBT as default."""
        # This should use the default MAX_DEBT from config
        result = validate_can_spend(player_repo, player_with_balance, 100)

        assert result.success is True


class TestValidatePositiveBalance:
    """Tests for validate_positive_balance."""

    def test_positive_balance_succeeds(self, player_repo, player_with_balance):
        """Player with positive balance passes."""
        result = validate_positive_balance(player_repo, player_with_balance)

        assert result.success is True
        assert result.value == 100

    def test_zero_balance_succeeds(self, player_repo, player_with_balance):
        """Player with zero balance passes."""
        player_repo.update_balance(player_with_balance, 0)
        result = validate_positive_balance(player_repo, player_with_balance)

        assert result.success is True
        assert result.value == 0

    def test_negative_balance_fails(self, player_repo, player_in_debt):
        """Player with negative balance fails."""
        result = validate_positive_balance(player_repo, player_in_debt)

        assert result.success is False
        assert result.error_code == error_codes.IN_DEBT


class TestValidateHasAmount:
    """Tests for validate_has_amount."""

    def test_has_sufficient_balance(self, player_repo, player_with_balance):
        """Player with sufficient balance passes."""
        result = validate_has_amount(player_repo, player_with_balance, 50)

        assert result.success is True
        assert result.value == 100

    def test_has_exact_balance(self, player_repo, player_with_balance):
        """Player with exact amount passes."""
        result = validate_has_amount(player_repo, player_with_balance, 100)

        assert result.success is True
        assert result.value == 100

    def test_insufficient_balance_fails(self, player_repo, player_with_balance):
        """Player with insufficient balance fails."""
        result = validate_has_amount(player_repo, player_with_balance, 150)

        assert result.success is False
        assert result.error_code == error_codes.INSUFFICIENT_FUNDS

    def test_player_in_debt_fails(self, player_repo, player_in_debt):
        """Player in debt fails for any positive amount."""
        result = validate_has_amount(player_repo, player_in_debt, 10)

        assert result.success is False
        assert result.error_code == error_codes.INSUFFICIENT_FUNDS
