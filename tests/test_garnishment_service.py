"""
Tests for GarnishmentService.

This service applies garnishment to income for players with debt (negative balance).
When a player has debt, a portion of their income is garnished to pay it down.
"""

import pytest

from services.garnishment_service import GarnishmentService
from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY


@pytest.fixture
def garnishment_service(player_repository):
    """Create a GarnishmentService with 100% garnishment rate."""
    return GarnishmentService(player_repository, garnishment_rate=1.0)


@pytest.fixture
def garnishment_service_50_percent(player_repository):
    """Create a GarnishmentService with 50% garnishment rate."""
    return GarnishmentService(player_repository, garnishment_rate=0.5)


@pytest.fixture
def test_player(player_repository):
    """Create a test player with default balance (3 JC)."""
    player_id = 10001
    player_repository.add(
        discord_id=player_id,
        discord_username="TestPlayer",
        guild_id=TEST_GUILD_ID,
        initial_mmr=1500,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    return player_id


class TestGarnishmentService:
    """Test GarnishmentService functionality."""

    def test_no_garnishment_when_positive_balance(
        self, garnishment_service, player_repository, test_player
    ):
        """Test that no garnishment is applied when balance is positive."""
        # Player starts with 3 JC (positive)
        result = garnishment_service.add_income(test_player, 100, TEST_GUILD_ID)

        assert result["gross"] == 100
        assert result["garnished"] == 0
        assert result["net"] == 100

        # Balance should be 103 (3 + 100)
        balance = player_repository.get_balance(test_player, TEST_GUILD_ID)
        assert balance == 103

    def test_full_garnishment_when_in_debt(
        self, garnishment_service, player_repository, test_player
    ):
        """Test that full garnishment is applied when in debt with 100% rate."""
        # Put player in debt
        player_repository.update_balance(test_player, TEST_GUILD_ID, -50)

        result = garnishment_service.add_income(test_player, 30, TEST_GUILD_ID)

        assert result["gross"] == 30
        assert result["garnished"] == 30  # 100% of 30
        assert result["net"] == 0  # All went to debt

        # Balance should be -20 (-50 + 30)
        balance = player_repository.get_balance(test_player, TEST_GUILD_ID)
        assert balance == -20

    def test_partial_garnishment_with_50_percent_rate(
        self, garnishment_service_50_percent, player_repository, test_player
    ):
        """Test that partial garnishment is applied with 50% rate."""
        # Put player in debt
        player_repository.update_balance(test_player, TEST_GUILD_ID, -100)

        result = garnishment_service_50_percent.add_income(test_player, 40, TEST_GUILD_ID)

        assert result["gross"] == 40
        assert result["garnished"] == 20  # 50% of 40
        assert result["net"] == 20  # Remaining after garnishment

        # Balance should be -60 (-100 + 40)
        balance = player_repository.get_balance(test_player, TEST_GUILD_ID)
        assert balance == -60

    def test_zero_income_no_garnishment(
        self, garnishment_service, player_repository, test_player
    ):
        """Test that zero income results in no garnishment."""
        player_repository.update_balance(test_player, TEST_GUILD_ID, -50)

        result = garnishment_service.add_income(test_player, 0, TEST_GUILD_ID)

        assert result["gross"] == 0
        assert result["garnished"] == 0
        assert result["net"] == 0

    def test_negative_income_no_garnishment(
        self, garnishment_service, player_repository, test_player
    ):
        """Test that negative income (like a loss) has no garnishment."""
        player_repository.update_balance(test_player, TEST_GUILD_ID, -50)

        result = garnishment_service.add_income(test_player, -20, TEST_GUILD_ID)

        assert result["gross"] == -20
        assert result["garnished"] == 0
        assert result["net"] == -20


class TestGarnishmentTransitions:
    """Test garnishment behavior around the zero-balance boundary."""

    def test_income_pays_off_debt_completely(
        self, garnishment_service, player_repository, test_player
    ):
        """Test that income can fully pay off debt."""
        # Put player in small debt
        player_repository.update_balance(test_player, TEST_GUILD_ID, -30)

        result = garnishment_service.add_income(test_player, 50, TEST_GUILD_ID)

        assert result["gross"] == 50
        assert result["garnished"] == 50  # All applied to balance
        assert result["net"] == 0  # From player's perspective

        # Balance should now be positive: -30 + 50 = 20
        balance = player_repository.get_balance(test_player, TEST_GUILD_ID)
        assert balance == 20

    def test_exactly_at_zero_balance(
        self, garnishment_service, player_repository, test_player
    ):
        """Test income when balance is exactly zero."""
        player_repository.update_balance(test_player, TEST_GUILD_ID, 0)

        result = garnishment_service.add_income(test_player, 100, TEST_GUILD_ID)

        # At zero balance, no debt, so no garnishment
        assert result["gross"] == 100
        assert result["garnished"] == 0
        assert result["net"] == 100


class TestGarnishmentRates:
    """Test different garnishment rate configurations."""

    def test_default_garnishment_rate_uses_config(self, player_repository):
        """Test that default garnishment rate uses config value."""
        from config import GARNISHMENT_PERCENTAGE

        service = GarnishmentService(player_repository)
        assert service.garnishment_rate == GARNISHMENT_PERCENTAGE

    def test_custom_garnishment_rate(self, player_repository):
        """Test that custom garnishment rate is applied."""
        service = GarnishmentService(player_repository, garnishment_rate=0.75)
        assert service.garnishment_rate == 0.75

    def test_zero_garnishment_rate(self, player_repository, test_player):
        """Test that zero garnishment rate means no garnishment."""
        service = GarnishmentService(player_repository, garnishment_rate=0.0)
        player_repository.update_balance(test_player, TEST_GUILD_ID, -100)

        result = service.add_income(test_player, 50, TEST_GUILD_ID)

        assert result["garnished"] == 0
        assert result["net"] == 50


class TestGarnishmentGuildIsolation:
    """Test that garnishment respects guild isolation."""

    def test_garnishment_uses_correct_guild_balance(self, player_repository):
        """Test that garnishment checks balance in the correct guild."""
        service = GarnishmentService(player_repository, garnishment_rate=1.0)

        # Create player in two guilds with different balances
        player_id = 20001
        player_repository.add(
            discord_id=player_id,
            discord_username="MultiGuildPlayer",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1500,
        )
        player_repository.add(
            discord_id=player_id,
            discord_username="MultiGuildPlayer",
            guild_id=TEST_GUILD_ID_SECONDARY,
            initial_mmr=1500,
        )

        # Set different balances
        player_repository.update_balance(player_id, TEST_GUILD_ID, -50)  # In debt
        player_repository.update_balance(player_id, TEST_GUILD_ID_SECONDARY, 100)  # Positive

        # Add income to each guild
        result1 = service.add_income(player_id, 30, TEST_GUILD_ID)
        result2 = service.add_income(player_id, 30, TEST_GUILD_ID_SECONDARY)

        # Guild with debt should have garnishment
        assert result1["garnished"] == 30

        # Guild without debt should have no garnishment
        assert result2["garnished"] == 0


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
