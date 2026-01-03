"""
Tests for the Federal Reserve (totally legitimate) commands.
"""

import os
import tempfile

import pytest

from database import Database
from repositories.player_repository import PlayerRepository
from commands.federal_reserve import (
    _is_chairman,
    FEDERAL_RESERVE_CHAIRMAN,
    DENIED_MESSAGES,
    BAILOUT_DENIED_MESSAGES,
    COMMUNITY_SERVICE_DENIED,
)


class MockUser:
    """Mock Discord user for testing."""
    def __init__(self, name: str, user_id: int = 12345):
        self.name = name
        self.id = user_id


@pytest.fixture
def db_and_repo():
    """Create test database and player repository."""
    fd, db_path = tempfile.mkstemp(suffix=".db")
    os.close(fd)

    db = Database(db_path)
    player_repo = PlayerRepository(db_path)

    yield {"db": db, "player_repo": player_repo, "db_path": db_path}

    try:
        os.unlink(db_path)
    except OSError:
        pass


class TestChairmanCheck:
    """Tests for the chairman identification logic."""

    def test_chairman_is_recognized(self):
        """The Fed Chairman should be recognized."""
        user = MockUser("Jmgoblue77?")
        assert _is_chairman(user) is True

    def test_chairman_case_insensitive(self):
        """Chairman check should be case insensitive."""
        user = MockUser("jmgoblue77?")
        assert _is_chairman(user) is True

        user = MockUser("JMGOBLUE77?")
        assert _is_chairman(user) is True

    def test_non_chairman_is_rejected(self):
        """Random plebs should not be recognized as chairman."""
        user = MockUser("RandomPeasant")
        assert _is_chairman(user) is False

        user = MockUser("Jmgoblue78")  # Close but no cigar
        assert _is_chairman(user) is False

    def test_chairman_constant_is_correct(self):
        """The chairman username constant should be correct."""
        assert FEDERAL_RESERVE_CHAIRMAN.lower() == "jmgoblue77?"


class TestDenialMessages:
    """Ensure denial messages exist and are snarky."""

    def test_denied_messages_exist(self):
        """Should have multiple denial messages for variety."""
        assert len(DENIED_MESSAGES) >= 3

    def test_bailout_denied_messages_exist(self):
        """Should have multiple bailout denial messages."""
        assert len(BAILOUT_DENIED_MESSAGES) >= 3

    def test_community_service_denied_exist(self):
        """Should have community service denial messages."""
        assert len(COMMUNITY_SERVICE_DENIED) >= 3


class TestPrintMoneyLogic:
    """Test the balance modification logic that would be used by printmoney."""

    def test_add_balance_works(self, db_and_repo):
        """Verify we can add balance to a player."""
        player_repo = db_and_repo["player_repo"]

        # Create a player
        player_repo.add(
            discord_id=12345,
            discord_username="Jmgoblue77?",
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )

        # Check initial balance (should be 3 from default)
        initial_balance = player_repo.get_balance(12345)
        assert initial_balance == 3

        # Print some money
        player_repo.add_balance(12345, 1000)

        # Verify new balance
        new_balance = player_repo.get_balance(12345)
        assert new_balance == 1003


class TestBailoutLogic:
    """Test the debt clearing logic that would be used by bailout."""

    def test_clear_debt_and_add_bonus(self, db_and_repo):
        """Verify we can clear debt and add bonus."""
        player_repo = db_and_repo["player_repo"]

        # Create a player with debt
        player_repo.add(
            discord_id=12345,
            discord_username="Jmgoblue77?",
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )

        # Put them in debt
        player_repo.update_balance(12345, -300)
        assert player_repo.get_balance(12345) == -300

        # Bailout: clear debt and add bonus
        bailout_amount = 500
        player_repo.update_balance(12345, bailout_amount)

        # Verify debt is cleared and bonus added
        assert player_repo.get_balance(12345) == 500


class TestCommunityServiceLogic:
    """Test the community service logic."""

    def test_community_service_reduces_debt(self, db_and_repo):
        """Community service should reduce debt."""
        player_repo = db_and_repo["player_repo"]

        # Create a player in debt
        player_repo.add(
            discord_id=12345,
            discord_username="Jmgoblue77?",
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
        player_repo.update_balance(12345, -200)

        # Do 2 hours of community service (50 coins/hour)
        hours = 2
        coins_per_hour = 50
        earned = hours * coins_per_hour

        player_repo.add_balance(12345, earned)

        # Should have reduced debt from -200 to -100
        assert player_repo.get_balance(12345) == -100

    def test_community_service_can_clear_debt(self, db_and_repo):
        """Enough community service should clear debt entirely."""
        player_repo = db_and_repo["player_repo"]

        # Create a player with small debt
        player_repo.add(
            discord_id=12345,
            discord_username="Jmgoblue77?",
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
        player_repo.update_balance(12345, -100)

        # Do 5 hours of community service
        hours = 5
        coins_per_hour = 50
        earned = hours * coins_per_hour

        player_repo.add_balance(12345, earned)

        # Should now have positive balance
        assert player_repo.get_balance(12345) == 150
