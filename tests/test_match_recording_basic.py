"""
Basic unit tests for match recording functionality.
Tests the critical bug fix where wins/losses were incorrectly recorded.
"""

import pytest

from database import Database


class TestMatchRecording:
    """Test match recording and win/loss tracking."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    @pytest.fixture
    def test_players(self, test_db):
        """Create test players in the database."""
        player_ids = [1001, 1002, 1003, 1004, 1005, 1006, 1007, 1008, 1009, 1010]
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

    def test_record_match_team1_wins(self, test_db, test_players):
        """Test recording a match where team 1 wins."""
        team1_ids = test_players[:5]  # First 5 players
        team2_ids = test_players[5:]  # Last 5 players

        # Record match - team 1 wins
        match_id = test_db.record_match(team1_ids=team1_ids, team2_ids=team2_ids, winning_team=1)

        assert match_id is not None

        # Check win/loss counts
        for pid in team1_ids:
            player = test_db.get_player(pid)
            assert player.wins == 1
            assert player.losses == 0

        for pid in team2_ids:
            player = test_db.get_player(pid)
            assert player.wins == 0
            assert player.losses == 1

    def test_record_match_team2_wins(self, test_db, test_players):
        """Test recording a match where team 2 wins."""
        team1_ids = test_players[:5]
        team2_ids = test_players[5:]

        # Record match - team 2 wins
        match_id = test_db.record_match(team1_ids=team1_ids, team2_ids=team2_ids, winning_team=2)

        assert match_id is not None

        # Check win/loss counts
        for pid in team1_ids:
            player = test_db.get_player(pid)
            assert player.wins == 0
            assert player.losses == 1

        for pid in team2_ids:
            player = test_db.get_player(pid)
            assert player.wins == 1
            assert player.losses == 0

    def test_record_multiple_matches(self, test_db, test_players):
        """Test recording multiple matches and accumulating wins/losses."""
        team1_ids = test_players[:5]
        team2_ids = test_players[5:]

        # Record 3 matches - team 1 wins first two, team 2 wins third
        test_db.record_match(team1_ids, team2_ids, winning_team=1)
        test_db.record_match(team1_ids, team2_ids, winning_team=1)
        test_db.record_match(team1_ids, team2_ids, winning_team=2)

        # Check accumulated stats
        for pid in team1_ids:
            player = test_db.get_player(pid)
            assert player.wins == 2
            assert player.losses == 1

        for pid in team2_ids:
            player = test_db.get_player(pid)
            assert player.wins == 1
            assert player.losses == 2

    def test_record_match_participants(self, test_db, test_players):
        """Test that match participants are correctly recorded."""
        team1_ids = test_players[:5]
        team2_ids = test_players[5:]

        match_id = test_db.record_match(team1_ids=team1_ids, team2_ids=team2_ids, winning_team=1)

        # Check participants in database
        conn = test_db.get_connection()
        cursor = conn.cursor()

        # Check team 1 participants
        cursor.execute(
            """
            SELECT discord_id, team_number, won
            FROM match_participants
            WHERE match_id = ? AND team_number = 1
        """,
            (match_id,),
        )
        team1_participants = cursor.fetchall()
        assert len(team1_participants) == 5
        for pid, team_num, won in team1_participants:
            assert pid in team1_ids
            assert team_num == 1
            assert won == 1  # Team 1 won

        # Check team 2 participants
        cursor.execute(
            """
            SELECT discord_id, team_number, won
            FROM match_participants
            WHERE match_id = ? AND team_number = 2
        """,
            (match_id,),
        )
        team2_participants = cursor.fetchall()
        assert len(team2_participants) == 5
        for pid, team_num, won in team2_participants:
            assert pid in team2_ids
            assert team_num == 2
            assert won == 0  # Team 2 lost

        conn.close()


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
