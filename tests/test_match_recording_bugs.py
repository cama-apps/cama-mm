"""
Tests for critical bugs in match recording.
Tests the bug where excluded players were getting losses recorded,
and the player order preservation bug.
"""

import pytest
import os
import tempfile
import time

from database import Database


class TestExcludedPlayersBug:
    """Test the critical bug where excluded players were getting losses recorded."""
    
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
    
    def test_excluded_player_should_not_get_loss(self, test_db):
        """
        Test the exact bug scenario: player was excluded from match but got a loss.
        
        Scenario:
        - 11 players in lobby
        - 10 players selected for match, 1 excluded (BugReporter)
        - Match recorded with Dire won
        - Bug: BugReporter (excluded) got a loss
        - Expected: BugReporter should have 0 wins, 0 losses (not in match)
        """
        # Create 11 players (10 for match + 1 excluded)
        player_names = [
            "FakeUser172699",
            "FakeUser169817",
            "FakeUser167858",
            "FakeUser175544",
            "FakeUser173967",
            "FakeUser170233",
            "FakeUser174788",
            "FakeUser171621",
            "FakeUser166664",
            "FakeUser168472",
            "BugReporter",  # This player should be excluded
        ]
        
        player_ids = []
        for idx, name in enumerate(player_names):
            discord_id = 96001 + idx
            player_ids.append(discord_id)
            test_db.add_player(
                discord_id=discord_id,
                discord_username=name,
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06
            )
        
        # Simulate shuffle: 10 players in match, 1 excluded
        # First 10 players are in the match
        match_player_ids = player_ids[:10]
        excluded_player_id = player_ids[10]  # BugReporter
        
        # Split match players into teams
        radiant_team_ids = match_player_ids[:5]
        dire_team_ids = match_player_ids[5:10]
        
        # Verify excluded player is NOT in match
        assert excluded_player_id not in radiant_team_ids
        assert excluded_player_id not in dire_team_ids
        assert excluded_player_id not in match_player_ids[:10]
        
        # Record match - Dire won
        # Simulate the fixed logic
        team1_ids_for_db = dire_team_ids  # Dire goes to team1
        team2_ids_for_db = radiant_team_ids  # Radiant goes to team2
        winning_team_for_db = 1  # team1 (Dire) won
        
        # CRITICAL VALIDATION: Ensure excluded player is NOT in match
        all_match_ids = set(team1_ids_for_db + team2_ids_for_db)
        assert excluded_player_id not in all_match_ids, \
            f"BUG: Excluded player {excluded_player_id} found in match teams!"
        
        # Record the match
        match_id = test_db.record_match(
            team1_ids=team1_ids_for_db,
            team2_ids=team2_ids_for_db,
            winning_team=winning_team_for_db
        )
        
        assert match_id is not None
        
        # CRITICAL TEST: Excluded player should have 0 wins, 0 losses
        excluded_player = test_db.get_player(excluded_player_id)
        assert excluded_player is not None
        assert excluded_player.wins == 0, \
            f"BUG: Excluded player BugReporter should have 0 wins, got {excluded_player.wins}"
        assert excluded_player.losses == 0, \
            f"BUG: Excluded player BugReporter should have 0 losses, got {excluded_player.losses}. " \
            f"This is the exact bug that was reported!"
        
        # Verify match players have correct stats
        for pid in dire_team_ids:
            player = test_db.get_player(pid)
            assert player.wins == 1, f"Dire player {player.name} should have 1 win"
            assert player.losses == 0, f"Dire player {player.name} should have 0 losses"
        
        for pid in radiant_team_ids:
            player = test_db.get_player(pid)
            assert player.wins == 0, f"Radiant player {player.name} should have 0 wins"
            assert player.losses == 1, f"Radiant player {player.name} should have 1 loss"
    
    def test_excluded_players_validation(self, test_db):
        """Test that validation prevents excluded players from being in match."""
        # Create 11 players
        player_ids = list(range(97001, 97012))
        for pid in player_ids:
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06
            )
        
        # Simulate: 10 players in match, 1 excluded
        match_player_ids = player_ids[:10]
        excluded_player_id = player_ids[10]
        
        # Split into teams
        team1_ids = match_player_ids[:5]
        team2_ids = match_player_ids[5:10]
        
        # Verify excluded player is not in teams
        assert excluded_player_id not in team1_ids
        assert excluded_player_id not in team2_ids
        
        # Record match
        match_id = test_db.record_match(
            team1_ids=team1_ids,
            team2_ids=team2_ids,
            winning_team=1
        )
        
        # Verify excluded player has no stats
        excluded_player = test_db.get_player(excluded_player_id)
        assert excluded_player.wins == 0
        assert excluded_player.losses == 0
        
        # Verify match players have stats
        for pid in team1_ids:
            player = test_db.get_player(pid)
            assert player.wins == 1
            assert player.losses == 0
        
        for pid in team2_ids:
            player = test_db.get_player(pid)
            assert player.wins == 0
            assert player.losses == 1


class TestPlayerOrderPreservation:
    """
    Test that get_players_by_ids preserves input order.
    
    This is critical because the player_name_to_id mapping relies on
    zip(player_ids, players) being in the same order.
    
    Bug scenario: SQLite returns rows in arbitrary order, causing mismatched
    Discord IDs and Player names, which results in wrong team assignments.
    """
    
    @pytest.fixture
    def test_db(self):
        """Create a temporary test database."""
        with tempfile.NamedTemporaryFile(suffix='.db', delete=False) as f:
            db_path = f.name
        
        db = Database(db_path)
        yield db
        
        # Cleanup
        try:
            os.unlink(db_path)
        except:
            pass
    
    def test_get_players_by_ids_preserves_order(self, test_db):
        """
        Test that get_players_by_ids returns players in the SAME order
        as the input discord_ids, regardless of database insertion order.
        """
        # Add players in a specific order
        players_data = [
            (1001, "Alice", 1500),
            (1002, "Bob", 1600),
            (1003, "Charlie", 1700),
            (1004, "Diana", 1800),
            (1005, "Eve", 1900),
        ]
        
        for discord_id, name, rating in players_data:
            test_db.add_player(
                discord_id=discord_id,
                discord_username=name,
                initial_mmr=1500,
                glicko_rating=float(rating),
                glicko_rd=350.0,
                glicko_volatility=0.06
            )
        
        # Request players in REVERSE order
        requested_ids = [1005, 1003, 1001, 1004, 1002]
        players = test_db.get_players_by_ids(requested_ids)
        
        # CRITICAL: Players must be returned in the same order as requested
        assert len(players) == 5
        assert players[0].name == "Eve"     # 1005
        assert players[1].name == "Charlie" # 1003
        assert players[2].name == "Alice"   # 1001
        assert players[3].name == "Diana"   # 1004
        assert players[4].name == "Bob"     # 1002
    
    def test_player_name_to_id_mapping_correctness(self, test_db):
        """
        Test that the player_name_to_id mapping is correct when using
        get_players_by_ids with zip().
        
        This is the exact pattern used in bot.py that was causing the bug.
        """
        # Add players
        players_data = [
            (1001, "Alice", 1500),
            (1002, "Bob", 1600),
            (1003, "Charlie", 1700),
        ]
        
        for discord_id, name, rating in players_data:
            test_db.add_player(
                discord_id=discord_id,
                discord_username=name,
                initial_mmr=1500,
                glicko_rating=float(rating),
                glicko_rd=350.0,
                glicko_volatility=0.06
            )
        
        # Simulate the pattern used in bot.py
        player_ids = [1003, 1001, 1002]  # Not in insertion order
        players = test_db.get_players_by_ids(player_ids)
        
        # Build the mapping (exact pattern from bot.py)
        player_name_to_id = {pl.name: pid for pid, pl in zip(player_ids, players)}
        
        # CRITICAL: Mapping must be correct
        assert player_name_to_id["Charlie"] == 1003
        assert player_name_to_id["Alice"] == 1001
        assert player_name_to_id["Bob"] == 1002
    
    def test_team_assignment_with_shuffled_ids(self, test_db):
        """
        End-to-end test simulating the exact scenario that caused the bug:
        1. Players join lobby (in some order)
        2. get_players_by_ids is called
        3. player_name_to_id mapping is built
        4. Teams are assigned
        5. Match is recorded
        
        The bug was that team assignments were scrambled because
        get_players_by_ids returned players in a different order than
        the input IDs.
        """
        # Create 10 players
        player_data = [
            (101, "BugReporter", 1500),
            (102, "FakeUser2", 1992),
            (103, "FakeUser7", 1667),
            (104, "TestPlayerA", 1028),
            (105, "FakeUser3", 1078),
            (106, "TestPlayerB", 1021),
            (107, "FakeUser6", 1184),
            (108, "FakeUser4", 1494),
            (109, "FakeUser5", 1759),
            (110, "FakeUser1", 1825),
        ]
        
        for discord_id, name, rating in player_data:
            test_db.add_player(
                discord_id=discord_id,
                discord_username=name,
                initial_mmr=1500,
                glicko_rating=float(rating),
                glicko_rd=350.0,
                glicko_volatility=0.06
            )
        
        # Simulate lobby order (might be different from insertion order)
        lobby_player_ids = [110, 109, 108, 107, 106, 105, 104, 103, 102, 101]
        
        # Get players from database
        players = test_db.get_players_by_ids(lobby_player_ids)
        
        # Build the name-to-id mapping
        player_name_to_id = {pl.name: pid for pid, pl in zip(lobby_player_ids, players)}
        
        # Verify mapping is correct
        assert player_name_to_id["FakeUser1"] == 110
        assert player_name_to_id["FakeUser5"] == 109
        assert player_name_to_id["FakeUser4"] == 108
        assert player_name_to_id["FakeUser6"] == 107
        assert player_name_to_id["TestPlayerB"] == 106
        assert player_name_to_id["FakeUser3"] == 105
        assert player_name_to_id["TestPlayerA"] == 104
        assert player_name_to_id["FakeUser7"] == 103
        assert player_name_to_id["FakeUser2"] == 102
        assert player_name_to_id["BugReporter"] == 101
        
        # Simulate team assignment (from shuffle)
        radiant_names = ["BugReporter", "FakeUser2", "FakeUser7", "TestPlayerA", "FakeUser3"]
        dire_names = ["TestPlayerB", "FakeUser6", "FakeUser4", "FakeUser5", "FakeUser1"]
        
        # Map names to IDs (the critical step that was failing)
        radiant_team_ids = [player_name_to_id[name] for name in radiant_names]
        dire_team_ids = [player_name_to_id[name] for name in dire_names]
        
        # Verify team IDs are correct
        assert radiant_team_ids == [101, 102, 103, 104, 105]
        assert dire_team_ids == [106, 107, 108, 109, 110]
        
        # Record match - Radiant won
        match_id = test_db.record_match(
            radiant_team_ids=radiant_team_ids,
            dire_team_ids=dire_team_ids,
            winning_team="radiant"
        )
        
        # Verify results
        # Radiant players should have wins
        for pid in radiant_team_ids:
            player = test_db.get_player(pid)
            assert player.wins == 1, f"Radiant player {player.name} should have 1 win"
            assert player.losses == 0, f"Radiant player {player.name} should have 0 losses"
        
        # Dire players should have losses
        for pid in dire_team_ids:
            player = test_db.get_player(pid)
            assert player.wins == 0, f"Dire player {player.name} should have 0 wins"
            assert player.losses == 1, f"Dire player {player.name} should have 1 loss"


if __name__ == "__main__":
    pytest.main([__file__, "-v"])

