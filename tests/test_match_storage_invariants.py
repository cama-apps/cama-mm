"""
Tests for the new match storage invariants:
- team1_players = Radiant, team2_players = Dire
- winning_team = 1 (Radiant won) or 2 (Dire won)
- match_participants.side is populated for all participants
"""

import pytest

from database import Database
from repositories.match_repository import MatchRepository
from repositories.player_repository import PlayerRepository
from services.match_service import MatchService

TEST_GUILD_ID = 123


class TestMatchStorageInvariants:
    """Test the Radiant/Dire storage invariants."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    @pytest.fixture
    def test_players(self, test_db):
        """Create 10 test players in the database."""
        player_ids = list(range(7001, 7011))
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

    def test_radiant_wins_stores_winning_team_1(self, test_db, test_players):
        """Test that Radiant winning stores winning_team=1."""
        radiant_ids = test_players[:5]
        dire_ids = test_players[5:]

        match_id = test_db.record_match(
            radiant_team_ids=radiant_ids,
            dire_team_ids=dire_ids,
            winning_team="radiant",
        )

        conn = test_db.get_connection()
        cursor = conn.cursor()
        cursor.execute(
            "SELECT team1_players, team2_players, winning_team FROM matches WHERE match_id = ?",
            (match_id,),
        )
        row = cursor.fetchone()
        conn.close()

        import json

        assert json.loads(row["team1_players"]) == radiant_ids, "team1 should be Radiant"
        assert json.loads(row["team2_players"]) == dire_ids, "team2 should be Dire"
        assert row["winning_team"] == 1, "Radiant winning should store winning_team=1"

    def test_dire_wins_stores_winning_team_2(self, test_db, test_players):
        """Test that Dire winning stores winning_team=2."""
        radiant_ids = test_players[:5]
        dire_ids = test_players[5:]

        match_id = test_db.record_match(
            radiant_team_ids=radiant_ids,
            dire_team_ids=dire_ids,
            winning_team="dire",
        )

        conn = test_db.get_connection()
        cursor = conn.cursor()
        cursor.execute(
            "SELECT team1_players, team2_players, winning_team FROM matches WHERE match_id = ?",
            (match_id,),
        )
        row = cursor.fetchone()
        conn.close()

        import json

        assert json.loads(row["team1_players"]) == radiant_ids, "team1 should be Radiant"
        assert json.loads(row["team2_players"]) == dire_ids, "team2 should be Dire"
        assert row["winning_team"] == 2, "Dire winning should store winning_team=2"

    def test_match_participants_side_populated(self, test_db, test_players):
        """Test that match_participants.side is populated for all participants."""
        radiant_ids = test_players[:5]
        dire_ids = test_players[5:]

        match_id = test_db.record_match(
            radiant_team_ids=radiant_ids,
            dire_team_ids=dire_ids,
            winning_team="radiant",
        )

        conn = test_db.get_connection()
        cursor = conn.cursor()

        # Check Radiant players have side='radiant'
        for pid in radiant_ids:
            cursor.execute(
                "SELECT side FROM match_participants WHERE match_id = ? AND discord_id = ?",
                (match_id, pid),
            )
            row = cursor.fetchone()
            assert row is not None, f"Player {pid} should be in match_participants"
            assert row["side"] == "radiant", f"Player {pid} should have side='radiant'"

        # Check Dire players have side='dire'
        for pid in dire_ids:
            cursor.execute(
                "SELECT side FROM match_participants WHERE match_id = ? AND discord_id = ?",
                (match_id, pid),
            )
            row = cursor.fetchone()
            assert row is not None, f"Player {pid} should be in match_participants"
            assert row["side"] == "dire", f"Player {pid} should have side='dire'"

        conn.close()

    def test_wins_losses_correct_for_radiant_win(self, test_db, test_players):
        """Test that wins/losses are correct when Radiant wins."""
        radiant_ids = test_players[:5]
        dire_ids = test_players[5:]

        test_db.record_match(
            radiant_team_ids=radiant_ids,
            dire_team_ids=dire_ids,
            winning_team="radiant",
        )

        # Radiant players should have wins
        for pid in radiant_ids:
            player = test_db.get_player(pid)
            assert player.wins == 1, f"Radiant player {pid} should have 1 win"
            assert player.losses == 0, f"Radiant player {pid} should have 0 losses"

        # Dire players should have losses
        for pid in dire_ids:
            player = test_db.get_player(pid)
            assert player.wins == 0, f"Dire player {pid} should have 0 wins"
            assert player.losses == 1, f"Dire player {pid} should have 1 loss"

    def test_wins_losses_correct_for_dire_win(self, test_db, test_players):
        """Test that wins/losses are correct when Dire wins."""
        radiant_ids = test_players[:5]
        dire_ids = test_players[5:]

        test_db.record_match(
            radiant_team_ids=radiant_ids,
            dire_team_ids=dire_ids,
            winning_team="dire",
        )

        # Dire players should have wins
        for pid in dire_ids:
            player = test_db.get_player(pid)
            assert player.wins == 1, f"Dire player {pid} should have 1 win"
            assert player.losses == 0, f"Dire player {pid} should have 0 losses"

        # Radiant players should have losses
        for pid in radiant_ids:
            player = test_db.get_player(pid)
            assert player.wins == 0, f"Radiant player {pid} should have 0 wins"
            assert player.losses == 1, f"Radiant player {pid} should have 1 loss"


class TestConcurrencyGuard:
    """Test the concurrency guard for match recording."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    @pytest.fixture
    def player_repo(self, test_db):
        """Create a PlayerRepository instance."""
        return PlayerRepository(test_db.db_path)

    @pytest.fixture
    def test_players(self, test_db, player_repo):
        """Create 10 test players in the database."""
        player_ids = list(range(8001, 8011))
        for pid in player_ids:
            player_repo.add(
                discord_id=pid,
                discord_username=f"Player{pid}",
                guild_id=TEST_GUILD_ID,
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
                preferred_roles=["1", "2", "3", "4", "5"],
            )
        return player_ids

    def test_double_record_fails(self, test_db, player_repo, test_players):
        """Test that attempting to record twice fails."""
        match_repo = MatchRepository(test_db.db_path)
        match_service = MatchService(
            player_repo=player_repo, match_repo=match_repo, use_glicko=True
        )

        # Shuffle to create pending match
        match_service.shuffle_players(test_players, guild_id=TEST_GUILD_ID)

        # First record should succeed
        result = match_service.record_match("radiant", guild_id=TEST_GUILD_ID)
        assert result["match_id"] is not None

        # Second record should fail (no pending match)
        with pytest.raises(ValueError, match="No recent shuffle found"):
            match_service.record_match("radiant", guild_id=TEST_GUILD_ID)

    def test_consume_pending_match_atomic(self, test_db):
        """Test that consume_pending_match is atomic."""
        # Save a pending match
        test_db.save_pending_match(123, {"test": "data"})

        # First consume should return the data
        result1 = test_db.consume_pending_match(123)
        assert result1 == {"test": "data"}

        # Second consume should return None
        result2 = test_db.consume_pending_match(123)
        assert result2 is None


class TestOldApiCompatibility:
    """Test backward compatibility with old team1/team2 API."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    @pytest.fixture
    def test_players(self, test_db):
        """Create 10 test players in the database."""
        player_ids = list(range(9001, 9011))
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

    def test_old_api_team1_wins(self, test_db, test_players):
        """Test old API with team1 winning."""
        team1_ids = test_players[:5]
        team2_ids = test_players[5:]

        test_db.record_match(
            team1_ids=team1_ids,
            team2_ids=team2_ids,
            winning_team=1,
        )

        # Team1 (treated as Radiant) should have wins
        for pid in team1_ids:
            player = test_db.get_player(pid)
            assert player.wins == 1, f"Team1 player {pid} should have 1 win"
            assert player.losses == 0, f"Team1 player {pid} should have 0 losses"

        # Team2 (treated as Dire) should have losses
        for pid in team2_ids:
            player = test_db.get_player(pid)
            assert player.wins == 0, f"Team2 player {pid} should have 0 wins"
            assert player.losses == 1, f"Team2 player {pid} should have 1 loss"

    def test_old_api_team2_wins(self, test_db, test_players):
        """Test old API with team2 winning."""
        team1_ids = test_players[:5]
        team2_ids = test_players[5:]

        test_db.record_match(
            team1_ids=team1_ids,
            team2_ids=team2_ids,
            winning_team=2,
        )

        # Team1 (treated as Radiant) should have losses
        for pid in team1_ids:
            player = test_db.get_player(pid)
            assert player.wins == 0, f"Team1 player {pid} should have 0 wins"
            assert player.losses == 1, f"Team1 player {pid} should have 1 loss"

        # Team2 (treated as Dire) should have wins
        for pid in team2_ids:
            player = test_db.get_player(pid)
            assert player.wins == 1, f"Team2 player {pid} should have 1 win"
            assert player.losses == 0, f"Team2 player {pid} should have 0 losses"


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
