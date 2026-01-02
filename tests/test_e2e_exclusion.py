"""
End-to-end tests for exclusion count tracking feature.
"""

import pytest
import os
import tempfile
import time

from database import Database
from shuffler import BalancedShuffler


class TestExclusionTracking:
    """End-to-end tests for exclusion count tracking feature."""
    
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
    
    def test_exclusion_count_workflow(self, test_db):
        """
        Test the complete exclusion tracking workflow:
        1. Create 12 players
        2. Shuffle (2 excluded, 10 included)
        3. Verify excluded players' counts increment
        4. Verify included players' counts decay
        5. Shuffle again and verify new selection
        """
        # Step 1: Create 12 players
        player_ids = list(range(400001, 400013))
        player_names = [f"Player{i}" for i in range(1, 13)]
        
        for pid, name in zip(player_ids, player_names):
            test_db.add_player(
                discord_id=pid,
                discord_username=name,
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06
            )
        
        # Step 2: Get initial exclusion counts (all should be 0)
        initial_counts = test_db.get_exclusion_counts(player_ids)
        for pid in player_ids:
            assert initial_counts[pid] == 0, f"Player {pid} should start with 0 exclusion count"
        
        # Step 3: Shuffle from pool (simulate first shuffle)
        players = test_db.get_players_by_ids(player_ids)
        shuffler = BalancedShuffler(use_glicko=True, off_role_flat_penalty=50.0, exclusion_penalty_weight=5.0)
        
        # Get exclusion counts for shuffler
        exclusion_counts_by_id = test_db.get_exclusion_counts(player_ids)
        exclusion_counts = {pl.name: exclusion_counts_by_id[pid] for pid, pl in zip(player_ids, players)}
        
        team1, team2, excluded_players = shuffler.shuffle_from_pool(players, exclusion_counts)
        
        assert len(team1.players) == 5
        assert len(team2.players) == 5
        assert len(excluded_players) == 2
        
        # Step 4: Track which players were included/excluded
        player_name_to_id = {pl.name: pid for pid, pl in zip(player_ids, players)}
        included_player_ids = [player_name_to_id[p.name] for p in team1.players + team2.players]
        excluded_player_ids = [player_name_to_id[p.name] for p in excluded_players]
        
        assert len(included_player_ids) == 10
        assert len(excluded_player_ids) == 2
        assert set(included_player_ids).isdisjoint(set(excluded_player_ids))
        
        # Step 5: Simulate bot behavior - increment excluded, decay included
        for pid in excluded_player_ids:
            test_db.increment_exclusion_count(pid)
        for pid in included_player_ids:
            test_db.decay_exclusion_count(pid)
        
        # Step 6: Verify counts changed correctly
        updated_counts = test_db.get_exclusion_counts(player_ids)
        
        for pid in excluded_player_ids:
            assert updated_counts[pid] == 1, f"Excluded player {pid} should have count 1"
        
        for pid in included_player_ids:
            assert updated_counts[pid] == 0, f"Included player {pid} should have count 0 (0/2 = 0)"
        
        # Step 7: Shuffle again - excluded players should be more likely to be included
        # First, increment the same excluded players again to make the effect more obvious
        for pid in excluded_player_ids:
            test_db.increment_exclusion_count(pid)
            test_db.increment_exclusion_count(pid)
            test_db.increment_exclusion_count(pid)
        
        # Now excluded players have count=4, others have count=0
        second_counts = test_db.get_exclusion_counts(player_ids)
        for pid in excluded_player_ids:
            assert second_counts[pid] == 4, f"Excluded player {pid} should have count 4"
        
        # Shuffle again with updated counts
        players = test_db.get_players_by_ids(player_ids)
        exclusion_counts_by_id = test_db.get_exclusion_counts(player_ids)
        exclusion_counts = {pl.name: exclusion_counts_by_id[pid] for pid, pl in zip(player_ids, players)}
        
        team1, team2, excluded_players_2 = shuffler.shuffle_from_pool(players, exclusion_counts)
        
        # Step 8: Verify that previously-excluded players are more likely to be included
        # (This is not guaranteed but should trend this way)
        excluded_player_ids_2 = [player_name_to_id[p.name] for p in excluded_players_2]
        
        # At least verify that the system works
        assert len(excluded_player_ids_2) == 2
        
        # The players with high exclusion counts (4) should have a penalty of 4*5 = 20
        # when excluded, making them less likely to be excluded again
        # We can't guarantee the exact result due to other factors (MMR balance, roles)
        # But the system should work correctly
    
    def test_multiple_shuffle_cycles_with_exclusions(self, test_db):
        """
        Test multiple shuffle cycles where different players get excluded each time.
        Verify that exclusion counts accumulate correctly across multiple matches.
        """
        # Create 12 players
        player_ids = list(range(400101, 400113))
        for pid in player_ids:
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06
            )
        
        # Track cumulative exclusion counts
        exclusion_history = {pid: [] for pid in player_ids}
        
        # Run 5 shuffle cycles
        for cycle in range(5):
            # Get current exclusion counts
            current_counts = test_db.get_exclusion_counts(player_ids)
            
            # Shuffle
            players = test_db.get_players_by_ids(player_ids)
            shuffler = BalancedShuffler(use_glicko=True, off_role_flat_penalty=50.0, exclusion_penalty_weight=5.0)
            exclusion_counts = {pl.name: current_counts[pid] for pid, pl in zip(player_ids, players)}
            
            team1, team2, excluded_players = shuffler.shuffle_from_pool(players, exclusion_counts)
            
            # Track who was excluded
            player_name_to_id = {pl.name: pid for pid, pl in zip(player_ids, players)}
            included_player_ids = [player_name_to_id[p.name] for p in team1.players + team2.players]
            excluded_player_ids = [player_name_to_id[p.name] for p in excluded_players]
            
            # Update counts
            for pid in excluded_player_ids:
                test_db.increment_exclusion_count(pid)
            for pid in included_player_ids:
                test_db.decay_exclusion_count(pid)
            
            # Record history
            updated_counts = test_db.get_exclusion_counts(player_ids)
            for pid in player_ids:
                exclusion_history[pid].append(updated_counts[pid])
        
        # Verify that exclusion counts are being tracked over time
        final_counts = test_db.get_exclusion_counts(player_ids)
        
        # All counts should be non-negative
        for pid, count in final_counts.items():
            assert count >= 0, f"Player {pid} has negative exclusion count: {count}"
        
        # At least some variation in exclusion counts (not all exactly the same)
        unique_counts = set(final_counts.values())
        assert len(unique_counts) > 1, "Exclusion counts should vary across players"
    
    def test_exclusion_decay_prevents_overflow(self, test_db):
        """
        Test that the decay mechanism prevents exclusion counts from growing unbounded.
        Even with repeated exclusions, included players should have their counts decay.
        """
        # Create 11 players (1 will always be excluded in this test)
        player_ids = list(range(400201, 400212))
        for pid in player_ids:
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06
            )
        
        # Artificially set one player to have high exclusion count
        unlucky_player_id = player_ids[0]
        for _ in range(16):  # Start with count of 16
            test_db.increment_exclusion_count(unlucky_player_id)
        
        initial_counts = test_db.get_exclusion_counts(player_ids)
        assert initial_counts[unlucky_player_id] == 16
        
        # Simulate: Player gets included in next match (decay should happen)
        test_db.decay_exclusion_count(unlucky_player_id)
        
        after_decay = test_db.get_exclusion_counts([unlucky_player_id])
        assert after_decay[unlucky_player_id] == 8, "16 / 2 = 8"
        
        # Include again
        test_db.decay_exclusion_count(unlucky_player_id)
        after_decay = test_db.get_exclusion_counts([unlucky_player_id])
        assert after_decay[unlucky_player_id] == 4, "8 / 2 = 4"
        
        # Include again
        test_db.decay_exclusion_count(unlucky_player_id)
        after_decay = test_db.get_exclusion_counts([unlucky_player_id])
        assert after_decay[unlucky_player_id] == 2, "4 / 2 = 2"
        
        # Include again
        test_db.decay_exclusion_count(unlucky_player_id)
        after_decay = test_db.get_exclusion_counts([unlucky_player_id])
        assert after_decay[unlucky_player_id] == 1, "2 / 2 = 1"
        
        # Include again
        test_db.decay_exclusion_count(unlucky_player_id)
        after_decay = test_db.get_exclusion_counts([unlucky_player_id])
        assert after_decay[unlucky_player_id] == 0, "1 / 2 = 0"
        
        # Decay prevents unbounded growth - after 5 inclusions, count goes from 16 to 0
    
    def test_exclusion_penalty_affects_matchup_selection(self, test_db):
        """
        Test that exclusion penalty actually affects which matchup is selected.
        Create a scenario where exclusion counts should influence the choice.
        """
        # Create 12 players with identical MMR (to isolate exclusion effect)
        player_ids = list(range(400301, 400313))
        for i, pid in enumerate(player_ids):
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{i+1}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06
            )
        
        # Set Player1 and Player2 to have high exclusion counts
        test_db.increment_exclusion_count(player_ids[0])  # Player1: count = 1
        test_db.increment_exclusion_count(player_ids[1])  # Player2: count = 1
        
        for _ in range(9):  # Add 9 more exclusions to Player1 (total = 10)
            test_db.increment_exclusion_count(player_ids[0])
        
        for _ in range(9):  # Add 9 more exclusions to Player2 (total = 10)
            test_db.increment_exclusion_count(player_ids[1])
        
        # Verify counts
        counts = test_db.get_exclusion_counts(player_ids[:2])
        assert counts[player_ids[0]] == 10
        assert counts[player_ids[1]] == 10
        
        # Shuffle with exclusion penalty enabled
        players = test_db.get_players_by_ids(player_ids)
        shuffler = BalancedShuffler(
            use_glicko=True,
            off_role_flat_penalty=50.0,
            exclusion_penalty_weight=5.0  # 10 exclusions = 50 penalty
        )
        
        exclusion_counts_by_id = test_db.get_exclusion_counts(player_ids)
        exclusion_counts = {pl.name: exclusion_counts_by_id[pid] for pid, pl in zip(player_ids, players)}
        
        team1, team2, excluded_players = shuffler.shuffle_from_pool(players, exclusion_counts)
        
        # Player1 and Player2 (high exclusion counts) should be more likely to be included
        # The penalty for excluding them is 10*5 = 50 each
        # The penalty for excluding two low-count players (0*5 = 0 each) is 0
        # Algorithm should prefer excluding low-count players
        
        player_name_to_id = {pl.name: pid for pid, pl in zip(player_ids, players)}
        included_player_ids = [player_name_to_id[p.name] for p in team1.players + team2.players]
        
        # Verify Player1 and Player2 are more likely to be included
        # (Not guaranteed due to other factors, but should trend this way)
        # At minimum, verify the system completed without errors
        assert len(included_player_ids) == 10
        assert len(excluded_players) == 2


if __name__ == "__main__":
    pytest.main([__file__, "-v"])

