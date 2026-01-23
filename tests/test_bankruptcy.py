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


class TestBankruptcyCount:
    """Tests for bankruptcy count tracking."""

    def test_first_bankruptcy_sets_count_to_one(self, db_and_repos, bankruptcy_service):
        """First bankruptcy sets count to 1."""
        player_repo = db_and_repos["player_repo"]
        bet_repo = db_and_repos["bet_repo"]
        pid = create_test_player(player_repo, 1001, balance=-200)

        bankruptcy_service.declare_bankruptcy(pid)

        count = bet_repo.get_player_bankruptcy_count(pid)
        assert count == 1

    def test_multiple_bankruptcies_increment_count(self, db_and_repos):
        """Multiple bankruptcies increment the count correctly."""
        player_repo = db_and_repos["player_repo"]
        bankruptcy_repo = db_and_repos["bankruptcy_repo"]
        bet_repo = db_and_repos["bet_repo"]

        # Use very short cooldown for testing
        service = BankruptcyService(
            bankruptcy_repo=bankruptcy_repo,
            player_repo=player_repo,
            cooldown_seconds=0,  # No cooldown for testing
            penalty_games=5,
            penalty_rate=0.5,
        )

        pid = create_test_player(player_repo, 1001, balance=-200)

        # First bankruptcy
        service.declare_bankruptcy(pid)
        assert bet_repo.get_player_bankruptcy_count(pid) == 1

        # Put back in debt and declare again
        player_repo.update_balance(pid, -100)
        service.declare_bankruptcy(pid)
        assert bet_repo.get_player_bankruptcy_count(pid) == 2

        # Third bankruptcy
        player_repo.update_balance(pid, -50)
        service.declare_bankruptcy(pid)
        assert bet_repo.get_player_bankruptcy_count(pid) == 3

    def test_reset_cooldown_does_not_increment_count(self, db_and_repos, bankruptcy_service):
        """Admin reset of cooldown should not increment bankruptcy count."""
        player_repo = db_and_repos["player_repo"]
        bankruptcy_repo = db_and_repos["bankruptcy_repo"]
        bet_repo = db_and_repos["bet_repo"]
        pid = create_test_player(player_repo, 1001, balance=-200)

        # Declare bankruptcy
        bankruptcy_service.declare_bankruptcy(pid)
        assert bet_repo.get_player_bankruptcy_count(pid) == 1

        # Admin resets cooldown using reset_cooldown_only (not upsert_state)
        bankruptcy_repo.reset_cooldown_only(
            discord_id=pid,
            last_bankruptcy_at=0,
            penalty_games_remaining=0,
        )

        # Count should still be 1
        assert bet_repo.get_player_bankruptcy_count(pid) == 1

    def test_player_with_no_bankruptcy_has_zero_count(self, db_and_repos, bankruptcy_service):
        """Players who never declared bankruptcy have count of 0."""
        player_repo = db_and_repos["player_repo"]
        bet_repo = db_and_repos["bet_repo"]
        pid = create_test_player(player_repo, 1001, balance=100)

        count = bet_repo.get_player_bankruptcy_count(pid)
        assert count == 0


class TestBulkBankruptcyState:
    """Tests for bulk state fetching."""

    def test_get_bulk_states_empty_list(self, db_and_repos, bankruptcy_service):
        """Empty list should return empty dict."""
        result = bankruptcy_service.get_bulk_states([])
        assert result == {}

    def test_get_bulk_states_single_user_no_bankruptcy(self, db_and_repos, bankruptcy_service):
        """User without bankruptcy should return default state."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 1001, balance=100)

        states = bankruptcy_service.get_bulk_states([pid])

        assert len(states) == 1
        assert pid in states
        assert states[pid].discord_id == pid
        assert states[pid].penalty_games_remaining == 0
        assert states[pid].is_on_cooldown is False
        assert states[pid].last_bankruptcy_at is None

    def test_get_bulk_states_single_user_with_bankruptcy(self, db_and_repos, bankruptcy_service):
        """User with bankruptcy should return correct state."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 1001, balance=-200)

        bankruptcy_service.declare_bankruptcy(pid)
        states = bankruptcy_service.get_bulk_states([pid])

        assert len(states) == 1
        assert states[pid].discord_id == pid
        assert states[pid].penalty_games_remaining == 5
        assert states[pid].is_on_cooldown is True
        assert states[pid].last_bankruptcy_at is not None

    def test_get_bulk_states_multiple_users_mixed(self, db_and_repos, bankruptcy_service):
        """Should correctly fetch mixed users - some with bankruptcy, some without."""
        player_repo = db_and_repos["player_repo"]

        # User with bankruptcy
        pid1 = create_test_player(player_repo, 1001, balance=-200)
        bankruptcy_service.declare_bankruptcy(pid1)

        # User without bankruptcy (positive balance)
        pid2 = create_test_player(player_repo, 1002, balance=50)

        # User without bankruptcy (never registered for bankruptcy)
        pid3 = create_test_player(player_repo, 1003, balance=10)

        states = bankruptcy_service.get_bulk_states([pid1, pid2, pid3])

        assert len(states) == 3

        # User 1: has bankruptcy
        assert states[pid1].penalty_games_remaining == 5
        assert states[pid1].is_on_cooldown is True

        # User 2: no bankruptcy
        assert states[pid2].penalty_games_remaining == 0
        assert states[pid2].is_on_cooldown is False

        # User 3: no bankruptcy
        assert states[pid3].penalty_games_remaining == 0
        assert states[pid3].is_on_cooldown is False

    def test_get_bulk_states_cooldown_calculation(self, db_and_repos):
        """Should correctly calculate cooldown status for bulk fetch."""
        player_repo = db_and_repos["player_repo"]
        bankruptcy_repo = db_and_repos["bankruptcy_repo"]

        # Create service with very short cooldown for testing
        short_cooldown_service = BankruptcyService(
            bankruptcy_repo=bankruptcy_repo,
            player_repo=player_repo,
            cooldown_seconds=1,  # 1 second cooldown
            penalty_games=5,
            penalty_rate=0.5,
        )

        pid = create_test_player(player_repo, 1001, balance=-200)
        short_cooldown_service.declare_bankruptcy(pid)

        # Immediately after: should be on cooldown
        states = short_cooldown_service.get_bulk_states([pid])
        assert states[pid].is_on_cooldown is True

        # Wait for cooldown to expire
        time.sleep(1.1)

        # After cooldown: should not be on cooldown
        states = short_cooldown_service.get_bulk_states([pid])
        assert states[pid].is_on_cooldown is False

    def test_get_bulk_states_nonexistent_user(self, db_and_repos, bankruptcy_service):
        """Non-existent user should return default state."""
        # User ID that doesn't exist in any table
        nonexistent_id = 999999999

        states = bankruptcy_service.get_bulk_states([nonexistent_id])

        assert len(states) == 1
        assert nonexistent_id in states
        assert states[nonexistent_id].penalty_games_remaining == 0
        assert states[nonexistent_id].is_on_cooldown is False

    def test_get_bulk_states_repo_returns_only_existing(self, db_and_repos, bankruptcy_service):
        """Repository bulk fetch should only return users with bankruptcy records."""
        player_repo = db_and_repos["player_repo"]
        bankruptcy_repo = db_and_repos["bankruptcy_repo"]

        # User with bankruptcy
        pid1 = create_test_player(player_repo, 1001, balance=-200)
        bankruptcy_service.declare_bankruptcy(pid1)

        # User without bankruptcy
        pid2 = create_test_player(player_repo, 1002, balance=50)

        # Repository-level fetch (raw, not service)
        raw_states = bankruptcy_repo.get_bulk_states([pid1, pid2])

        # Repository only returns users WITH bankruptcy records
        assert len(raw_states) == 1
        assert pid1 in raw_states
        assert pid2 not in raw_states

    def test_get_bulk_states_with_duplicates(self, db_and_repos, bankruptcy_service):
        """Should handle duplicate IDs in input list."""
        player_repo = db_and_repos["player_repo"]
        pid = create_test_player(player_repo, 1001, balance=-200)
        bankruptcy_service.declare_bankruptcy(pid)

        # Pass same ID multiple times
        states = bankruptcy_service.get_bulk_states([pid, pid, pid])

        # Should still return just one entry
        assert len(states) == 1
        assert states[pid].penalty_games_remaining == 5
