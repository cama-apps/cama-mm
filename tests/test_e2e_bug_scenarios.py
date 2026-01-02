"""
End-to-end tests for bug scenarios reported by users.
Tests the exact bug scenario and production bugs.
"""

import pytest
import os
import tempfile
import time

from database import Database
from rating_system import CamaRatingSystem


class TestExactBugScenario:
    """Test the exact bug scenario reported by user - Dire wins but recorded as loss."""
    
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
    
    def test_exact_bug_scenario_shuffle_to_leaderboard(self, test_db):
        """
        End-to-end test reproducing the EXACT bug scenario from the user's report.
        
        This test simulates the complete flow:
        1. Players join lobby
        2. Shuffle creates teams (Radiant vs Dire with exact player names)
        3. Match is recorded with "Dire won"
        4. Leaderboard is checked - should show Dire players with wins
        
        The bug was: Dire won, but Dire players showed as losses in leaderboard.
        """
        # Exact player data from bug report
        player_data = [
            # Radiant team (from shuffle output)
            ("FakeUser917762", 1405),
            ("FakeUser924119", 1120),
            ("FakeUser926408", 1763),
            ("FakeUser921765", 1689),
            ("FakeUser925589", 1568),
            # Dire team (from shuffle output)
            ("FakeUser923487", 1161),
            ("BugReporter", 1500),  # The user who reported the bug
            ("FakeUser921510", 1816),
            ("FakeUser920053", 1500),
            ("FakeUser919197", 1601),
        ]
        
        # Step 1: Register all players (simulate /register command)
        player_ids = []
        for idx, (name, rating) in enumerate(player_data):
            discord_id = 95001 + idx
            player_ids.append(discord_id)
            test_db.add_player(
                discord_id=discord_id,
                discord_username=name,
                initial_mmr=1500,
                glicko_rating=float(rating),
                glicko_rd=350.0,
                glicko_volatility=0.06
            )
        
        # Step 2: Simulate shuffle output (as stored in bot.last_shuffle)
        # This is what the shuffle command creates
        radiant_team_ids = player_ids[:5]  # First 5 = Radiant
        dire_team_ids = player_ids[5:]     # Last 5 = Dire
        
        # Team number assignment (randomly assigned in real shuffle)
        radiant_team_num = 1
        dire_team_num = 2
        
        # Step 3: Simulate /record command with "Dire won"
        winning_team_num = dire_team_num
        winning_team_display = "Dire"
        
        # Apply the FIXED logic from bot.py
        actual_radiant_team_num = radiant_team_num
        actual_dire_team_num = dire_team_num
        
        # Validate team assignments (as the fix does)
        assert actual_radiant_team_num is not None
        assert actual_dire_team_num is not None
        assert actual_radiant_team_num != actual_dire_team_num
        
        # Map winning team to team1/team2 for database (FIXED LOGIC)
        if winning_team_num == actual_radiant_team_num:
            team1_ids_for_db = radiant_team_ids
            team2_ids_for_db = dire_team_ids
            winning_team_for_db = 1
        elif winning_team_num == actual_dire_team_num:
            # Dire won - THIS IS THE BUG SCENARIO
            team1_ids_for_db = dire_team_ids
            team2_ids_for_db = radiant_team_ids
            winning_team_for_db = 1
        else:
            raise ValueError(f"Invalid winning_team_num: {winning_team_num}")
        
        # Step 4: Record match (simulate db.record_match call)
        match_id = test_db.record_match(
            team1_ids=team1_ids_for_db,
            team2_ids=team2_ids_for_db,
            winning_team=winning_team_for_db
        )
        
        assert match_id is not None
        
        # Step 5: Simulate /leaderboard command
        all_players = test_db.get_all_players()
        
        rating_system = CamaRatingSystem()
        
        players_with_stats = []
        for player in all_players:
            total_games = player.wins + player.losses
            win_rate = (player.wins / total_games * 100) if total_games > 0 else 0.0
            cama_rating = None
            if player.glicko_rating is not None:
                cama_rating = rating_system.rating_to_display(player.glicko_rating)
            players_with_stats.append((player, player.wins, player.losses, win_rate, cama_rating))
        
        # Sort by wins (descending), then by rating
        players_with_stats.sort(key=lambda x: (x[1], x[4] if x[4] is not None else 0), reverse=True)
        
        # Step 6: CRITICAL VERIFICATION - The bug fix
        
        # Find BugReporter (the bug reporter)
        reporter_found = False
        for player, wins, losses, win_rate, rating in players_with_stats:
            if player.name == "BugReporter":
                reporter_found = True
                # THE BUG: BugReporter showed 0-1 even though Dire won
                # THE FIX: BugReporter should show 1-0
                assert wins == 1, \
                    f"BUG FIX: BugReporter should have 1 win (Dire won), got {wins}. " \
                    f"This is the exact bug scenario that was reported!"
                assert losses == 0, \
                    f"BUG FIX: BugReporter should have 0 losses (Dire won), got {losses}"
                assert win_rate == 100.0, \
                    f"BUG FIX: BugReporter should have 100% win rate, got {win_rate:.1f}%"
                break
        
        assert reporter_found, "BugReporter not found in leaderboard results"
        
        # Verify all Dire players have correct stats
        dire_names = ["BugReporter", "FakeUser923487", "FakeUser921510", "FakeUser920053", "FakeUser919197"]
        for player, wins, losses, win_rate, rating in players_with_stats:
            if player.name in dire_names:
                assert wins == 1, \
                    f"Dire player {player.name} should have 1 win, got {wins}"
                assert losses == 0, \
                    f"Dire player {player.name} should have 0 losses, got {losses}"
        
        # Verify all Radiant players have correct stats
        radiant_names = ["FakeUser917762", "FakeUser924119", "FakeUser926408", "FakeUser921765", "FakeUser925589"]
        for player, wins, losses, win_rate, rating in players_with_stats:
            if player.name in radiant_names:
                assert wins == 0, \
                    f"Radiant player {player.name} should have 0 wins, got {wins}"
                assert losses == 1, \
                    f"Radiant player {player.name} should have 1 loss, got {losses}"
        
        # Verify leaderboard order matches expected (winners on top)
        # In the bug report, winners were on top but with wrong stats
        # After fix, winners should be on top with correct stats
        top_5 = players_with_stats[:5]
        bottom_5 = players_with_stats[5:]
        
        # All top 5 should have 1 win (Dire players)
        for player, wins, losses, win_rate, rating in top_5:
            assert wins == 1, \
                f"Top 5 player {player.name} should have 1 win, got {wins}"
        
        # All bottom 5 should have 0 wins (Radiant players)
        for player, wins, losses, win_rate, rating in bottom_5:
            assert wins == 0, \
                f"Bottom 5 player {player.name} should have 0 wins, got {wins}"


class TestProductionBugScenarios:
    """End-to-end tests for actual production bugs reported by users."""
    
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
    
    def test_excluded_players_get_losses_bug(self, test_db):
        """
        Full end-to-end test for the bug where excluded players get losses recorded.
        
        Scenario from production:
        - 12 players in lobby
        - 10 players selected for match, 2 excluded (BugReporter and FakeUser547520)
        - Match recorded with Dire won
        - Bug: BugReporter (excluded) got a loss (0-2)
        - Expected: BugReporter should have 0-0 (not in match)
        """
        # Create 12 players (exact scenario from bug report)
        player_data = [
            ("FakeUser172699", 1623),
            ("FakeUser169817", 1018),
            ("FakeUser167858", 1744),
            ("FakeUser175544", 1822),
            ("FakeUser173967", 1836),
            ("FakeUser170233", 1590),
            ("FakeUser174788", 1457),
            ("FakeUser171621", 1882),
            ("FakeUser166664", 1579),
            ("FakeUser168472", 1537),
            ("BugReporter", 1500),  # This player was excluded but got a loss
            ("FakeUser547520", 1900),  # This player was also excluded
        ]
        
        player_ids = []
        for idx, (name, rating) in enumerate(player_data):
            discord_id = 98001 + idx
            player_ids.append(discord_id)
            test_db.add_player(
                discord_id=discord_id,
                discord_username=name,
                initial_mmr=1500,
                glicko_rating=float(rating),
                glicko_rd=350.0,
                glicko_volatility=0.06
            )
        
        # Simulate shuffle_from_pool: 10 players selected, 2 excluded
        # In real scenario, the algorithm selects the best 10
        # For this test, we'll simulate: first 10 in match, last 2 excluded
        match_player_ids = player_ids[:10]
        excluded_player_ids = player_ids[10:]  # BugReporter and FakeUser547520
        
        # Verify excluded players
        assert len(excluded_player_ids) == 2
        reporter_id = next(pid for pid, name in zip(player_ids, [p[0] for p in player_data]) if name == "BugReporter")
        assert reporter_id in excluded_player_ids, "BugReporter should be excluded"
        
        # Split match players into teams (simulate shuffle result)
        radiant_team_ids = match_player_ids[:5]
        dire_team_ids = match_player_ids[5:10]
        
        # Verify excluded players are NOT in match
        all_match_ids = set(radiant_team_ids + dire_team_ids)
        excluded_set = set(excluded_player_ids)
        assert all_match_ids.isdisjoint(excluded_set), \
            "Excluded players should not be in match teams"
        
        # Simulate team number assignment
        radiant_team_num = 1
        dire_team_num = 2
        
        # Record match - Dire won
        winning_team_num = dire_team_num
        
        # Apply the fixed logic
        if winning_team_num == dire_team_num:
            team1_ids_for_db = dire_team_ids  # Dire goes to team1
            team2_ids_for_db = radiant_team_ids  # Radiant goes to team2
            winning_team_for_db = 1  # team1 (Dire) won
        else:
            team1_ids_for_db = radiant_team_ids
            team2_ids_for_db = dire_team_ids
            winning_team_for_db = 1
        
        # CRITICAL VALIDATION: Ensure excluded players are NOT in match
        all_match_ids = set(team1_ids_for_db + team2_ids_for_db)
        excluded_set = set(excluded_player_ids)
        excluded_in_match = all_match_ids.intersection(excluded_set)
        assert len(excluded_in_match) == 0, \
            f"BUG: Excluded players found in match: {excluded_in_match}"
        
        # Record the match
        match_id = test_db.record_match(
            team1_ids=team1_ids_for_db,
            team2_ids=team2_ids_for_db,
            winning_team=winning_team_for_db
        )
        
        assert match_id is not None
        
        # CRITICAL TEST: Excluded players should have 0-0
        for excluded_id in excluded_player_ids:
            player = test_db.get_player(excluded_id)
            player_name = player.name if player else f"Unknown({excluded_id})"
            assert player.wins == 0, \
                f"BUG: Excluded player {player_name} should have 0 wins, got {player.wins}"
            assert player.losses == 0, \
                f"BUG: Excluded player {player_name} should have 0 losses, got {player.losses}. " \
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
    
    def test_radiant_wins_excluded_players_bug(self, test_db):
        """
        Full end-to-end test: Radiant wins with excluded players.
        
        Scenario from production:
        - 12 players in lobby
        - 10 players selected, 2 excluded (FakeUser547520, FakeUser546625)
        - Match recorded with Radiant won
        - Bug: Excluded players got wins/losses
        - Expected: Excluded players should have 0-0
        """
        # Create 12 players (exact scenario from latest bug report)
        player_data = [
            ("FakeUser542744", 1421),
            ("BugReporter", 1500),
            ("FakeUser548931", 1638),
            ("FakeUser545142", 1307),
            ("FakeUser541518", 1331),
            ("FakeUser551025", 1675),
            ("FakeUser543693", 1874),
            ("FakeUser546016", 1522),
            ("FakeUser539516", 1077),
            ("FakeUser549780", 1049),
            ("FakeUser547520", 1900),  # Excluded
            ("FakeUser546625", 1750),  # Excluded
        ]
        
        player_ids = []
        for idx, (name, rating) in enumerate(player_data):
            discord_id = 99001 + idx
            player_ids.append(discord_id)
            test_db.add_player(
                discord_id=discord_id,
                discord_username=name,
                initial_mmr=1500,
                glicko_rating=float(rating),
                glicko_rd=350.0,
                glicko_volatility=0.06
            )
        
        # Simulate shuffle: 10 players in match, 2 excluded
        # Based on the shuffle output from production:
        # Radiant: FakeUser542744, BugReporter, FakeUser548931, FakeUser545142, FakeUser541518
        # Dire: FakeUser551025, FakeUser543693, FakeUser546016, FakeUser539516, FakeUser549780
        # Excluded: FakeUser547520, FakeUser546625
        
        # Map names to IDs
        name_to_id = {name: pid for pid, (name, _) in zip(player_ids, player_data)}
        
        radiant_names = ["FakeUser542744", "BugReporter", "FakeUser548931", "FakeUser545142", "FakeUser541518"]
        dire_names = ["FakeUser551025", "FakeUser543693", "FakeUser546016", "FakeUser539516", "FakeUser549780"]
        excluded_names = ["FakeUser547520", "FakeUser546625"]
        
        radiant_team_ids = [name_to_id[name] for name in radiant_names]
        dire_team_ids = [name_to_id[name] for name in dire_names]
        excluded_player_ids = [name_to_id[name] for name in excluded_names]
        
        # Verify we have correct teams
        assert len(radiant_team_ids) == 5
        assert len(dire_team_ids) == 5
        assert len(excluded_player_ids) == 2
        
        # Verify excluded players are NOT in match
        all_match_ids = set(radiant_team_ids + dire_team_ids)
        excluded_set = set(excluded_player_ids)
        assert all_match_ids.isdisjoint(excluded_set), \
            "Excluded players should not be in match teams"
        
        # Record match - Radiant won
        radiant_team_num = 1
        dire_team_num = 2
        winning_team_num = radiant_team_num
        
        # Apply the fixed logic
        if winning_team_num == radiant_team_num:
            team1_ids_for_db = radiant_team_ids  # Radiant goes to team1
            team2_ids_for_db = dire_team_ids  # Dire goes to team2
            winning_team_for_db = 1  # team1 (Radiant) won
        else:
            team1_ids_for_db = dire_team_ids
            team2_ids_for_db = radiant_team_ids
            winning_team_for_db = 1
        
        # Final validation: Ensure excluded players are NOT in match
        final_match_ids = set(team1_ids_for_db + team2_ids_for_db)
        excluded_in_final = final_match_ids.intersection(excluded_set)
        assert len(excluded_in_final) == 0, \
            f"BUG: Excluded players in final match teams: {excluded_in_final}"
        
        # Record the match
        match_id = test_db.record_match(
            team1_ids=team1_ids_for_db,
            team2_ids=team2_ids_for_db,
            winning_team=winning_team_for_db
        )
        
        assert match_id is not None
        
        # CRITICAL TEST: Excluded players should have 0-0
        for excluded_name in excluded_names:
            excluded_id = name_to_id[excluded_name]
            player = test_db.get_player(excluded_id)
            assert player.wins == 0, \
                f"BUG: Excluded player {excluded_name} should have 0 wins, got {player.wins}"
            assert player.losses == 0, \
                f"BUG: Excluded player {excluded_name} should have 0 losses, got {player.losses}"
        
        # Verify Radiant players (winners) have 1-0
        for radiant_name in radiant_names:
            radiant_id = name_to_id[radiant_name]
            player = test_db.get_player(radiant_id)
            assert player.wins == 1, \
                f"Radiant player {radiant_name} should have 1 win, got {player.wins}"
            assert player.losses == 0, \
                f"Radiant player {radiant_name} should have 0 losses, got {player.losses}"
        
        # Verify Dire players (losers) have 0-1
        for dire_name in dire_names:
            dire_id = name_to_id[dire_name]
            player = test_db.get_player(dire_id)
            assert player.wins == 0, \
                f"Dire player {dire_name} should have 0 wins, got {player.wins}"
            assert player.losses == 1, \
                f"Dire player {dire_name} should have 1 loss, got {player.losses}"
    
    def test_multiple_matches_with_exclusions(self, test_db):
        """
        Test multiple matches where players are sometimes excluded.
        
        This tests that excluded players don't accumulate incorrect stats
        across multiple matches.
        """
        # Create 12 players
        player_names = [f"Player{i}" for i in range(1, 13)]
        player_ids = list(range(100001, 100013))
        
        for pid, name in zip(player_ids, player_names):
            test_db.add_player(
                discord_id=pid,
                discord_username=name,
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06
            )
        
        # Match 1: First 10 players, last 2 excluded
        match1_player_ids = player_ids[:10]
        match1_excluded = player_ids[10:]
        match1_radiant = match1_player_ids[:5]
        match1_dire = match1_player_ids[5:10]
        
        # Record match 1 - Radiant won
        test_db.record_match(
            team1_ids=match1_radiant,
            team2_ids=match1_dire,
            winning_team=1
        )
        
        # Verify excluded players still have 0-0
        for excluded_id in match1_excluded:
            player = test_db.get_player(excluded_id)
            assert player.wins == 0 and player.losses == 0, \
                f"After match 1: Excluded player {player.name} should have 0-0"
        
        # Match 2: Different 10 players (rotate)
        match2_player_ids = player_ids[2:12]  # Skip first 2, include last 2
        match2_excluded = player_ids[:2]  # First 2 now excluded
        match2_radiant = match2_player_ids[:5]
        match2_dire = match2_player_ids[5:10]
        
        # Record match 2 - Dire won
        test_db.record_match(
            team1_ids=match2_dire,  # Dire goes to team1
            team2_ids=match2_radiant,  # Radiant goes to team2
            winning_team=1  # team1 (Dire) won
        )
        
        # Verify previously excluded players (now in match) have correct stats
        for pid in match2_radiant:  # Radiant lost match 2
            player = test_db.get_player(pid)
            if pid in match1_radiant:  # Was in match 1 and won
                assert player.wins == 1 and player.losses == 1, \
                    f"Player {player.name} should have 1-1 (won match 1, lost match 2)"
            elif pid in match1_dire:  # Was in match 1 and lost
                assert player.wins == 0 and player.losses == 2, \
                    f"Player {player.name} should have 0-2 (lost match 1 and match 2)"
            else:  # Was excluded from match 1, new to match 2
                assert player.wins == 0 and player.losses == 1, \
                    f"Player {player.name} should have 0-1 (lost match 2)"
        
        # Verify newly excluded players still have stats from match 1
        for excluded_id in match2_excluded:
            player = test_db.get_player(excluded_id)
            if excluded_id in match1_radiant:  # Was in match 1
                assert player.wins == 1 and player.losses == 0, \
                    f"Player {player.name} should have 1-0 (won match 1, excluded from match 2)"
            else:  # Was excluded from match 1 too
                assert player.wins == 0 and player.losses == 0, \
                    f"Player {player.name} should have 0-0 (excluded from both matches)"


if __name__ == "__main__":
    pytest.main([__file__, "-v"])

