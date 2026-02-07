"""
Tests for admin override functionality in match recording.
"""

import pytest

from database import Database
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.match_service import MatchService
from tests.conftest import TEST_GUILD_ID


# =============================================================================
# MODULE-LEVEL FIXTURES (shared by all test classes)
# =============================================================================


@pytest.fixture
def admin_test_db(repo_db_path):
    """Create a test database using centralized fast fixture."""
    return Database(repo_db_path)


@pytest.fixture
def admin_player_repo(admin_test_db):
    """Create a PlayerRepository instance."""
    return PlayerRepository(admin_test_db.db_path)


@pytest.fixture
def admin_match_service(admin_test_db, admin_player_repo):
    """Create a MatchService instance with Glicko enabled."""
    match_repo = MatchRepository(admin_test_db.db_path)
    return MatchService(player_repo=admin_player_repo, match_repo=match_repo, use_glicko=True)


def _create_test_players(player_repo, player_ids):
    """Helper to create test players."""
    for pid in player_ids:
        player_repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            guild_id=TEST_GUILD_ID,
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
    return player_ids


@pytest.fixture
def admin_test_players(admin_player_repo):
    """Create 10 test players for admin tests."""
    return _create_test_players(admin_player_repo, list(range(5001, 5011)))


@pytest.fixture
def voting_test_players(admin_player_repo):
    """Create 10 test players for voting tests."""
    return _create_test_players(admin_player_repo, list(range(6001, 6011)))


@pytest.fixture
def abort_test_players(admin_player_repo):
    """Create 10 test players for abort tests."""
    return _create_test_players(admin_player_repo, list(range(7001, 7011)))


# =============================================================================
# TEST CLASSES
# =============================================================================


class TestAdminOverride:
    """Test admin override functionality for match recording."""

    def test_has_admin_submission_with_no_submissions(self, admin_match_service, admin_test_players):
        """Test has_admin_submission returns False when no submissions exist."""
        admin_match_service.shuffle_players(admin_test_players, guild_id=TEST_GUILD_ID)

        assert admin_match_service.has_admin_submission(TEST_GUILD_ID) is False

    def test_has_admin_submission_with_non_admin_submission(
        self, admin_match_service, admin_test_players
    ):
        """Test has_admin_submission returns False when only non-admin submits."""
        admin_match_service.shuffle_players(admin_test_players, guild_id=TEST_GUILD_ID)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1001, result="radiant", is_admin=False)

        assert admin_match_service.has_admin_submission(TEST_GUILD_ID) is False

    def test_has_admin_submission_with_admin_submission(self, admin_match_service, admin_test_players):
        """Test has_admin_submission returns True when admin submits."""
        admin_match_service.shuffle_players(admin_test_players, guild_id=TEST_GUILD_ID)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=9999, result="radiant", is_admin=True)

        assert admin_match_service.has_admin_submission(TEST_GUILD_ID) is True

    def test_can_record_match_with_admin_override(self, admin_match_service, admin_test_players):
        """Test can_record_match returns True with admin override, bypassing non-admin requirement."""
        admin_match_service.shuffle_players(admin_test_players, guild_id=TEST_GUILD_ID)

        # Admin submits - should bypass the 3 non-admin requirement
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=9999, result="radiant", is_admin=True)

        # Should be ready to record even though non_admin_count is 0
        assert admin_match_service.can_record_match(TEST_GUILD_ID) is True
        assert admin_match_service.get_non_admin_submission_count(TEST_GUILD_ID) == 0

    def test_can_record_match_without_admin_requires_3_non_admin(
        self, admin_match_service, admin_test_players
    ):
        """Test can_record_match requires 3 non-admin submissions when no admin submits."""
        admin_match_service.shuffle_players(admin_test_players, guild_id=TEST_GUILD_ID)

        # Add 2 non-admin submissions - should not be ready
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1001, result="radiant", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1002, result="radiant", is_admin=False)

        assert admin_match_service.can_record_match(TEST_GUILD_ID) is False
        assert admin_match_service.get_non_admin_submission_count(TEST_GUILD_ID) == 2

        # Add 3rd non-admin submission - should be ready
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1003, result="radiant", is_admin=False)

        assert admin_match_service.can_record_match(TEST_GUILD_ID) is True
        assert admin_match_service.get_non_admin_submission_count(TEST_GUILD_ID) == 3

    def test_admin_override_allows_immediate_recording(self, admin_match_service, admin_test_players):
        """Test that admin submission allows immediate match recording."""
        admin_match_service.shuffle_players(admin_test_players, guild_id=TEST_GUILD_ID)

        # Admin submits - should allow immediate recording
        submission = admin_match_service.add_record_submission(
            TEST_GUILD_ID, user_id=9999, result="radiant", is_admin=True
        )

        assert submission["is_ready"] is True
        assert submission["non_admin_count"] == 0
        assert admin_match_service.can_record_match(TEST_GUILD_ID) is True

        # Should be able to record match immediately
        record_result = admin_match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        assert record_result["match_id"] is not None
        assert record_result["winning_team"] == "radiant"
        assert record_result["updated_count"] == 10

    def test_admin_override_with_mixed_submissions(self, admin_match_service, admin_test_players):
        """Test admin override works even when non-admin submissions exist."""
        admin_match_service.shuffle_players(admin_test_players, guild_id=TEST_GUILD_ID)

        # Add 1 non-admin submission (not enough)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1001, result="radiant", is_admin=False)
        assert admin_match_service.can_record_match(TEST_GUILD_ID) is False

        # Admin submits - should override and allow recording
        submission = admin_match_service.add_record_submission(
            TEST_GUILD_ID, user_id=9999, result="radiant", is_admin=True
        )

        assert submission["is_ready"] is True
        assert submission["non_admin_count"] == 1  # Still only 1 non-admin
        assert admin_match_service.can_record_match(TEST_GUILD_ID) is True

        # Should be able to record
        record_result = admin_match_service.record_match("radiant", guild_id=TEST_GUILD_ID)
        assert record_result["match_id"] is not None

    def test_admin_override_clears_state_after_recording(
        self, admin_match_service, admin_test_players
    ):
        """Test that state is cleared after admin override recording."""
        admin_match_service.shuffle_players(admin_test_players, guild_id=TEST_GUILD_ID)

        # Admin submits and records
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=9999, result="radiant", is_admin=True)
        admin_match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        # State should be cleared
        assert admin_match_service.get_last_shuffle(TEST_GUILD_ID) is None
        assert admin_match_service.can_record_match(TEST_GUILD_ID) is False
        assert admin_match_service.has_admin_submission(TEST_GUILD_ID) is False


class TestFirstToThreeVoting:
    """Test first-to-3 voting system for non-admin match recording."""

    def test_get_vote_counts_empty(self, admin_match_service, voting_test_players):
        """Test get_vote_counts returns zeros when no submissions."""
        admin_match_service.shuffle_players(voting_test_players, guild_id=TEST_GUILD_ID)

        counts = admin_match_service.get_vote_counts(TEST_GUILD_ID)
        assert counts == {"radiant": 0, "dire": 0}

    def test_get_vote_counts_tracks_votes(self, admin_match_service, voting_test_players):
        """Test get_vote_counts correctly tracks non-admin votes."""
        admin_match_service.shuffle_players(voting_test_players, guild_id=TEST_GUILD_ID)

        # Add some votes
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1001, result="radiant", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1002, result="dire", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1003, result="radiant", is_admin=False)

        counts = admin_match_service.get_vote_counts(TEST_GUILD_ID)
        assert counts == {"radiant": 2, "dire": 1}

    def test_get_vote_counts_excludes_admin(self, admin_match_service, voting_test_players):
        """Test get_vote_counts does not count admin votes."""
        admin_match_service.shuffle_players(voting_test_players, guild_id=TEST_GUILD_ID)

        # Add admin and non-admin votes
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=9999, result="radiant", is_admin=True)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1001, result="radiant", is_admin=False)

        counts = admin_match_service.get_vote_counts(TEST_GUILD_ID)
        assert counts == {"radiant": 1, "dire": 0}

    def test_conflicting_votes_allowed(self, admin_match_service, voting_test_players):
        """Test that users can vote for different results (requires MIN_NON_ADMIN_SUBMISSIONS to confirm)."""
        admin_match_service.shuffle_players(voting_test_players, guild_id=TEST_GUILD_ID)

        # Add conflicting votes - should not raise
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1001, result="radiant", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1002, result="dire", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1003, result="radiant", is_admin=False)

        counts = admin_match_service.get_vote_counts(TEST_GUILD_ID)
        assert counts == {"radiant": 2, "dire": 1}

    def test_first_to_3_radiant_wins(self, admin_match_service, voting_test_players):
        """Test that radiant wins when it reaches 3 votes first."""
        admin_match_service.shuffle_players(voting_test_players, guild_id=TEST_GUILD_ID)

        # 2 radiant, 2 dire - not ready
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1001, result="radiant", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1002, result="dire", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1003, result="radiant", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1004, result="dire", is_admin=False)

        assert admin_match_service.can_record_match(TEST_GUILD_ID) is False
        assert admin_match_service.get_pending_record_result(TEST_GUILD_ID) is None

        # 3rd radiant vote - radiant wins!
        submission = admin_match_service.add_record_submission(
            TEST_GUILD_ID, user_id=1005, result="radiant", is_admin=False
        )

        assert submission["is_ready"] is True
        assert submission["result"] == "radiant"
        assert admin_match_service.can_record_match(TEST_GUILD_ID) is True
        assert admin_match_service.get_pending_record_result(TEST_GUILD_ID) == "radiant"

    def test_first_to_3_dire_wins(self, admin_match_service, voting_test_players):
        """Test that dire wins when it reaches 3 votes first."""
        admin_match_service.shuffle_players(voting_test_players, guild_id=TEST_GUILD_ID)

        # 1 radiant, 2 dire
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1001, result="radiant", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1002, result="dire", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1003, result="dire", is_admin=False)

        assert admin_match_service.can_record_match(TEST_GUILD_ID) is False

        # 3rd dire vote - dire wins!
        submission = admin_match_service.add_record_submission(
            TEST_GUILD_ID, user_id=1004, result="dire", is_admin=False
        )

        assert submission["is_ready"] is True
        assert submission["result"] == "dire"
        assert admin_match_service.get_pending_record_result(TEST_GUILD_ID) == "dire"

    def test_user_cannot_change_vote(self, admin_match_service, voting_test_players):
        """Test that a user cannot change their vote."""
        admin_match_service.shuffle_players(voting_test_players, guild_id=TEST_GUILD_ID)

        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1001, result="radiant", is_admin=False)

        # Same user tries to vote differently
        with pytest.raises(ValueError, match="already submitted"):
            admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1001, result="dire", is_admin=False)

    def test_user_can_revote_same_result(self, admin_match_service, voting_test_players):
        """Test that a user can submit the same vote again (no-op)."""
        admin_match_service.shuffle_players(voting_test_players, guild_id=TEST_GUILD_ID)

        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1001, result="radiant", is_admin=False)
        # Same vote again - should not raise, just update
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1001, result="radiant", is_admin=False)

        counts = admin_match_service.get_vote_counts(TEST_GUILD_ID)
        assert counts == {"radiant": 1, "dire": 0}  # Still just 1 vote

    def test_submission_returns_vote_counts(self, admin_match_service, voting_test_players):
        """Test that add_record_submission returns current vote counts."""
        admin_match_service.shuffle_players(voting_test_players, guild_id=TEST_GUILD_ID)

        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1001, result="radiant", is_admin=False)
        submission = admin_match_service.add_record_submission(
            TEST_GUILD_ID, user_id=1002, result="dire", is_admin=False
        )

        assert "vote_counts" in submission
        assert submission["vote_counts"] == {"radiant": 1, "dire": 1}

    def test_first_to_3_records_correct_winner(self, admin_match_service, voting_test_players):
        """Test that the match is recorded with the correct winner."""
        admin_match_service.shuffle_players(voting_test_players, guild_id=TEST_GUILD_ID)

        # Radiant gets 3 votes, Dire gets 2
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1001, result="dire", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1002, result="radiant", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1003, result="dire", is_admin=False)
        admin_match_service.add_record_submission(TEST_GUILD_ID, user_id=1004, result="radiant", is_admin=False)
        submission = admin_match_service.add_record_submission(
            TEST_GUILD_ID, user_id=1005, result="radiant", is_admin=False
        )

        assert submission["is_ready"] is True
        assert submission["result"] == "radiant"

        # Record the match
        record_result = admin_match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

        assert record_result["winning_team"] == "radiant"
        assert record_result["match_id"] is not None


class TestAbortVoting:
    """Test abort submission handling for match recording."""

    def test_non_admin_abort_requires_three_votes(self, admin_match_service, abort_test_players):
        admin_match_service.shuffle_players(abort_test_players, guild_id=TEST_GUILD_ID)
        assert admin_match_service.can_abort_match(TEST_GUILD_ID) is False

        admin_match_service.add_abort_submission(TEST_GUILD_ID, user_id=1001, is_admin=False)
        admin_match_service.add_abort_submission(TEST_GUILD_ID, user_id=1002, is_admin=False)

        assert admin_match_service.can_abort_match(TEST_GUILD_ID) is False
        submission = admin_match_service.add_abort_submission(TEST_GUILD_ID, user_id=1003, is_admin=False)
        assert submission["is_ready"] is True
        assert admin_match_service.can_abort_match(TEST_GUILD_ID) is True

    def test_admin_abort_overrides_minimum(self, admin_match_service, abort_test_players):
        admin_match_service.shuffle_players(abort_test_players, guild_id=TEST_GUILD_ID)
        submission = admin_match_service.add_abort_submission(TEST_GUILD_ID, user_id=9999, is_admin=True)

        assert submission["is_ready"] is True
        assert admin_match_service.can_abort_match(TEST_GUILD_ID) is True
        assert submission["non_admin_count"] == admin_match_service.get_abort_submission_count(TEST_GUILD_ID)

    def test_clear_abort_state_after_abort(self, admin_match_service, abort_test_players):
        admin_match_service.shuffle_players(abort_test_players, guild_id=TEST_GUILD_ID)
        admin_match_service.add_abort_submission(TEST_GUILD_ID, user_id=1001, is_admin=False)
        admin_match_service.add_abort_submission(TEST_GUILD_ID, user_id=1002, is_admin=False)
        admin_match_service.add_abort_submission(TEST_GUILD_ID, user_id=1003, is_admin=False)
        assert admin_match_service.can_abort_match(TEST_GUILD_ID) is True

        admin_match_service.clear_last_shuffle(TEST_GUILD_ID)
        assert admin_match_service.can_abort_match(TEST_GUILD_ID) is False


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
