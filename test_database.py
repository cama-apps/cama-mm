"""
Unit tests for database operations.
"""

import os
import tempfile

import pytest

import remove_fake_users
from database import Database


class TestDatabase:
    """Test Database class functionality."""

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
        import time

        time.sleep(0.1)
        try:
            os.unlink(db_path)
        except PermissionError:
            time.sleep(0.2)
            try:
                os.unlink(db_path)
            except Exception:
                pass

    def test_add_player(self, test_db):
        """Test adding a player to the database."""
        player_id = 5001
        test_db.add_player(
            discord_id=player_id,
            discord_username="TestPlayer",
            initial_mmr=2000,
            glicko_rating=1800.0,
            glicko_rd=300.0,
            glicko_volatility=0.06,
        )

        player = test_db.get_player(player_id)
        assert player is not None
        assert player.name == "TestPlayer"
        assert player.mmr == 2000
        assert player.glicko_rating == 1800.0

    def test_get_player_not_found(self, test_db):
        """Test getting a player that doesn't exist."""
        player = test_db.get_player(99999)
        assert player is None

    def test_update_player_glicko_rating(self, test_db):
        """Test updating a player's Glicko-2 rating."""
        player_id = 5002
        test_db.add_player(
            discord_id=player_id,
            discord_username="TestPlayer2",
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )

        # Update rating
        test_db.update_player_glicko_rating(player_id, 1600.0, 300.0, 0.05)

        # Verify update
        rating, rd, vol = test_db.get_player_glicko_rating(player_id)
        assert rating == 1600.0
        assert rd == 300.0
        assert vol == 0.05

    def test_get_players_by_ids(self, test_db):
        """Test getting multiple players by their IDs."""
        player_ids = [6001, 6002, 6003]
        for pid in player_ids:
            test_db.add_player(discord_id=pid, discord_username=f"Player{pid}", initial_mmr=1500)

        players = test_db.get_players_by_ids(player_ids)
        assert len(players) == 3
        # Verify all players exist and have correct names
        player_names = {p.name for p in players}
        expected_names = {f"Player{pid}" for pid in player_ids}
        assert player_names == expected_names

    def test_get_players_by_ids_with_missing(self, test_db):
        """Test getting players when some IDs don't exist."""
        player_ids = [7001, 7002, 99999]  # 99999 doesn't exist
        test_db.add_player(discord_id=7001, discord_username="Player1", initial_mmr=1500)
        test_db.add_player(discord_id=7002, discord_username="Player2", initial_mmr=1500)

        players = test_db.get_players_by_ids(player_ids)
        # Should return only existing players
        assert len(players) == 2
        assert all(p.name in ["Player1", "Player2"] for p in players)

    def test_record_match_creates_match(self, test_db):
        """Test that record_match creates a match entry."""
        # Create players first
        team1_ids = [8001, 8002]
        team2_ids = [8003, 8004]

        for pid in team1_ids + team2_ids:
            test_db.add_player(discord_id=pid, discord_username=f"P{pid}", initial_mmr=1500)

        match_id = test_db.record_match(team1_ids=team1_ids, team2_ids=team2_ids, winning_team=1)

        # Verify match exists
        conn = test_db.get_connection()
        cursor = conn.cursor()
        cursor.execute("SELECT * FROM matches WHERE match_id = ?", (match_id,))
        match = cursor.fetchone()
        assert match is not None
        assert match[3] == 1  # winning_team column
        conn.close()

    def test_delete_player(self, test_db):
        """Test deleting a player."""
        player_id = 9001
        test_db.add_player(discord_id=player_id, discord_username="ToDelete", initial_mmr=1500)

        # Verify player exists
        assert test_db.get_player(player_id) is not None

        # Delete player
        result = test_db.delete_player(player_id)
        assert result is True

        # Verify player is gone
        assert test_db.get_player(player_id) is None

    def test_delete_fake_users_cascades_related_tables(self, test_db):
        """Deleting fake users removes related match, rating, and bet data."""
        # Real user should remain
        test_db.add_player(discord_id=100, discord_username="RealUser", initial_mmr=1500)
        # Fake users (negative IDs)
        for pid in (-1, -2):
            test_db.add_player(
                discord_id=pid, discord_username=f"FakeUser{abs(pid)}", initial_mmr=1500
            )

        with test_db.connection() as conn:
            cursor = conn.cursor()
            # Minimal match row for FK references
            cursor.execute(
                """
                INSERT INTO matches (team1_players, team2_players, winning_team)
                VALUES (?, ?, ?)
                """,
                ("[1]", "[2]", 1),
            )
            match_id = cursor.lastrowid
            # Related rows for fake users
            for pid in (-1, -2):
                cursor.execute(
                    """
                    INSERT INTO match_participants (match_id, discord_id, team_number, won, side)
                    VALUES (?, ?, ?, ?, ?)
                    """,
                    (match_id, pid, 1, True, "radiant"),
                )
            cursor.execute(
                "INSERT INTO rating_history (discord_id, rating, match_id) VALUES (?, ?, ?)",
                (-1, 1200, match_id),
            )
            cursor.execute(
                """
                INSERT INTO bets (guild_id, match_id, discord_id, team_bet_on, amount, bet_time)
                VALUES (?, ?, ?, ?, ?, ?)
                """,
                (0, None, -2, "radiant", 10, 0),
            )

        deleted = test_db.delete_fake_users()
        assert deleted == 2

        # Fake users and related rows gone
        with test_db.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT COUNT(*) FROM players WHERE discord_id < 0")
            assert cursor.fetchone()[0] == 0

            cursor.execute("SELECT COUNT(*) FROM match_participants WHERE discord_id < 0")
            assert cursor.fetchone()[0] == 0

            cursor.execute("SELECT COUNT(*) FROM rating_history WHERE discord_id < 0")
            assert cursor.fetchone()[0] == 0

            cursor.execute("SELECT COUNT(*) FROM bets WHERE discord_id < 0")
            assert cursor.fetchone()[0] == 0

        # Real user remains
        assert test_db.get_player(100) is not None

    def test_clear_all_players(self, test_db):
        """Test clearing all players."""
        # Add some players
        for pid in range(10001, 10006):
            test_db.add_player(discord_id=pid, discord_username=f"P{pid}", initial_mmr=1500)

        # Verify players exist
        assert test_db.get_player(10001) is not None

        # Clear all
        test_db.clear_all_players()

        # Verify all are gone
        assert test_db.get_player(10001) is None

    def test_remove_fake_users_script(self, tmp_path):
        """Script removes fake users and leaves real users untouched."""
        db_path = tmp_path / "temp.db"
        db = Database(str(db_path))

        db.add_player(discord_id=-1, discord_username="FakeUser1", initial_mmr=1500)
        db.add_player(discord_id=200, discord_username="RealUser", initial_mmr=1500)

        os.environ["DB_PATH"] = str(db_path)
        try:
            exit_code = remove_fake_users.main()
        finally:
            os.environ.pop("DB_PATH", None)

        assert exit_code == 0
        assert db.get_player(-1) is None
        assert db.get_player(200) is not None

    def test_get_exclusion_counts(self, test_db):
        """Test retrieving exclusion counts for multiple players."""
        player_ids = [11001, 11002, 11003]
        for pid in player_ids:
            test_db.add_player(discord_id=pid, discord_username=f"P{pid}", initial_mmr=1500)

        # Initially, all should have 0 exclusion count
        exclusion_counts = test_db.get_exclusion_counts(player_ids)
        assert len(exclusion_counts) == 3
        for pid in player_ids:
            assert exclusion_counts[pid] == 0

    def test_increment_exclusion_count(self, test_db):
        """Test incrementing a player's exclusion count."""
        player_id = 11101
        test_db.add_player(discord_id=player_id, discord_username="TestPlayer", initial_mmr=1500)

        # Increment exclusion count twice (4 per exclusion)
        test_db.increment_exclusion_count(player_id)

        # Verify count increased by 4
        exclusion_counts = test_db.get_exclusion_counts([player_id])
        assert exclusion_counts[player_id] == 4

        # Increment again
        test_db.increment_exclusion_count(player_id)
        exclusion_counts = test_db.get_exclusion_counts([player_id])
        assert exclusion_counts[player_id] == 8

    def test_decay_exclusion_count(self, test_db):
        """Test decaying a player's exclusion count (halves it)."""
        player_id = 11201
        test_db.add_player(discord_id=player_id, discord_username="TestPlayer", initial_mmr=1500)

        # Set exclusion count to 10 exclusions (count becomes 40)
        for _ in range(10):
            test_db.increment_exclusion_count(player_id)

        exclusion_counts = test_db.get_exclusion_counts([player_id])
        assert exclusion_counts[player_id] == 40

        # Decay (should become 20)
        test_db.decay_exclusion_count(player_id)
        exclusion_counts = test_db.get_exclusion_counts([player_id])
        assert exclusion_counts[player_id] == 20

        # Decay again (should become 10)
        test_db.decay_exclusion_count(player_id)
        exclusion_counts = test_db.get_exclusion_counts([player_id])
        assert exclusion_counts[player_id] == 10

        # Decay again (should become 5)
        test_db.decay_exclusion_count(player_id)
        exclusion_counts = test_db.get_exclusion_counts([player_id])
        assert exclusion_counts[player_id] == 5

        # Decay again (should become 2)
        test_db.decay_exclusion_count(player_id)
        exclusion_counts = test_db.get_exclusion_counts([player_id])
        assert exclusion_counts[player_id] == 2

        # Decay again (should become 1)
        test_db.decay_exclusion_count(player_id)
        exclusion_counts = test_db.get_exclusion_counts([player_id])
        assert exclusion_counts[player_id] == 1

        # Decay again (should become 0)
        test_db.decay_exclusion_count(player_id)
        exclusion_counts = test_db.get_exclusion_counts([player_id])
        assert exclusion_counts[player_id] == 0

    def test_exclusion_count_with_odd_numbers(self, test_db):
        """Test decay with odd numbers (should round down)."""
        player_id = 11301
        test_db.add_player(discord_id=player_id, discord_username="TestPlayer", initial_mmr=1500)

        # Set count to 7 exclusions (28 total)
        for _ in range(7):
            test_db.increment_exclusion_count(player_id)

        exclusion_counts = test_db.get_exclusion_counts([player_id])
        assert exclusion_counts[player_id] == 28

        # Decay: 28 / 2 = 14
        test_db.decay_exclusion_count(player_id)
        exclusion_counts = test_db.get_exclusion_counts([player_id])
        assert exclusion_counts[player_id] == 14


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
