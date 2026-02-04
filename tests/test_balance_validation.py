"""Tests for balance validation utilities."""

import pytest

from services.balance_validation import (
    validate_can_spend,
    validate_positive_balance,
    validate_has_amount,
)
from services import error_codes
from repositories.player_repository import PlayerRepository


@pytest.fixture
def player_repo(repo_db_path):
    """Create a player repository using centralized fast fixture."""
    return PlayerRepository(repo_db_path)


TEST_GUILD_ID = 12345


@pytest.fixture
def player_with_balance(player_repo):
    """Create a player with 100 balance."""
    discord_id = 12345
    player_repo.add(
        discord_id=discord_id,
        discord_username="TestPlayer",
        guild_id=TEST_GUILD_ID,
        glicko_rating=1500,
        glicko_rd=350,
    )
    player_repo.update_balance(discord_id, TEST_GUILD_ID, 100)
    return discord_id


@pytest.fixture
def player_in_debt(player_repo):
    """Create a player with -50 balance."""
    discord_id = 67890
    player_repo.add(
        discord_id=discord_id,
        discord_username="DebtPlayer",
        guild_id=TEST_GUILD_ID,
        glicko_rating=1500,
        glicko_rd=350,
    )
    player_repo.update_balance(discord_id, TEST_GUILD_ID, -50)
    return discord_id


class TestValidateCanSpend:
    """Tests for validate_can_spend."""

    def test_can_spend_within_balance(self, player_repo, player_with_balance):
        """Can spend when amount is within balance."""
        result = validate_can_spend(player_repo, player_with_balance, 50, guild_id=TEST_GUILD_ID)

        assert result.success is True
        assert result.value == 50  # New balance: 100 - 50

    def test_can_spend_entire_balance(self, player_repo, player_with_balance):
        """Can spend entire balance."""
        result = validate_can_spend(player_repo, player_with_balance, 100, guild_id=TEST_GUILD_ID)

        assert result.success is True
        assert result.value == 0

    def test_can_spend_into_debt(self, player_repo, player_with_balance):
        """Can spend into debt within max_debt limit."""
        result = validate_can_spend(player_repo, player_with_balance, 150, max_debt=100, guild_id=TEST_GUILD_ID)

        assert result.success is True
        assert result.value == -50  # 100 - 150

    def test_cannot_exceed_max_debt(self, player_repo, player_with_balance):
        """Cannot spend beyond max_debt."""
        result = validate_can_spend(player_repo, player_with_balance, 250, max_debt=100, guild_id=TEST_GUILD_ID)

        assert result.success is False
        assert result.error_code == error_codes.MAX_DEBT_EXCEEDED

    def test_uses_default_max_debt(self, player_repo, player_with_balance):
        """Uses config.MAX_DEBT as default."""
        # This should use the default MAX_DEBT from config
        result = validate_can_spend(player_repo, player_with_balance, 100, guild_id=TEST_GUILD_ID)

        assert result.success is True


class TestValidatePositiveBalance:
    """Tests for validate_positive_balance."""

    def test_positive_balance_succeeds(self, player_repo, player_with_balance):
        """Player with positive balance passes."""
        result = validate_positive_balance(player_repo, player_with_balance, guild_id=TEST_GUILD_ID)

        assert result.success is True
        assert result.value == 100

    def test_zero_balance_succeeds(self, player_repo, player_with_balance):
        """Player with zero balance passes."""
        player_repo.update_balance(player_with_balance, TEST_GUILD_ID, 0)
        result = validate_positive_balance(player_repo, player_with_balance, guild_id=TEST_GUILD_ID)

        assert result.success is True
        assert result.value == 0

    def test_negative_balance_fails(self, player_repo, player_in_debt):
        """Player with negative balance fails."""
        result = validate_positive_balance(player_repo, player_in_debt, guild_id=TEST_GUILD_ID)

        assert result.success is False
        assert result.error_code == error_codes.IN_DEBT


class TestValidateHasAmount:
    """Tests for validate_has_amount."""

    def test_has_sufficient_balance(self, player_repo, player_with_balance):
        """Player with sufficient balance passes."""
        result = validate_has_amount(player_repo, player_with_balance, 50, guild_id=TEST_GUILD_ID)

        assert result.success is True
        assert result.value == 100

    def test_has_exact_balance(self, player_repo, player_with_balance):
        """Player with exact amount passes."""
        result = validate_has_amount(player_repo, player_with_balance, 100, guild_id=TEST_GUILD_ID)

        assert result.success is True
        assert result.value == 100

    def test_insufficient_balance_fails(self, player_repo, player_with_balance):
        """Player with insufficient balance fails."""
        result = validate_has_amount(player_repo, player_with_balance, 150, guild_id=TEST_GUILD_ID)

        assert result.success is False
        assert result.error_code == error_codes.INSUFFICIENT_FUNDS

    def test_player_in_debt_fails(self, player_repo, player_in_debt):
        """Player in debt fails for any positive amount."""
        result = validate_has_amount(player_repo, player_in_debt, 10, guild_id=TEST_GUILD_ID)

        assert result.success is False
        assert result.error_code == error_codes.INSUFFICIENT_FUNDS
