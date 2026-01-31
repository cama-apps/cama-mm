"""
End-to-end tests based on real-world usage examples.
"""

import pytest

from database import Database
from services.lobby_manager_service import LobbyManagerService as LobbyManager
from rating_system import CamaRatingSystem
from repositories.lobby_repository import LobbyRepository
from shuffler import BalancedShuffler


class TestRealWorldScenario:
    """End-to-end test based on real-world usage example."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    @pytest.fixture
    def mock_lobby_manager(self, test_db):
        """Create a fresh lobby manager for each test."""
        lobby_repo = LobbyRepository(test_db.db_path)
        return LobbyManager(lobby_repo)

    def test_balanced_shuffle_real_world_example(self, test_db, mock_lobby_manager):
        """
        Test based on real-world example:
        - Balanced teams (8043 vs 8045, difference of 2)
        - All players on-role (0 off-role)
        - Dire wins
        - Match recorded successfully
        """
        # Create 10 players matching the real-world example
        # Player data: name, rating (Cama rating), role
        player_data = [
            ("FakeUser172699", 1623, "1"),  # Carry
            ("FakeUser169817", 1018, "2"),  # Mid
            ("FakeUser167858", 1744, "3"),  # Offlane
            ("FakeUser175544", 1822, "4"),  # Soft Support
            ("FakeUser173967", 1836, "5"),  # Hard Support
            ("FakeUser170233", 1590, "1"),  # Carry
            ("FakeUser174788", 1457, "2"),  # Mid
            ("FakeUser171621", 1882, "3"),  # Offlane
            ("FakeUser166664", 1579, "4"),  # Soft Support
            ("FakeUser168472", 1537, "5"),  # Hard Support
        ]

        player_ids = list(range(90001, 90011))

        # Register all players with their ratings
        # Convert Cama rating to Glicko-2 (approximate conversion)
        CamaRatingSystem()
        for pid, (name, cama_rating, role) in zip(player_ids, player_data):
            # Approximate Glicko-2 from Cama rating (simplified)
            # In real system, this would use the rating system conversion
            glicko_rating = cama_rating * 1.0  # Simplified for test
            test_db.add_player(
                discord_id=pid,
                discord_username=name,
                initial_mmr=None,  # No MMR in this example
                glicko_rating=glicko_rating,
                glicko_rd=350.0,
                glicko_volatility=0.06,
                preferred_roles=[role],
            )

        # Verify all players registered
        for pid in player_ids:
            player = test_db.get_player(pid)
            assert player is not None
            assert player.preferred_roles is not None
            assert len(player.preferred_roles) == 1

        # Join all players to lobby
        lobby = mock_lobby_manager.get_or_create_lobby(creator_id=player_ids[0])
        for pid in player_ids:
            result = lobby.add_player(pid)
            assert result is True

        assert lobby.get_player_count() == 10
        assert lobby.is_ready()

        # Get players from database for shuffling
        players = test_db.get_players_by_ids(player_ids)
        assert len(players) == 10

        # Shuffle teams
        shuffler = BalancedShuffler(use_glicko=True, off_role_flat_penalty=50.0)
        team1, team2 = shuffler.shuffle(players)

        assert len(team1.players) == 5
        assert len(team2.players) == 5

        # Calculate team values (using Glicko ratings)
        team1_value = team1.get_team_value(use_glicko=True, off_role_multiplier=1.0)
        team2_value = team2.get_team_value(use_glicko=True, off_role_multiplier=1.0)

        # Verify teams are balanced (difference should be small)
        value_diff = abs(team1_value - team2_value)
        assert value_diff < 100, f"Teams should be balanced, but difference is {value_diff}"

        # Verify all players are on-role (0 off-role players)
        team1_off_roles = team1.get_off_role_count()
        team2_off_roles = team2.get_off_role_count()
        total_off_roles = team1_off_roles + team2_off_roles
        assert total_off_roles == 0, f"Expected 0 off-role players, got {total_off_roles}"

        # Map teams back to Discord IDs
        player_name_to_id = {pl.name: pid for pid, pl in zip(player_ids, players)}
        team1_ids = [player_name_to_id[p.name] for p in team1.players]
        team2_ids = [player_name_to_id[p.name] for p in team2.players]

        # Verify all players are in exactly one team
        assert len(set(team1_ids)) == 5
        assert len(set(team2_ids)) == 5
        assert set(team1_ids).isdisjoint(set(team2_ids))
        assert set(team1_ids).union(set(team2_ids)) == set(player_ids)

        # Simulate Radiant/Dire assignment (random in real system)
        # In this example, Team 1 = Radiant, Team 2 = Dire
        radiant_team_ids = team1_ids
        dire_team_ids = team2_ids

        # Calculate values for display (similar to bot output)
        radiant_value = team1_value
        dire_value = team2_value

        # Verify values are close to the example (8043 vs 8045)
        # Allow some variance due to Glicko-2 calculations
        assert 7000 < radiant_value < 9000, f"Radiant value {radiant_value} should be reasonable"
        assert 7000 < dire_value < 9000, f"Dire value {dire_value} should be reasonable"

        # Record match - Dire wins (Team 2 wins)
        # Map Radiant/Dire to team1/team2 for database
        match_id = test_db.record_match(
            team1_ids=radiant_team_ids,
            team2_ids=dire_team_ids,
            winning_team=2,  # Dire (Team 2) wins
        )

        assert match_id is not None

        # Verify win/loss counts - Dire (Team 2) players should have wins
        for pid in dire_team_ids:
            player = test_db.get_player(pid)
            assert player.wins == 1, f"Dire player {pid} should have 1 win, got {player.wins}"
            assert player.losses == 0, (
                f"Dire player {pid} should have 0 losses, got {player.losses}"
            )

        # Verify Radiant (Team 1) players should have losses
        for pid in radiant_team_ids:
            player = test_db.get_player(pid)
            assert player.wins == 0, f"Radiant player {pid} should have 0 wins, got {player.wins}"
            assert player.losses == 1, (
                f"Radiant player {pid} should have 1 loss, got {player.losses}"
            )

        # Verify match exists in database
        conn = test_db.get_connection()
        cursor = conn.cursor()
        cursor.execute("SELECT winning_team FROM matches WHERE match_id = ?", (match_id,))
        result = cursor.fetchone()
        assert result is not None
        assert result[0] == 2, "Winning team should be 2 (Dire)"

        # Verify match participants
        cursor.execute(
            """
            SELECT discord_id, team_number, won
            FROM match_participants
            WHERE match_id = ?
        """,
            (match_id,),
        )
        participants = cursor.fetchall()
        assert len(participants) == 10, "Should have 10 participants"

        # Verify Dire players (team 2) have won=1
        dire_participants = [p for p in participants if p[1] == 2]
        assert len(dire_participants) == 5
        for pid, _team_num, won in dire_participants:
            assert pid in dire_team_ids
            assert won == 1, f"Dire player {pid} should have won=1"

        # Verify Radiant players (team 1) have won=0
        radiant_participants = [p for p in participants if p[1] == 1]
        assert len(radiant_participants) == 5
        for pid, _team_num, won in radiant_participants:
            assert pid in radiant_team_ids
            assert won == 0, f"Radiant player {pid} should have won=0"

        conn.close()

        # Summary assertions matching the real-world example
        assert value_diff < 10, (
            f"Value difference should be small (like 2 in example), got {value_diff}"
        )
        assert total_off_roles == 0, "All players should be on-role"
        assert match_id is not None, "Match should be recorded"


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
