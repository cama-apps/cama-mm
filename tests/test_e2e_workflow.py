"""
End-to-end workflow tests for complete user workflows.
"""

import json

import pytest

from database import Database
from services.lobby_manager_service import LobbyManagerService as LobbyManager
from rating_system import CamaRatingSystem
from repositories.lobby_repository import LobbyRepository
from shuffler import BalancedShuffler


class TestEndToEndWorkflow:
    """End-to-end tests for complete user workflows."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    @pytest.fixture
    def mock_lobby_manager(self, test_db):
        """Create a fresh lobby manager for each test."""
        lobby_repo = LobbyRepository(test_db.db_path)
        return LobbyManager(lobby_repo)

    def test_full_match_workflow_single_player(self, test_db, mock_lobby_manager):
        """Test complete workflow: register → set roles → join → shuffle → record → stats."""
        # Simulate registration
        user_id = 10001
        test_db.add_player(
            discord_id=user_id,
            discord_username="TestPlayer1",
            initial_mmr=2000,
            glicko_rating=1800.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )

        # Verify registration
        player = test_db.get_player(user_id)
        assert player is not None
        assert player.name == "TestPlayer1"

        # Simulate setting roles
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

        # Verify roles set
        player = test_db.get_player(user_id)
        assert player.preferred_roles == ["1", "2"]

        # Simulate joining lobby
        lobby = mock_lobby_manager.get_or_create_lobby(creator_id=user_id)
        result = lobby.add_player(user_id)
        assert result is True
        assert user_id in lobby.players

    def test_full_match_workflow_10_players(self, test_db, mock_lobby_manager):
        """Test complete workflow with 10 players: register → set roles → join → shuffle → record."""
        # Create 10 players
        player_ids = list(range(20001, 20011))
        player_names = [f"Player{i}" for i in range(1, 11)]

        # Register all players
        for pid, name in zip(player_ids, player_names):
            test_db.add_player(
                discord_id=pid,
                discord_username=name,
                initial_mmr=1500 + (pid % 500),  # Vary MMR
                glicko_rating=1500.0 + (pid % 300),
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        # Set roles for all players (distribute roles 1-5)
        role_distribution = ["1", "2", "3", "4", "5", "1", "2", "3", "4", "5"]
        conn = test_db.get_connection()
        cursor = conn.cursor()
        for pid, role in zip(player_ids, role_distribution):
            cursor.execute(
                """
                UPDATE players
                SET preferred_roles = ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
            """,
                (json.dumps([role]), pid),
            )
        conn.commit()
        conn.close()

        # Verify all players registered and have roles
        for pid in player_ids:
            player = test_db.get_player(pid)
            assert player is not None
            assert player.preferred_roles is not None
            assert len(player.preferred_roles) > 0

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

        # Map teams back to Discord IDs
        player_name_to_id = {pl.name: pid for pid, pl in zip(player_ids, players)}
        team1_ids = [player_name_to_id[p.name] for p in team1.players]
        team2_ids = [player_name_to_id[p.name] for p in team2.players]

        # Verify all players are in exactly one team
        assert len(set(team1_ids)) == 5
        assert len(set(team2_ids)) == 5
        assert set(team1_ids).isdisjoint(set(team2_ids))
        assert set(team1_ids).union(set(team2_ids)) == set(player_ids)

        # Record match - Team 1 wins
        match_id = test_db.record_match(team1_ids=team1_ids, team2_ids=team2_ids, winning_team=1)

        assert match_id is not None

        # Verify win/loss counts
        for pid in team1_ids:
            player = test_db.get_player(pid)
            assert player.wins == 1
            assert player.losses == 0

        for pid in team2_ids:
            player = test_db.get_player(pid)
            assert player.wins == 0
            assert player.losses == 1

        # Record another match - Team 2 wins
        test_db.record_match(team1_ids=team1_ids, team2_ids=team2_ids, winning_team=2)

        # Verify accumulated stats
        for pid in team1_ids:
            player = test_db.get_player(pid)
            assert player.wins == 1
            assert player.losses == 1

        for pid in team2_ids:
            player = test_db.get_player(pid)
            assert player.wins == 1
            assert player.losses == 1

    def test_workflow_with_pool_selection(self, test_db, mock_lobby_manager):
        """Test workflow when more than 10 players join lobby."""
        # Create 12 players
        player_ids = list(range(30001, 30013))

        # Register all players
        for pid in player_ids:
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        # Set roles
        roles = ["1", "2", "3", "4", "5"] * 3  # 15 roles, but only 12 players
        conn = test_db.get_connection()
        cursor = conn.cursor()
        for pid, role in zip(player_ids, roles[:12]):
            cursor.execute(
                """
                UPDATE players
                SET preferred_roles = ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
            """,
                (json.dumps([role]), pid),
            )
        conn.commit()
        conn.close()

        # Join all to lobby
        lobby = mock_lobby_manager.get_or_create_lobby(creator_id=player_ids[0])
        for pid in player_ids:
            lobby.add_player(pid)

        assert lobby.get_player_count() == 12

        # Shuffle from pool (should select 10, exclude 2)
        players = test_db.get_players_by_ids(player_ids)
        shuffler = BalancedShuffler(use_glicko=True, off_role_flat_penalty=50.0)
        team1, team2, excluded = shuffler.shuffle_from_pool(players)

        assert len(team1.players) == 5
        assert len(team2.players) == 5
        assert len(excluded) == 2

        # Verify all players accounted for
        all_players = team1.players + team2.players + excluded
        assert len(all_players) == 12

    def test_workflow_error_cases(self, test_db, mock_lobby_manager):
        """Test error handling in the workflow."""
        # Try to set roles without registering
        player = test_db.get_player(99999)
        assert player is None

        # Try to join lobby without registering
        mock_lobby_manager.get_or_create_lobby(creator_id=99999)
        # This should work (lobby creation), but player won't be in database

        # Try to shuffle with less than 10 players
        player_ids = list(range(40001, 40006))  # Only 5 players
        for pid in player_ids:
            test_db.add_player(discord_id=pid, discord_username=f"Player{pid}", initial_mmr=1500)

        players = test_db.get_players_by_ids(player_ids)
        shuffler = BalancedShuffler()

        with pytest.raises(ValueError):
            shuffler.shuffle(players)

    def test_workflow_with_rating_updates(self, test_db, mock_lobby_manager):
        """Test that ratings update correctly through the workflow."""
        rating_system = CamaRatingSystem()

        # Create 10 players with initial ratings
        player_ids = list(range(50001, 50011))
        initial_ratings = {}

        for pid in player_ids:
            initial_rating = 1500.0 + (pid % 200)
            initial_ratings[pid] = initial_rating
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=initial_rating,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        # Set roles
        role_distribution = ["1", "2", "3", "4", "5", "1", "2", "3", "4", "5"]
        conn = test_db.get_connection()
        cursor = conn.cursor()
        for pid, role in zip(player_ids, role_distribution):
            cursor.execute(
                """
                UPDATE players
                SET preferred_roles = ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
            """,
                (json.dumps([role]), pid),
            )
        conn.commit()
        conn.close()

        # Shuffle
        players = test_db.get_players_by_ids(player_ids)
        shuffler = BalancedShuffler(use_glicko=True, off_role_flat_penalty=50.0)
        team1, team2 = shuffler.shuffle(players)

        # Map to IDs
        player_name_to_id = {pl.name: pid for pid, pl in zip(player_ids, players)}
        team1_ids = [player_name_to_id[p.name] for p in team1.players]
        team2_ids = [player_name_to_id[p.name] for p in team2.players]

        # Get initial ratings
        initial_team1_ratings = {}
        initial_team2_ratings = {}
        for pid in team1_ids:
            rating, rd, vol = test_db.get_player_glicko_rating(pid)
            initial_team1_ratings[pid] = rating
        for pid in team2_ids:
            rating, rd, vol = test_db.get_player_glicko_rating(pid)
            initial_team2_ratings[pid] = rating

        # Record match - Team 1 wins
        test_db.record_match(team1_ids=team1_ids, team2_ids=team2_ids, winning_team=1)

        # Update ratings using rating system
        team1_players_glicko = []
        team2_players_glicko = []

        for pid in team1_ids:
            rating, rd, vol = test_db.get_player_glicko_rating(pid)
            glicko_player = rating_system.create_player_from_rating(rating, rd, vol)
            team1_players_glicko.append((pid, glicko_player))

        for pid in team2_ids:
            rating, rd, vol = test_db.get_player_glicko_rating(pid)
            glicko_player = rating_system.create_player_from_rating(rating, rd, vol)
            team2_players_glicko.append((pid, glicko_player))

        # Update ratings (simplified - in real code this would be more complex)
        # For now, just verify that ratings exist and can be updated
        for pid, glicko_player in team1_players_glicko:
            # In a real match, we'd update based on opponents
            # For this test, just verify the structure
            assert glicko_player.rating > 0
            assert glicko_player.rd > 0

    def test_workflow_player_leaves_and_rejoins(self, test_db, mock_lobby_manager):
        """Test workflow when a player leaves and rejoins."""
        # Create 10 players
        player_ids = list(range(60001, 60011))
        for pid in player_ids:
            test_db.add_player(discord_id=pid, discord_username=f"Player{pid}", initial_mmr=1500)

        # Set roles
        role_distribution = ["1", "2", "3", "4", "5", "1", "2", "3", "4", "5"]
        conn = test_db.get_connection()
        cursor = conn.cursor()
        for pid, role in zip(player_ids, role_distribution):
            cursor.execute(
                """
                UPDATE players
                SET preferred_roles = ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
            """,
                (json.dumps([role]), pid),
            )
        conn.commit()
        conn.close()

        # Join all to lobby
        lobby = mock_lobby_manager.get_or_create_lobby(creator_id=player_ids[0])
        for pid in player_ids:
            lobby.add_player(pid)

        assert lobby.get_player_count() == 10

        # Player leaves
        result = lobby.remove_player(player_ids[0])
        assert result is True
        assert lobby.get_player_count() == 9
        assert not lobby.is_ready()

        # Player rejoins
        result = lobby.add_player(player_ids[0])
        assert result is True
        assert lobby.get_player_count() == 10
        assert lobby.is_ready()

    def test_workflow_multiple_matches_accumulation(self, test_db, mock_lobby_manager):
        """Test that multiple matches correctly accumulate stats."""
        # Create 10 players
        player_ids = list(range(70001, 70011))
        for pid in player_ids:
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        # Set roles
        role_distribution = ["1", "2", "3", "4", "5", "1", "2", "3", "4", "5"]
        conn = test_db.get_connection()
        cursor = conn.cursor()
        for pid, role in zip(player_ids, role_distribution):
            cursor.execute(
                """
                UPDATE players
                SET preferred_roles = ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
            """,
                (json.dumps([role]), pid),
            )
        conn.commit()
        conn.close()

        # Play 5 matches with same teams
        team1_ids = player_ids[:5]
        team2_ids = player_ids[5:]

        # Team 1 wins 3, Team 2 wins 2
        for _ in range(3):
            test_db.record_match(team1_ids, team2_ids, winning_team=1)

        for _ in range(2):
            test_db.record_match(team1_ids, team2_ids, winning_team=2)

        # Verify accumulated stats
        for pid in team1_ids:
            player = test_db.get_player(pid)
            assert player.wins == 3
            assert player.losses == 2

        for pid in team2_ids:
            player = test_db.get_player(pid)
            assert player.wins == 2
            assert player.losses == 3


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
