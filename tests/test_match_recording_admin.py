"""
Tests for admin override functionality in match recording.
"""

import pytest
import os
import tempfile
import time

from database import Database
from repositories.player_repository import PlayerRepository
from repositories.match_repository import MatchRepository
from services.match_service import MatchService


class TestAdminOverride:
    """Test admin override functionality for match recording."""
    
    @pytest.fixture
    def test_db(self):
        """Create a temporary test database."""
        fd, db_path = tempfile.mkstemp(suffix='.db')
        os.close(fd)
        db = Database(db_path)
        yield db
        try:
            import sqlite3
            sqlite3.connect(db_path).close()
        except:
            pass
        time.sleep(0.1)
        try:
            os.unlink(db_path)
        except PermissionError:
            time.sleep(0.2)
            try:
                os.unlink(db_path)
            except:
                pass
    
    @pytest.fixture
    def test_players(self, test_db):
        """Create 10 test players in the database."""
        player_ids = [5001, 5002, 5003, 5004, 5005, 5006, 5007, 5008, 5009, 5010]
        for pid in player_ids:
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06
            )
        return player_ids
    
    @pytest.fixture
    def match_service(self, test_db):
        """Create a MatchService instance."""
        player_repo = PlayerRepository(test_db.db_path)
        match_repo = MatchRepository(test_db.db_path)
        return MatchService(player_repo=player_repo, match_repo=match_repo, use_glicko=True)
    
    def test_has_admin_submission_with_no_submissions(self, match_service, test_db, test_players):
        """Test has_admin_submission returns False when no submissions exist."""
        match_service.shuffle_players(test_players, guild_id=123)
        
        assert match_service.has_admin_submission(123) is False
    
    def test_has_admin_submission_with_non_admin_submission(self, match_service, test_db, test_players):
        """Test has_admin_submission returns False when only non-admin submits."""
        match_service.shuffle_players(test_players, guild_id=123)
        match_service.add_record_submission(123, user_id=1001, result="radiant", is_admin=False)
        
        assert match_service.has_admin_submission(123) is False
    
    def test_has_admin_submission_with_admin_submission(self, match_service, test_db, test_players):
        """Test has_admin_submission returns True when admin submits."""
        match_service.shuffle_players(test_players, guild_id=123)
        match_service.add_record_submission(123, user_id=9999, result="radiant", is_admin=True)
        
        assert match_service.has_admin_submission(123) is True
    
    def test_can_record_match_with_admin_override(self, match_service, test_db, test_players):
        """Test can_record_match returns True with admin override, bypassing non-admin requirement."""
        match_service.shuffle_players(test_players, guild_id=123)
        
        # Admin submits - should bypass the 3 non-admin requirement
        match_service.add_record_submission(123, user_id=9999, result="radiant", is_admin=True)
        
        # Should be ready to record even though non_admin_count is 0
        assert match_service.can_record_match(123) is True
        assert match_service.get_non_admin_submission_count(123) == 0
    
    def test_can_record_match_without_admin_requires_3_non_admin(self, match_service, test_db, test_players):
        """Test can_record_match requires 3 non-admin submissions when no admin submits."""
        match_service.shuffle_players(test_players, guild_id=123)
        
        # Add 2 non-admin submissions - should not be ready
        match_service.add_record_submission(123, user_id=1001, result="radiant", is_admin=False)
        match_service.add_record_submission(123, user_id=1002, result="radiant", is_admin=False)
        
        assert match_service.can_record_match(123) is False
        assert match_service.get_non_admin_submission_count(123) == 2
        
        # Add 3rd non-admin submission - should be ready
        match_service.add_record_submission(123, user_id=1003, result="radiant", is_admin=False)
        
        assert match_service.can_record_match(123) is True
        assert match_service.get_non_admin_submission_count(123) == 3
    
    def test_admin_override_allows_immediate_recording(self, match_service, test_db, test_players):
        """Test that admin submission allows immediate match recording."""
        match_service.shuffle_players(test_players, guild_id=123)
        
        # Admin submits - should allow immediate recording
        submission = match_service.add_record_submission(123, user_id=9999, result="radiant", is_admin=True)
        
        assert submission["is_ready"] is True
        assert submission["non_admin_count"] == 0
        assert match_service.can_record_match(123) is True
        
        # Should be able to record match immediately
        record_result = match_service.record_match("radiant", guild_id=123)
        
        assert record_result["match_id"] is not None
        assert record_result["winning_team"] == "radiant"
        assert record_result["updated_count"] == 10
    
    def test_admin_override_with_mixed_submissions(self, match_service, test_db, test_players):
        """Test admin override works even when non-admin submissions exist."""
        match_service.shuffle_players(test_players, guild_id=123)
        
        # Add 1 non-admin submission (not enough)
        match_service.add_record_submission(123, user_id=1001, result="radiant", is_admin=False)
        assert match_service.can_record_match(123) is False
        
        # Admin submits - should override and allow recording
        submission = match_service.add_record_submission(123, user_id=9999, result="radiant", is_admin=True)
        
        assert submission["is_ready"] is True
        assert submission["non_admin_count"] == 1  # Still only 1 non-admin
        assert match_service.can_record_match(123) is True
        
        # Should be able to record
        record_result = match_service.record_match("radiant", guild_id=123)
        assert record_result["match_id"] is not None
    
    def test_admin_override_clears_state_after_recording(self, match_service, test_db, test_players):
        """Test that state is cleared after admin override recording."""
        match_service.shuffle_players(test_players, guild_id=123)
        
        # Admin submits and records
        match_service.add_record_submission(123, user_id=9999, result="radiant", is_admin=True)
        match_service.record_match("radiant", guild_id=123)
        
        # State should be cleared
        assert match_service.get_last_shuffle(123) is None
        assert match_service.can_record_match(123) is False
        assert match_service.has_admin_submission(123) is False


class TestFirstToThreeVoting:
    """Test first-to-3 voting system for non-admin match recording."""
    
    @pytest.fixture
    def test_db(self):
        """Create a temporary test database."""
        fd, db_path = tempfile.mkstemp(suffix='.db')
        os.close(fd)
        db = Database(db_path)
        yield db
        try:
            import sqlite3
            sqlite3.connect(db_path).close()
        except:
            pass
        time.sleep(0.1)
        try:
            os.unlink(db_path)
        except PermissionError:
            time.sleep(0.2)
            try:
                os.unlink(db_path)
            except:
                pass
    
    @pytest.fixture
    def test_players(self, test_db):
        """Create 10 test players in the database."""
        player_ids = [6001, 6002, 6003, 6004, 6005, 6006, 6007, 6008, 6009, 6010]
        for pid in player_ids:
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06
            )
        return player_ids
    
    @pytest.fixture
    def match_service(self, test_db):
        """Create a MatchService instance."""
        player_repo = PlayerRepository(test_db.db_path)
        match_repo = MatchRepository(test_db.db_path)
        return MatchService(player_repo=player_repo, match_repo=match_repo, use_glicko=True)
    
    def test_get_vote_counts_empty(self, match_service, test_db, test_players):
        """Test get_vote_counts returns zeros when no submissions."""
        match_service.shuffle_players(test_players, guild_id=123)
        
        counts = match_service.get_vote_counts(123)
        assert counts == {"radiant": 0, "dire": 0}
    
    def test_get_vote_counts_tracks_votes(self, match_service, test_db, test_players):
        """Test get_vote_counts correctly tracks non-admin votes."""
        match_service.shuffle_players(test_players, guild_id=123)
        
        # Add some votes
        match_service.add_record_submission(123, user_id=1001, result="radiant", is_admin=False)
        match_service.add_record_submission(123, user_id=1002, result="dire", is_admin=False)
        match_service.add_record_submission(123, user_id=1003, result="radiant", is_admin=False)
        
        counts = match_service.get_vote_counts(123)
        assert counts == {"radiant": 2, "dire": 1}
    
    def test_get_vote_counts_excludes_admin(self, match_service, test_db, test_players):
        """Test get_vote_counts does not count admin votes."""
        match_service.shuffle_players(test_players, guild_id=123)
        
        # Add admin and non-admin votes
        match_service.add_record_submission(123, user_id=9999, result="radiant", is_admin=True)
        match_service.add_record_submission(123, user_id=1001, result="radiant", is_admin=False)
        
        counts = match_service.get_vote_counts(123)
        assert counts == {"radiant": 1, "dire": 0}
    
    def test_conflicting_votes_allowed(self, match_service, test_db, test_players):
        """Test that users can vote for different results (requires MIN_NON_ADMIN_SUBMISSIONS to confirm)."""
        match_service.shuffle_players(test_players, guild_id=123)
        
        # Add conflicting votes - should not raise
        match_service.add_record_submission(123, user_id=1001, result="radiant", is_admin=False)
        match_service.add_record_submission(123, user_id=1002, result="dire", is_admin=False)
        match_service.add_record_submission(123, user_id=1003, result="radiant", is_admin=False)
        
        counts = match_service.get_vote_counts(123)
        assert counts == {"radiant": 2, "dire": 1}
    
    def test_first_to_3_radiant_wins(self, match_service, test_db, test_players):
        """Test that radiant wins when it reaches 3 votes first."""
        match_service.shuffle_players(test_players, guild_id=123)
        
        # 2 radiant, 2 dire - not ready
        match_service.add_record_submission(123, user_id=1001, result="radiant", is_admin=False)
        match_service.add_record_submission(123, user_id=1002, result="dire", is_admin=False)
        match_service.add_record_submission(123, user_id=1003, result="radiant", is_admin=False)
        match_service.add_record_submission(123, user_id=1004, result="dire", is_admin=False)
        
        assert match_service.can_record_match(123) is False
        assert match_service.get_pending_record_result(123) is None
        
        # 3rd radiant vote - radiant wins!
        submission = match_service.add_record_submission(123, user_id=1005, result="radiant", is_admin=False)
        
        assert submission["is_ready"] is True
        assert submission["result"] == "radiant"
        assert match_service.can_record_match(123) is True
        assert match_service.get_pending_record_result(123) == "radiant"
    
    def test_first_to_3_dire_wins(self, match_service, test_db, test_players):
        """Test that dire wins when it reaches 3 votes first."""
        match_service.shuffle_players(test_players, guild_id=123)
        
        # 1 radiant, 2 dire
        match_service.add_record_submission(123, user_id=1001, result="radiant", is_admin=False)
        match_service.add_record_submission(123, user_id=1002, result="dire", is_admin=False)
        match_service.add_record_submission(123, user_id=1003, result="dire", is_admin=False)
        
        assert match_service.can_record_match(123) is False
        
        # 3rd dire vote - dire wins!
        submission = match_service.add_record_submission(123, user_id=1004, result="dire", is_admin=False)
        
        assert submission["is_ready"] is True
        assert submission["result"] == "dire"
        assert match_service.get_pending_record_result(123) == "dire"
    
    def test_user_cannot_change_vote(self, match_service, test_db, test_players):
        """Test that a user cannot change their vote."""
        match_service.shuffle_players(test_players, guild_id=123)
        
        match_service.add_record_submission(123, user_id=1001, result="radiant", is_admin=False)
        
        # Same user tries to vote differently
        with pytest.raises(ValueError, match="already submitted"):
            match_service.add_record_submission(123, user_id=1001, result="dire", is_admin=False)
    
    def test_user_can_revote_same_result(self, match_service, test_db, test_players):
        """Test that a user can submit the same vote again (no-op)."""
        match_service.shuffle_players(test_players, guild_id=123)
        
        match_service.add_record_submission(123, user_id=1001, result="radiant", is_admin=False)
        # Same vote again - should not raise, just update
        match_service.add_record_submission(123, user_id=1001, result="radiant", is_admin=False)
        
        counts = match_service.get_vote_counts(123)
        assert counts == {"radiant": 1, "dire": 0}  # Still just 1 vote
    
    def test_submission_returns_vote_counts(self, match_service, test_db, test_players):
        """Test that add_record_submission returns current vote counts."""
        match_service.shuffle_players(test_players, guild_id=123)
        
        match_service.add_record_submission(123, user_id=1001, result="radiant", is_admin=False)
        submission = match_service.add_record_submission(123, user_id=1002, result="dire", is_admin=False)
        
        assert "vote_counts" in submission
        assert submission["vote_counts"] == {"radiant": 1, "dire": 1}
    
    def test_first_to_3_records_correct_winner(self, match_service, test_db, test_players):
        """Test that the match is recorded with the correct winner."""
        match_service.shuffle_players(test_players, guild_id=123)
        
        # Radiant gets 3 votes, Dire gets 2
        match_service.add_record_submission(123, user_id=1001, result="dire", is_admin=False)
        match_service.add_record_submission(123, user_id=1002, result="radiant", is_admin=False)
        match_service.add_record_submission(123, user_id=1003, result="dire", is_admin=False)
        match_service.add_record_submission(123, user_id=1004, result="radiant", is_admin=False)
        submission = match_service.add_record_submission(123, user_id=1005, result="radiant", is_admin=False)
        
        assert submission["is_ready"] is True
        assert submission["result"] == "radiant"
        
        # Record the match
        record_result = match_service.record_match("radiant", guild_id=123)
        
        assert record_result["winning_team"] == "radiant"
        assert record_result["match_id"] is not None


class TestAbortVoting:
    """Test abort submission handling for match recording."""

    @pytest.fixture
    def test_db(self):
        """Create a temporary test database."""
        fd, db_path = tempfile.mkstemp(suffix=".db")
        os.close(fd)
        db = Database(db_path)
        yield db
        try:
            import sqlite3

            sqlite3.connect(db_path).close()
        except Exception:
            pass
        time.sleep(0.1)
        try:
            os.unlink(db_path)
        except PermissionError:
            time.sleep(0.2)
            try:
                os.unlink(db_path)
            except Exception:
                pass

    @pytest.fixture
    def test_players(self, test_db):
        """Create 10 test players in the database."""
        player_ids = [7001, 7002, 7003, 7004, 7005, 7006, 7007, 7008, 7009, 7010]
        for pid in player_ids:
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )
        return player_ids

    @pytest.fixture
    def match_service(self, test_db):
        """Create a MatchService instance."""
        player_repo = PlayerRepository(test_db.db_path)
        match_repo = MatchRepository(test_db.db_path)
        return MatchService(player_repo=player_repo, match_repo=match_repo, use_glicko=True)

    def test_non_admin_abort_requires_three_votes(self, match_service, test_db, test_players):
        match_service.shuffle_players(test_players, guild_id=123)
        assert match_service.can_abort_match(123) is False

        match_service.add_abort_submission(123, user_id=1001, is_admin=False)
        match_service.add_abort_submission(123, user_id=1002, is_admin=False)

        assert match_service.can_abort_match(123) is False
        submission = match_service.add_abort_submission(123, user_id=1003, is_admin=False)
        assert submission["is_ready"] is True
        assert match_service.can_abort_match(123) is True

    def test_admin_abort_overrides_minimum(self, match_service, test_db, test_players):
        match_service.shuffle_players(test_players, guild_id=123)
        submission = match_service.add_abort_submission(123, user_id=9999, is_admin=True)

        assert submission["is_ready"] is True
        assert match_service.can_abort_match(123) is True
        assert submission["non_admin_count"] == match_service.get_abort_submission_count(123)

    def test_clear_abort_state_after_abort(self, match_service, test_db, test_players):
        match_service.shuffle_players(test_players, guild_id=123)
        match_service.add_abort_submission(123, user_id=1001, is_admin=False)
        match_service.add_abort_submission(123, user_id=1002, is_admin=False)
        match_service.add_abort_submission(123, user_id=1003, is_admin=False)
        assert match_service.can_abort_match(123) is True

        match_service.clear_last_shuffle(123)
        assert match_service.can_abort_match(123) is False


if __name__ == "__main__":
    pytest.main([__file__, "-v"])

