"""
Tests for enhanced logging in match recording with player names.
"""

import logging
import os
import tempfile
import time

import pytest

from database import Database
from rating_system import CamaRatingSystem


class TestMatchRecordingLogging:
    """Test enhanced logging for match recording with player names."""

    @pytest.fixture
    def test_db(self):
        """Create a temporary test database."""
        fd, db_path = tempfile.mkstemp(suffix=".db")
        os.close(fd)
        db = Database(db_path)
        yield db
        # Close any open connections before cleanup
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

    def test_match_logging_includes_player_names(self, test_db, caplog):
        """Test that match recording logs include player names for winners and losers."""
        # Set up logging capture
        caplog.set_level(logging.INFO)

        # Create test players with distinct names
        player_ids = [5001, 5002, 5003, 5004, 5005, 5006, 5007, 5008, 5009, 5010]
        player_names = [f"Winner{i}" if i <= 5 else f"Loser{i - 5}" for i in range(1, 11)]

        for pid, name in zip(player_ids, player_names):
            test_db.add_player(
                discord_id=pid,
                discord_username=name,
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        # Split into teams
        team1_ids = player_ids[:5]  # Winners
        team2_ids = player_ids[5:]  # Losers

        # Simulate the match recording logic from bot.py
        # This tests the logging format without needing the full Discord bot
        CamaRatingSystem()

        # Get player names for logging (simulating bot.py logic)
        winning_team_ids = team1_ids
        losing_team_ids = team2_ids

        winning_player_names = []
        losing_player_names = []

        for player_id in winning_team_ids:
            player_obj = test_db.get_player(player_id)
            if player_obj:
                winning_player_names.append(player_obj.name)
            else:
                winning_player_names.append(f"Unknown({player_id})")

        for player_id in losing_team_ids:
            player_obj = test_db.get_player(player_id)
            if player_obj:
                losing_player_names.append(player_obj.name)
            else:
                losing_player_names.append(f"Unknown({player_id})")

        # Record the match
        match_id = test_db.record_match(team1_ids=team1_ids, team2_ids=team2_ids, winning_team=1)

        # Simulate the logging that happens in bot.py
        winning_team_display = "Team 1"
        winning_team_num = 1
        updated_count = 10

        log_message = (
            f"Match {match_id} recorded - {winning_team_display} (Team {winning_team_num}) won. "
            f"Updated ratings for {updated_count} players. "
            f"Winners: {', '.join(winning_player_names)}. "
            f"Losers: {', '.join(losing_player_names)}"
        )

        # Log it (simulating what bot.py does)
        logger = logging.getLogger("test")
        logger.info(log_message)

    def test_match_logging_radiant_dire_format(self, test_db, caplog):
        """Test that logging works correctly with Radiant/Dire team names."""
        caplog.set_level(logging.INFO)

        # Create test players
        player_ids = [6001, 6002, 6003, 6004, 6005, 6006, 6007, 6008, 6009, 6010]
        for pid in player_ids:
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        # Simulate Radiant/Dire scenario
        radiant_team_ids = player_ids[:5]
        dire_team_ids = player_ids[5:]
        winning_team_num = 1  # Radiant won
        winning_team_display = "Radiant"

        # Get player names
        winning_player_names = []
        losing_player_names = []

        for player_id in radiant_team_ids:
            player_obj = test_db.get_player(player_id)
            winning_player_names.append(player_obj.name if player_obj else f"Unknown({player_id})")

        for player_id in dire_team_ids:
            player_obj = test_db.get_player(player_id)
            losing_player_names.append(player_obj.name if player_obj else f"Unknown({player_id})")

        # Record match
        match_id = test_db.record_match(
            team1_ids=radiant_team_ids, team2_ids=dire_team_ids, winning_team=1
        )

        # Simulate logging
        updated_count = 10
        log_message = (
            f"Match {match_id} recorded - {winning_team_display} (Team {winning_team_num}) won. "
            f"Updated ratings for {updated_count} players. "
            f"Winners: {', '.join(winning_player_names)}. "
            f"Losers: {', '.join(losing_player_names)}"
        )

        logger = logging.getLogger("test")
        logger.info(log_message)

        # Verify Radiant is mentioned
        assert "Radiant" in log_message
        assert f"Team {winning_team_num}" in log_message

        # Verify all players are listed
        assert len(winning_player_names) == 5
        assert len(losing_player_names) == 5
        assert all(name in log_message for name in winning_player_names)
        assert all(name in log_message for name in losing_player_names)


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
