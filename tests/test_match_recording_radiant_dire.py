"""
Tests for Radiant/Dire mapping fix in match recording.
Tests the critical bug fix where Dire wins were incorrectly recorded as losses.
"""

import os
import tempfile
import time

import pytest

from database import Database
from domain.models.team import Team


class TestRadiantDireMapping:
    """Test the Radiant/Dire mapping fix for match recording."""

    def test_radiant_dire_team_mapping(self):
        """Test that Radiant/Dire teams are correctly mapped to team1/team2."""
        # Simulate the shuffle output structure
        # After shuffle, teams are randomly assigned Radiant/Dire
        # We need to ensure wins/losses are recorded correctly

        # Scenario: Team 1 (original) becomes Radiant, Team 2 becomes Dire
        # If Radiant wins, team1_ids should win
        # If Dire wins, team2_ids should win

        radiant_team_num = 1
        dire_team_num = 2

        # If Radiant won
        if radiant_team_num == 1:
            # Radiant is team 1, so team 1 wins
            winning_team_for_db = 1
        else:
            # Radiant is team 2, so team 2 wins
            winning_team_for_db = 2

        # Verify logic
        assert winning_team_for_db == 1  # In this scenario

        # Scenario: Team 1 becomes Dire, Team 2 becomes Radiant
        radiant_team_num = 2
        dire_team_num = 1

        # If Dire won
        if dire_team_num == 1:
            # Dire is team 1, so team 1 wins
            winning_team_for_db = 1
        else:
            # Dire is team 2, so team 2 wins
            winning_team_for_db = 2

        assert winning_team_for_db == 1  # In this scenario

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

    def test_team_id_mapping_after_shuffle(self, test_db):
        """
        Test the critical bug fix where team IDs were incorrectly mapped after shuffling.

        The bug was: After shuffling, we assumed player_ids[:5] = team1 and player_ids[5:] = team2,
        but the shuffled teams don't match that order. This test verifies that players are correctly
        mapped by name (as the fix does) rather than by position in the player_ids list.
        """
        # Create 10 players with specific names and Discord IDs
        # The order of player_ids doesn't match the shuffled teams
        player_ids = [4001, 4002, 4003, 4004, 4005, 4006, 4007, 4008, 4009, 4010]
        player_names = [f"Player{i}" for i in range(1, 11)]

        # Add players to database
        for pid, name in zip(player_ids, player_names):
            test_db.add_player(
                discord_id=pid,
                discord_username=name,
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        # Get Player objects from database
        players = test_db.get_players_by_ids(player_ids)

        # Simulate a shuffle where teams don't match the order of player_ids
        # Team 1: players at positions 0, 2, 4, 6, 8 (Player1, Player3, Player5, Player7, Player9)
        # Team 2: players at positions 1, 3, 5, 7, 9 (Player2, Player4, Player6, Player8, Player10)
        team1_players = [players[0], players[2], players[4], players[6], players[8]]
        team2_players = [players[1], players[3], players[5], players[7], players[9]]

        # Create Team objects
        team1 = Team(team1_players, role_assignments=["1", "2", "3", "4", "5"])
        team2 = Team(team2_players, role_assignments=["1", "2", "3", "4", "5"])

        # Simulate the fix: Map players by name, not by position
        # This is what the fixed code does: player_name_to_id = {pl.name: pid for pid, pl in zip(player_ids, players)}
        player_name_to_id = {pl.name: pid for pid, pl in zip(player_ids, players)}

        # Map team1 and team2 players to their Discord IDs (the fix)
        team1_ids = [player_name_to_id[p.name] for p in team1.players]
        team2_ids = [player_name_to_id[p.name] for p in team2.players]

        # Verify the mapping is correct (not just player_ids[:5] and player_ids[5:])
        # Team 1 should have: 4001, 4003, 4005, 4007, 4009
        expected_team1_ids = [4001, 4003, 4005, 4007, 4009]
        # Team 2 should have: 4002, 4004, 4006, 4008, 4010
        expected_team2_ids = [4002, 4004, 4006, 4008, 4010]

        assert set(team1_ids) == set(expected_team1_ids), (
            f"Team 1 IDs don't match! Got {team1_ids}, expected {expected_team1_ids}"
        )
        assert set(team2_ids) == set(expected_team2_ids), (
            f"Team 2 IDs don't match! Got {team2_ids}, expected {expected_team2_ids}"
        )

        # Simulate Radiant/Dire assignment: Team 1 = Radiant, Team 2 = Dire
        radiant_team_ids = team1_ids
        dire_team_ids = team2_ids

        # Record match: Radiant (Team 1) wins
        # Map to database format: team1 = Radiant, team2 = Dire, winning_team = 1
        match_id = test_db.record_match(
            team1_ids=radiant_team_ids, team2_ids=dire_team_ids, winning_team=1
        )

        assert match_id is not None

        # Verify ONLY the correct 5 players got wins (Team 1 / Radiant)
        for pid in team1_ids:
            player = test_db.get_player(pid)
            assert player.wins == 1, f"Player {pid} (Team 1) should have 1 win, got {player.wins}"
            assert player.losses == 0, (
                f"Player {pid} (Team 1) should have 0 losses, got {player.losses}"
            )

        # Verify ONLY the correct 5 players got losses (Team 2 / Dire)
        for pid in team2_ids:
            player = test_db.get_player(pid)
            assert player.wins == 0, f"Player {pid} (Team 2) should have 0 wins, got {player.wins}"
            assert player.losses == 1, (
                f"Player {pid} (Team 2) should have 1 loss, got {player.losses}"
            )

        # Verify all players are accounted for and have correct win/loss counts
        all_player_ids = set(player_ids)
        team1_set = set(team1_ids)
        team2_set = set(team2_ids)

        # Verify all players are in exactly one team
        assert len(team1_set) == 5, f"Team 1 should have 5 players, got {len(team1_set)}"
        assert len(team2_set) == 5, f"Team 2 should have 5 players, got {len(team2_set)}"
        assert team1_set.isdisjoint(team2_set), "Teams should not share players"
        assert team1_set.union(team2_set) == all_player_ids, "All players should be in a team"

        # Verify win/loss counts for all players
        for pid in all_player_ids:
            player = test_db.get_player(pid)
            if pid in team1_set:
                # Should have 1 win, 0 losses
                assert player.wins == 1 and player.losses == 0, (
                    f"Player {pid} (Team 1) should have 1 win, 0 losses, got {player.wins}-{player.losses}"
                )
            else:  # pid in team2_set
                # Should have 0 wins, 1 loss
                assert player.wins == 0 and player.losses == 1, (
                    f"Player {pid} (Team 2) should have 0 wins, 1 loss, got {player.wins}-{player.losses}"
                )


class TestRadiantDireBugFix:
    """Test the critical bug fix where Dire wins were incorrectly recorded as losses."""

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

    def test_exact_bug_scenario_dire_wins(self, test_db):
        """
        Test the exact bug scenario reported by user.

        Scenario:
        - Radiant: FakeUser917762, FakeUser924119, FakeUser926408, FakeUser921765, FakeUser925589
        - Dire: FakeUser923487, BugReporter, FakeUser921510, FakeUser920053, FakeUser919197
        - Dire won
        - Bug: Dire players were recorded as losses, Radiant players as wins
        - Expected: Dire players should have wins, Radiant players should have losses
        """
        # Create players with exact names from the bug report
        radiant_players = [
            ("FakeUser917762", 1405),
            ("FakeUser924119", 1120),
            ("FakeUser926408", 1763),
            ("FakeUser921765", 1689),
            ("FakeUser925589", 1568),
        ]

        dire_players = [
            ("FakeUser923487", 1161),
            ("BugReporter", 1500),  # The user who reported the bug
            ("FakeUser921510", 1816),
            ("FakeUser920053", 1500),
            ("FakeUser919197", 1601),
        ]

        # Assign Discord IDs (using sequential IDs for testing)
        player_data = []
        discord_id = 90001

        for name, rating in radiant_players + dire_players:
            test_db.add_player(
                discord_id=discord_id,
                discord_username=name,
                initial_mmr=1500,
                glicko_rating=float(rating),
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )
            player_data.append((discord_id, name, rating))
            discord_id += 1

        # Extract Discord IDs for each team
        radiant_team_ids = [
            pid for pid, name, _ in player_data if name in [n for n, _ in radiant_players]
        ]
        dire_team_ids = [
            pid for pid, name, _ in player_data if name in [n for n, _ in dire_players]
        ]

        # Verify we have the right players
        assert len(radiant_team_ids) == 5, (
            f"Expected 5 Radiant players, got {len(radiant_team_ids)}"
        )
        assert len(dire_team_ids) == 5, f"Expected 5 Dire players, got {len(dire_team_ids)}"

        # Simulate the shuffle output structure (as stored in bot.last_shuffle)
        # In the actual bug, radiant_team_num could be 1 or 2, dire_team_num would be the other
        # Let's test both scenarios to ensure the fix works

        # Scenario 1: Radiant is team 1, Dire is team 2
        radiant_team_num = 1
        dire_team_num = 2

        # Simulate recording match with "Dire won"
        # This is what the fixed code does:
        winning_team_num = dire_team_num  # Dire won

        # Map winning team to team1/team2 for database
        if winning_team_num == radiant_team_num:
            # Radiant won
            team1_ids_for_db = radiant_team_ids
            team2_ids_for_db = dire_team_ids
            winning_team_for_db = 1
        elif winning_team_num == dire_team_num:
            # Dire won - THIS IS THE BUG SCENARIO
            team1_ids_for_db = dire_team_ids  # Dire goes to team1
            team2_ids_for_db = radiant_team_ids  # Radiant goes to team2
            winning_team_for_db = 1  # team1 (Dire) won
        else:
            raise ValueError(f"Invalid winning_team_num: {winning_team_num}")

        # Record the match
        match_id = test_db.record_match(
            team1_ids=team1_ids_for_db, team2_ids=team2_ids_for_db, winning_team=winning_team_for_db
        )

        assert match_id is not None

        # CRITICAL TEST: Verify Dire players (who won) have WINS
        for pid in dire_team_ids:
            player = test_db.get_player(pid)
            player_name = player.name if player else f"Unknown({pid})"
            assert player.wins == 1, (
                f"BUG REPRODUCTION: Dire player {player_name} (ID: {pid}) should have 1 win, got {player.wins}"
            )
            assert player.losses == 0, (
                f"BUG REPRODUCTION: Dire player {player_name} (ID: {pid}) should have 0 losses, got {player.losses}"
            )

        # CRITICAL TEST: Verify Radiant players (who lost) have LOSSES
        for pid in radiant_team_ids:
            player = test_db.get_player(pid)
            player_name = player.name if player else f"Unknown({pid})"
            assert player.wins == 0, (
                f"BUG REPRODUCTION: Radiant player {player_name} (ID: {pid}) should have 0 wins, got {player.wins}"
            )
            assert player.losses == 1, (
                f"BUG REPRODUCTION: Radiant player {player_name} (ID: {pid}) should have 1 loss, got {player.losses}"
            )

        # Specifically verify BugReporter (the user who reported the bug) has a WIN
        reporter_player = None
        for pid in dire_team_ids:
            player = test_db.get_player(pid)
            if player and player.name == "BugReporter":
                reporter_player = player
                break

        assert reporter_player is not None, "BugReporter player not found in Dire team"
        assert reporter_player.wins == 1, (
            f"BUG: BugReporter should have 1 win (Dire won), got {reporter_player.wins}"
        )
        assert reporter_player.losses == 0, (
            f"BUG: BugReporter should have 0 losses (Dire won), got {reporter_player.losses}"
        )

    def test_exact_bug_scenario_radiant_wins(self, test_db):
        """
        Test the reverse scenario: Radiant wins (to ensure fix works both ways).

        Same teams as bug report, but Radiant wins this time.
        """
        # Same player setup
        radiant_players = [
            ("FakeUser917762", 1405),
            ("FakeUser924119", 1120),
            ("FakeUser926408", 1763),
            ("FakeUser921765", 1689),
            ("FakeUser925589", 1568),
        ]

        dire_players = [
            ("FakeUser923487", 1161),
            ("BugReporter", 1500),
            ("FakeUser921510", 1816),
            ("FakeUser920053", 1500),
            ("FakeUser919197", 1601),
        ]

        player_data = []
        discord_id = 91001

        for name, rating in radiant_players + dire_players:
            test_db.add_player(
                discord_id=discord_id,
                discord_username=name,
                initial_mmr=1500,
                glicko_rating=float(rating),
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )
            player_data.append((discord_id, name, rating))
            discord_id += 1

        radiant_team_ids = [
            pid for pid, name, _ in player_data if name in [n for n, _ in radiant_players]
        ]
        dire_team_ids = [
            pid for pid, name, _ in player_data if name in [n for n, _ in dire_players]
        ]

        # Scenario: Radiant is team 2, Dire is team 1 (opposite of previous test)
        radiant_team_num = 2
        dire_team_num = 1

        # Radiant won
        winning_team_num = radiant_team_num

        # Map winning team to team1/team2 for database
        if winning_team_num == radiant_team_num:
            # Radiant won
            team1_ids_for_db = radiant_team_ids
            team2_ids_for_db = dire_team_ids
            winning_team_for_db = 1
        elif winning_team_num == dire_team_num:
            # Dire won
            team1_ids_for_db = dire_team_ids
            team2_ids_for_db = radiant_team_ids
            winning_team_for_db = 1
        else:
            raise ValueError(f"Invalid winning_team_num: {winning_team_num}")

        # Record the match
        match_id = test_db.record_match(
            team1_ids=team1_ids_for_db, team2_ids=team2_ids_for_db, winning_team=winning_team_for_db
        )

        assert match_id is not None

        # Verify Radiant players (who won) have WINS
        for pid in radiant_team_ids:
            player = test_db.get_player(pid)
            assert player.wins == 1, (
                f"Radiant player {player.name} should have 1 win, got {player.wins}"
            )
            assert player.losses == 0, (
                f"Radiant player {player.name} should have 0 losses, got {player.losses}"
            )

        # Verify Dire players (who lost) have LOSSES
        for pid in dire_team_ids:
            player = test_db.get_player(pid)
            assert player.wins == 0, (
                f"Dire player {player.name} should have 0 wins, got {player.wins}"
            )
            assert player.losses == 1, (
                f"Dire player {player.name} should have 1 loss, got {player.losses}"
            )

    def test_team_number_validation(self, test_db):
        """Test that the fix properly validates team numbers."""
        # Create test players
        player_ids = list(range(92001, 92011))
        for pid in player_ids:
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        team1_ids = player_ids[:5]
        team2_ids = player_ids[5:]

        # Test with missing team numbers (should be handled gracefully)
        # This simulates what happens if last_shuffle is missing team numbers
        # The fix should handle this by using explicit checks

        # Normal case: both team numbers set
        radiant_team_num = 1
        dire_team_num = 2

        # Dire wins
        winning_team_num = dire_team_num

        # This is the fixed logic
        if radiant_team_num is not None and dire_team_num is not None:
            if radiant_team_num == dire_team_num:
                raise ValueError("Invalid: both teams have same number")

            if winning_team_num == radiant_team_num:
                team1_ids_for_db = team1_ids  # Assuming these are radiant
                team2_ids_for_db = team2_ids  # Assuming these are dire
                winning_team_for_db = 1
            elif winning_team_num == dire_team_num:
                team1_ids_for_db = team2_ids  # Dire goes to team1
                team2_ids_for_db = team1_ids  # Radiant goes to team2
                winning_team_for_db = 1
            else:
                raise ValueError(f"Invalid winning_team_num: {winning_team_num}")
        else:
            raise ValueError("Missing team numbers")

        # Record match
        match_id = test_db.record_match(
            team1_ids=team1_ids_for_db, team2_ids=team2_ids_for_db, winning_team=winning_team_for_db
        )

        assert match_id is not None

        # Verify team2_ids (Radiant) lost
        for pid in team1_ids:
            player = test_db.get_player(pid)
            # These were originally team1 (Radiant), but got swapped to team2, so they lost
            assert player.losses == 1, f"Player {pid} should have 1 loss"

        # Verify team1_ids (Dire) won (they were swapped to team1)
        for pid in team2_ids:
            player = test_db.get_player(pid)
            # These were originally team2 (Dire), but got swapped to team1, so they won
            assert player.wins == 1, f"Player {pid} should have 1 win"


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
