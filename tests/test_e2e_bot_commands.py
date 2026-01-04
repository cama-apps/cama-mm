"""
End-to-end tests for bot commands (database operations).
"""

import json
import os
import tempfile
import time

import pytest

from database import Database


class TestEndToEndBotCommands:
    """Test bot commands in isolation (mocked Discord interactions)."""

    @pytest.fixture
    def test_db(self):
        """Create a temporary test database."""
        fd, db_path = tempfile.mkstemp(suffix=".db")
        os.close(fd)
        db = Database(db_path)
        yield db
        # Cleanup
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

    def test_register_command_flow(self, test_db):
        """Test the register command flow (database operations only)."""
        # Test the underlying database operations that the register command uses
        user_id = 1001

        # Simulate checking if player exists (register command does this first)
        player = test_db.get_player(user_id)
        assert player is None  # Not registered yet

        # Simulate adding player (what register command does after OpenDota fetch)
        test_db.add_player(
            discord_id=user_id,
            discord_username="TestUser",
            initial_mmr=2000,
            glicko_rating=1800.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )

        # Verify player was added
        player = test_db.get_player(user_id)
        assert player is not None
        assert player.name == "TestUser"
        assert player.mmr == 2000

        # Test duplicate registration check
        # The register command checks this before adding
        existing_player = test_db.get_player(user_id)
        assert existing_player is not None  # Would prevent re-registration

    def test_database_operations_through_workflow(self, test_db):
        """Test that database operations work correctly through the workflow."""
        # Register player
        user_id = 80001
        test_db.add_player(
            discord_id=user_id,
            discord_username="TestUser",
            initial_mmr=2000,
            glicko_rating=1800.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )

        # Update roles
        conn = test_db.get_connection()
        cursor = conn.cursor()
        cursor.execute(
            """
            UPDATE players
            SET preferred_roles = ?, updated_at = CURRENT_TIMESTAMP
            WHERE discord_id = ?
        """,
            (json.dumps(["1", "2"]), user_id),
        )
        conn.commit()
        conn.close()

        # Verify
        player = test_db.get_player(user_id)
        assert player.preferred_roles == ["1", "2"]

        # Record a match (simplified - just one player)
        # In real scenario, we'd have 10 players
        team1_ids = [user_id]
        team2_ids = [80002]  # Another player

        # Add second player
        test_db.add_player(discord_id=80002, discord_username="TestUser2", initial_mmr=2000)

        # Record match
        match_id = test_db.record_match(team1_ids=team1_ids, team2_ids=team2_ids, winning_team=1)

        assert match_id is not None

        # Verify match recorded
        player = test_db.get_player(user_id)
        assert player.wins == 1
        assert player.losses == 0


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
