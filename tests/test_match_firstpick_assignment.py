"""
Tests for firstpick team assignment in match shuffling.
"""

import os
import tempfile
import time

import pytest

from database import Database
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.match_service import MatchService


class TestFirstpickAssignment:
    """Test that firstpick team is randomly assigned between Radiant and Dire."""

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

    def test_firstpick_is_radiant_or_dire(self, match_service, test_db, test_players):
        """Test that firstpick team is always either 'Radiant' or 'Dire'."""
        result = match_service.shuffle_players(test_players, guild_id=123)

        assert "first_pick_team" in result
        assert result["first_pick_team"] in ("Radiant", "Dire")

    def test_firstpick_is_in_shuffle_result(self, match_service, test_db, test_players):
        """Test that firstpick team is included in shuffle result."""
        result = match_service.shuffle_players(test_players, guild_id=123)

        assert "first_pick_team" in result
        first_pick = result["first_pick_team"]
        assert first_pick == "Radiant" or first_pick == "Dire"

    def test_firstpick_is_persisted_in_state(self, match_service, test_db, test_players):
        """Test that firstpick team is persisted in match state."""
        result = match_service.shuffle_players(test_players, guild_id=123)

        # Get the persisted state
        state = match_service.get_last_shuffle(123)
        assert state is not None
        assert "first_pick_team" in state
        assert state["first_pick_team"] in ("Radiant", "Dire")
        assert state["first_pick_team"] == result["first_pick_team"]

    def test_firstpick_randomization_statistical(self, match_service, test_db, test_players):
        """
        Test that firstpick assignment is random by running multiple shuffles.
        
        This test performs multiple shuffles and verifies that both Radiant and Dire
        appear as firstpick at least once, indicating proper randomization.
        """
        firstpick_counts = {"Radiant": 0, "Dire": 0}
        num_runs = 50

        for _ in range(num_runs):
            result = match_service.shuffle_players(test_players, guild_id=123)
            first_pick = result["first_pick_team"]
            firstpick_counts[first_pick] += 1

        # Verify both values appear (with high probability, 50 runs should produce both)
        assert firstpick_counts["Radiant"] > 0, "Radiant should appear as firstpick at least once"
        assert firstpick_counts["Dire"] > 0, "Dire should appear as firstpick at least once"

        # Verify distribution is roughly balanced (not perfect, but should be close)
        # With 50 runs, we expect ~25 of each, so we check that neither is extremely skewed
        # (e.g., at least 10% of runs for each team, meaning at least 5 of 50)
        assert firstpick_counts["Radiant"] >= 5, "Radiant should appear reasonably often"
        assert firstpick_counts["Dire"] >= 5, "Dire should appear reasonably often"

    def test_firstpick_assignment_multiple_guilds(self, match_service, test_db, test_players):
        """Test that firstpick assignment works correctly for different guilds."""
        result1 = match_service.shuffle_players(test_players, guild_id=100)
        result2 = match_service.shuffle_players(test_players, guild_id=200)

        assert "first_pick_team" in result1
        assert "first_pick_team" in result2
        assert result1["first_pick_team"] in ("Radiant", "Dire")
        assert result2["first_pick_team"] in ("Radiant", "Dire")

        # Verify each guild has its own state with firstpick
        state1 = match_service.get_last_shuffle(100)
        state2 = match_service.get_last_shuffle(200)

        assert state1 is not None
        assert state2 is not None
        assert state1["first_pick_team"] == result1["first_pick_team"]
        assert state2["first_pick_team"] == result2["first_pick_team"]


class TestFirstpickEndToEnd:
    """End-to-end tests for firstpick assignment through the full workflow."""

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
        player_ids = [8001, 8002, 8003, 8004, 8005, 8006, 8007, 8008, 8009, 8010]
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

    def test_firstpick_persists_through_record_workflow(self, match_service, test_db, test_players):
        """
        Test that firstpick assignment persists through the shuffle and record workflow.
        
        This is an end-to-end test that verifies firstpick is assigned during shuffle,
        stored in state, and available until the match is recorded.
        """
        # Shuffle players
        result = match_service.shuffle_players(test_players, guild_id=999)

        # Verify firstpick is in the result
        assert "first_pick_team" in result
        first_pick = result["first_pick_team"]
        assert first_pick in ("Radiant", "Dire")

        # Verify firstpick is in the stored state
        state = match_service.get_last_shuffle(999)
        assert state is not None
        assert state["first_pick_team"] == first_pick

        # Record the match (state should be cleared after)
        match_service.record_match("radiant", guild_id=999)

        # Verify state is cleared
        assert match_service.get_last_shuffle(999) is None

    def test_firstpick_independent_of_radiant_dire_assignment(self, match_service, test_db, test_players):
        """
        Test that firstpick assignment is independent of which team is Radiant/Dire.
        
        This ensures that firstpick can be either Radiant or Dire regardless of
        which players are assigned to which team.
        """
        # Run multiple shuffles to get different team assignments
        firstpick_with_radiant_wins = 0
        firstpick_with_dire_wins = 0
        num_runs = 30

        for _ in range(num_runs):
            result = match_service.shuffle_players(test_players, guild_id=888)

            first_pick = result["first_pick_team"]
            assert first_pick in ("Radiant", "Dire")

            # Record the match to clear state for next iteration
            # Use firstpick to determine winner (just for testing)
            if first_pick == "Radiant":
                firstpick_with_radiant_wins += 1
                match_service.record_match("radiant", guild_id=888)
            else:
                firstpick_with_dire_wins += 1
                match_service.record_match("dire", guild_id=888)

        # Verify both firstpick options appeared
        assert firstpick_with_radiant_wins > 0
        assert firstpick_with_dire_wins > 0

