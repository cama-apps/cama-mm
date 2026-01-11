"""
Tests for the bankruptcy feature.
"""

import os
import tempfile
import time

import pytest

from database import Database
from repositories.bet_repository import BetRepository
from repositories.player_repository import PlayerRepository
from services.bankruptcy_service import BankruptcyRepository, BankruptcyService
from services.betting_service import BettingService


@pytest.fixture
def db_and_repos():
    """Create test database and repositories."""
    fd, db_path = tempfile.mkstemp(suffix=".db")
    os.close(fd)

    db = Database(db_path)
    player_repo = PlayerRepository(db_path)
    bankruptcy_repo = BankruptcyRepository(db_path)
    bet_repo = BetRepository(db_path)

    yield {
        "db": db,
        "player_repo": player_repo,
        "bankruptcy_repo": bankruptcy_repo,
        "bet_repo": bet_repo,
        "db_path": db_path,
    }

    try:
        os.unlink(db_path)
    except OSError:
        pass


@pytest.fixture
def bankruptcy_service(db_and_repos):
    """Create bankruptcy service with test settings."""
    return BankruptcyService(
        bankruptcy_repo=db_and_repos["bankruptcy_repo"],
        player_repo=db_and_repos["player_repo"],
        cooldown_seconds=604800,  # 1 week
        penalty_games=5,
        penalty_rate=0.5,
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


class TestBankruptcyEligibility:
    """Tests for bankruptcy eligibility checks."""

    def test_cannot_declare_bankruptcy_with_positive_balance(
        self, db_and_repos, bankruptcy_service
    ):
        """Players with positive balance cannot declare bankruptcy."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 1001, balance=100)

        result = bankruptcy_service.can_declare_bankruptcy(pid)
        assert result["allowed"] is False
        assert result["reason"] == "not_in_debt"
        assert result["balance"] == 100

    def test_cannot_declare_bankruptcy_with_zero_balance(self, db_and_repos, bankruptcy_service):
        """Players with zero balance cannot declare bankruptcy."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 1001, balance=0)

        result = bankruptcy_service.can_declare_bankruptcy(pid)
        assert result["allowed"] is False
        assert result["reason"] == "not_in_debt"
        assert result["balance"] == 0

    def test_can_declare_bankruptcy_with_negative_balance(self, db_and_repos, bankruptcy_service):
        """Players with negative balance can declare bankruptcy."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 1001, balance=-100)

        result = bankruptcy_service.can_declare_bankruptcy(pid)
        assert result["allowed"] is True
        assert result["debt"] == 100


class TestBankruptcyCooldown:
    """Tests for bankruptcy cooldown enforcement."""

    def test_cannot_declare_bankruptcy_on_cooldown(self, db_and_repos, bankruptcy_service):
        """Players on cooldown cannot declare bankruptcy again."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 1001, balance=-200)

        # First bankruptcy should succeed
        result1 = bankruptcy_service.declare_bankruptcy(pid)
        assert result1["success"] is True

        # Put player back in debt
        player_repo.update_balance(pid, -100)

        # Second bankruptcy should fail due to cooldown
        result2 = bankruptcy_service.can_declare_bankruptcy(pid)
        assert result2["allowed"] is False
        assert result2["reason"] == "on_cooldown"

    def test_cooldown_expires_after_duration(self, db_and_repos):
        """Players can declare bankruptcy after cooldown expires."""
        player_repo = db_and_repos["player_repo"]
        bankruptcy_repo = db_and_repos["bankruptcy_repo"]

        # Create service with 1 second cooldown for testing
        service = BankruptcyService(
            bankruptcy_repo=bankruptcy_repo,
            player_repo=player_repo,
            cooldown_seconds=1,  # Very short for testing
            penalty_games=5,
            penalty_rate=0.5,
        )

        pid = create_test_player(player_repo, 1001, balance=-200)

        # First bankruptcy
        result1 = service.declare_bankruptcy(pid)
        assert result1["success"] is True

        # Wait for cooldown to expire
        time.sleep(1.1)

        # Put player back in debt
        player_repo.update_balance(pid, -100)

        # Should be able to declare again
        result2 = service.can_declare_bankruptcy(pid)
        assert result2["allowed"] is True


class TestBankruptcyDeclaration:
    """Tests for successful bankruptcy declaration."""

    def test_bankruptcy_clears_debt(self, db_and_repos, bankruptcy_service):
        """Declaring bankruptcy clears debt and gives fresh start balance of 3."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 1001, balance=-300)

        result = bankruptcy_service.declare_bankruptcy(pid)

        assert result["success"] is True
        assert result["debt_cleared"] == 300
        assert player_repo.get_balance(pid) == 3

    def test_bankruptcy_sets_penalty_games(self, db_and_repos, bankruptcy_service):
        """Declaring bankruptcy sets penalty games."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 1001, balance=-300)

        result = bankruptcy_service.declare_bankruptcy(pid)

        assert result["penalty_games"] == 5
        state = bankruptcy_service.get_state(pid)
        assert state.penalty_games_remaining == 5


class TestBankruptcyPenalty:
    """Tests for bankruptcy penalty application."""

    def test_penalty_reduces_winnings(self, db_and_repos, bankruptcy_service):
        """Bankruptcy penalty reduces win bonuses."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 1001, balance=-300)

        # Declare bankruptcy
        bankruptcy_service.declare_bankruptcy(pid)

        # Apply penalty to winnings
        result = bankruptcy_service.apply_penalty_to_winnings(pid, 10)

        assert result["original"] == 10
        assert result["penalized"] == 5  # 50% penalty
        assert result["penalty_applied"] == 5

    def test_no_penalty_without_bankruptcy(self, db_and_repos, bankruptcy_service):
        """Players without bankruptcy get full winnings."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 1001, balance=100)

        result = bankruptcy_service.apply_penalty_to_winnings(pid, 10)

        assert result["original"] == 10
        assert result["penalized"] == 10
        assert result["penalty_applied"] == 0

    def test_penalty_games_decrement(self, db_and_repos, bankruptcy_service):
        """Playing games decrements the penalty counter."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 1001, balance=-300)

        bankruptcy_service.declare_bankruptcy(pid)
        assert bankruptcy_service.get_state(pid).penalty_games_remaining == 5

        # Play 3 games
        for _ in range(3):
            bankruptcy_service.on_game_played(pid)

        assert bankruptcy_service.get_state(pid).penalty_games_remaining == 2

    def test_penalty_expires_after_games(self, db_and_repos, bankruptcy_service):
        """Penalty stops applying after all penalty games are played."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 1001, balance=-300)

        bankruptcy_service.declare_bankruptcy(pid)

        # Play all 5 penalty games
        for _ in range(5):
            bankruptcy_service.on_game_played(pid)

        # No more penalty
        result = bankruptcy_service.apply_penalty_to_winnings(pid, 10)
        assert result["penalized"] == 10
        assert result["penalty_applied"] == 0


class TestBettingServiceIntegration:
    """Tests for bankruptcy integration with betting service."""

    def test_win_bonus_applies_bankruptcy_penalty(self, db_and_repos):
        """Win bonuses are reduced for players with bankruptcy penalty."""
        player_repo = db_and_repos["player_repo"]
        bankruptcy_repo = db_and_repos["bankruptcy_repo"]
        bet_repo = db_and_repos["bet_repo"]

        bankruptcy_service = BankruptcyService(
            bankruptcy_repo=bankruptcy_repo,
            player_repo=player_repo,
            cooldown_seconds=604800,
            penalty_games=5,
            penalty_rate=0.5,
        )

        betting_service = BettingService(
            bet_repo=bet_repo,
            player_repo=player_repo,
            bankruptcy_service=bankruptcy_service,
        )

        # Create player with debt and declare bankruptcy
        pid = create_test_player(player_repo, 1001, balance=-200)
        bankruptcy_service.declare_bankruptcy(pid)

        # Award win bonus (default 2 jopacoin)
        results = betting_service.award_win_bonus([pid])

        # Should get half due to penalty
        assert results[pid]["bankruptcy_penalty"] == 1  # Half of 2 is 1
        assert results[pid]["net"] == 1  # Gets only 1 instead of 2

    def test_participation_decrements_penalty_games(self, db_and_repos):
        """Participation awards decrement bankruptcy penalty games."""
        player_repo = db_and_repos["player_repo"]
        bankruptcy_repo = db_and_repos["bankruptcy_repo"]
        bet_repo = db_and_repos["bet_repo"]

        bankruptcy_service = BankruptcyService(
            bankruptcy_repo=bankruptcy_repo,
            player_repo=player_repo,
            cooldown_seconds=604800,
            penalty_games=5,
            penalty_rate=0.5,
        )

        betting_service = BettingService(
            bet_repo=bet_repo,
            player_repo=player_repo,
            bankruptcy_service=bankruptcy_service,
        )

        # Create player with debt and declare bankruptcy
        pid = create_test_player(player_repo, 1001, balance=-200)
        bankruptcy_service.declare_bankruptcy(pid)

        assert bankruptcy_service.get_state(pid).penalty_games_remaining == 5

        # Award participation
        betting_service.award_participation([pid])

        # Penalty games should be decremented
        assert bankruptcy_service.get_state(pid).penalty_games_remaining == 4


class TestBankruptcyState:
    """Tests for bankruptcy state retrieval."""

    def test_no_state_for_new_player(self, db_and_repos, bankruptcy_service):
        """New players have no bankruptcy state."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 1001)

        state = bankruptcy_service.get_state(pid)

        assert state.discord_id == pid
        assert state.last_bankruptcy_at is None
        assert state.penalty_games_remaining == 0
        assert state.is_on_cooldown is False
        assert state.cooldown_ends_at is None

    def test_state_after_bankruptcy(self, db_and_repos, bankruptcy_service):
        """Bankruptcy state is correct after declaration."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 1001, balance=-200)

        now = int(time.time())
        bankruptcy_service.declare_bankruptcy(pid)

        state = bankruptcy_service.get_state(pid)

        assert state.discord_id == pid
        assert state.last_bankruptcy_at is not None
        assert state.last_bankruptcy_at >= now
        assert state.penalty_games_remaining == 5
        assert state.is_on_cooldown is True
        assert state.cooldown_ends_at is not None
