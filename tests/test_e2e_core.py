"""
End-to-end core tests: complete user workflows, leaderboard sorting,
win/loss recording, bug scenarios, and bot command database operations.

Sections:
  - Workflow (from test_e2e_workflow.py)
  - Leaderboard (from test_e2e_leaderboard.py)
  - Win/loss flow (from test_e2e_win_loss_flow.py)
  - Bug scenarios (from test_e2e_bug_scenarios.py)
  - Bot commands (from test_e2e_bot_commands.py)
"""

import json

import pytest

# Ensure e2e fixtures (e2e_test_db) are registered for win/loss section.
pytest_plugins = ["tests.conftest_e2e"]

from database import Database
from rating_system import CamaRatingSystem
from repositories.lobby_repository import LobbyRepository
from repositories.player_repository import PlayerRepository
from services.lobby_manager_service import LobbyManagerService as LobbyManager
from services.player_service import PlayerService
from shuffler import BalancedShuffler
from utils.formatting import JOPACOIN_EMOTE

# Legacy Database.add_player() uses guild_id=0 by default; the win/loss-flow
# tests below intentionally exercise that path.
LEGACY_GUILD_ID = 0


# =============================================================================
# Helpers (hoisted from source files)
# =============================================================================


def _set_player_roles(test_db: Database, discord_id: int, roles: list[str]) -> None:
    """Set ``preferred_roles`` via PlayerRepository so the test exercises the
    real persistence path. Hand-rolled UPDATE SQL was silently writing the
    wrong column name when schema changed; routing through the repo means
    a column rename or JSON shape change actually breaks the test."""
    repo = PlayerRepository(test_db.db_path)
    repo.update_roles(discord_id, None, roles)


def _create_players(db: Database, start_id: int = 91001, count: int = 10):
    """Helper used by the win/loss flow section."""
    ids = list(range(start_id, start_id + count))
    for idx, pid in enumerate(ids):
        db.add_player(
            discord_id=pid,
            discord_username=f"E2EPlayer{pid}",
            initial_mmr=1500,
            glicko_rating=1500.0 + idx,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )
    return ids


def _sort_leaderboard_like_command(repo: PlayerRepository):
    """Sort all players the same way the leaderboard command does."""
    with repo.connection() as conn:
        cursor = conn.cursor()
        cursor.execute(
            "SELECT discord_id, discord_username, wins, losses, glicko_rating, "
            "COALESCE(jopacoin_balance, 0) as jopacoin_balance FROM players"
        )
        rows = cursor.fetchall()

    players = []
    for row in rows:
        jopacoin = row["jopacoin_balance"] or 0
        wins = row["wins"] or 0
        rating_value = row["glicko_rating"]
        # The command sorts by jopacoin, then wins, then rating
        players.append((row["discord_id"], jopacoin, wins, rating_value or 0))

    players.sort(key=lambda x: (x[1], x[2], x[3]), reverse=True)
    return players


# =============================================================================
# Section: Workflow (from test_e2e_workflow.py)
# =============================================================================


class TestE2EWorkflow:
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

        # Simulate setting roles via the real persistence path
        _set_player_roles(test_db, user_id, ["1", "2"])

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

        # Set roles for all players (distribute roles 1-5) via the real path
        role_distribution = ["1", "2", "3", "4", "5", "1", "2", "3", "4", "5"]
        for pid, role in zip(player_ids, role_distribution):
            _set_player_roles(test_db, pid, [role])

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

        # Set roles via the real path
        roles = ["1", "2", "3", "4", "5"] * 3  # 15 roles, but only 12 players
        for pid, role in zip(player_ids, roles[:12]):
            _set_player_roles(test_db, pid, [role])

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
        for pid, role in zip(player_ids, role_distribution):
            _set_player_roles(test_db, pid, [role])

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
            team1_players_glicko.append((glicko_player, pid))

        for pid in team2_ids:
            rating, rd, vol = test_db.get_player_glicko_rating(pid)
            glicko_player = rating_system.create_player_from_rating(rating, rd, vol)
            team2_players_glicko.append((glicko_player, pid))

        # Apply the actual Glicko-2 update for team1 winning vs team2 and
        # write the new ratings back. Then assert: winners' ratings strictly
        # increased, losers' strictly decreased, RD strictly decreased for
        # everyone (we just played a match), and the magnitude is non-trivial.
        team1_updated, team2_updated = rating_system.update_ratings_after_match(
            team1_players_glicko, team2_players_glicko, 1,
        )
        for new_rating, new_rd, new_vol, pid in team1_updated:
            test_db.update_player_glicko_rating(pid, new_rating, new_rd, new_vol)
        for new_rating, new_rd, new_vol, pid in team2_updated:
            test_db.update_player_glicko_rating(pid, new_rating, new_rd, new_vol)

        for pid in team1_ids:
            new_rating, new_rd, _ = test_db.get_player_glicko_rating(pid)
            assert new_rating > initial_team1_ratings[pid], (
                f"Winner {pid} rating did not increase"
            )
            assert new_rd < 350.0, f"Winner {pid} RD did not shrink after a match"

        for pid in team2_ids:
            new_rating, new_rd, _ = test_db.get_player_glicko_rating(pid)
            assert new_rating < initial_team2_ratings[pid], (
                f"Loser {pid} rating did not decrease"
            )
            assert new_rd < 350.0, f"Loser {pid} RD did not shrink after a match"

    def test_workflow_player_leaves_and_rejoins(self, test_db, mock_lobby_manager):
        """Test workflow when a player leaves and rejoins."""
        # Create 10 players
        player_ids = list(range(60001, 60011))
        for pid in player_ids:
            test_db.add_player(discord_id=pid, discord_username=f"Player{pid}", initial_mmr=1500)

        # Set roles
        role_distribution = ["1", "2", "3", "4", "5", "1", "2", "3", "4", "5"]
        for pid, role in zip(player_ids, role_distribution):
            _set_player_roles(test_db, pid, [role])

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

        # Set roles via the real path
        role_distribution = ["1", "2", "3", "4", "5", "1", "2", "3", "4", "5"]
        for pid, role in zip(player_ids, role_distribution):
            _set_player_roles(test_db, pid, [role])

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


# =============================================================================
# Section: Leaderboard (from test_e2e_leaderboard.py)
# =============================================================================


class TestE2ELeaderboardEdgeCases:
    """Tests for leaderboard edge cases."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    def test_empty_leaderboard(self, test_db):
        """Test leaderboard with no players."""
        # Get all players (should be empty)
        conn = test_db.get_connection()
        cursor = conn.cursor()
        cursor.execute("SELECT * FROM players")
        players = cursor.fetchall()
        conn.close()

        assert len(players) == 0, "Should have no players"

    def test_leaderboard_with_ties(self, test_db):
        """Test leaderboard with players having same jopacoin balance."""
        # Create players with same jopacoin but different ratings
        player_ids = [300001, 300002, 300003]
        player_names = ["Player1", "Player2", "Player3"]

        # All have same jopacoin balance but different ratings
        for pid, name in zip(player_ids, player_names):
            test_db.add_player(
                discord_id=pid,
                discord_username=name,
                initial_mmr=1500,
                glicko_rating=1500.0 + (pid % 500),  # Different ratings
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        # Set same jopacoin balance for all players
        conn = test_db.get_connection()
        cursor = conn.cursor()
        for pid in player_ids:
            cursor.execute("UPDATE players SET jopacoin_balance = 100 WHERE discord_id = ?", (pid,))
        conn.commit()
        conn.close()

        # Get all players and sort by jopacoin, then rating
        rating_system = CamaRatingSystem()

        # Track players with their discord_id
        players_with_ids = []
        for pid in player_ids:
            player = test_db.get_player(pid)
            if player:
                players_with_ids.append((player, pid))

        # Sort by jopacoin (descending), then wins (descending), then rating (descending)
        players_with_stats = []
        for player, pid in players_with_ids:
            total_games = player.wins + player.losses
            win_rate = (player.wins / total_games * 100) if total_games > 0 else 0.0
            cama_rating = (
                rating_system.rating_to_display(player.glicko_rating)
                if player.glicko_rating
                else None
            )
            jopacoin_balance = test_db.get_player_balance(pid)
            players_with_stats.append(
                (player, jopacoin_balance, player.wins, player.losses, win_rate, cama_rating)
            )

        players_with_stats.sort(
            key=lambda x: (x[1], x[2], x[5] if x[5] is not None else 0), reverse=True
        )

        # All should have 100 jopacoin (tied)
        for player, jopacoin, _wins, _losses, win_rate, _rating in players_with_stats:
            assert jopacoin == 100, (
                f"All players should have 100 jopacoin, {player.name} has {jopacoin}"
            )

        # Should be sorted by wins, then rating as tiebreaker (all have same jopacoin)
        # Since all have same wins from the matches, should be sorted by rating
        ratings = [r for _, _, _, _, _, r in players_with_stats if r is not None]
        assert ratings == sorted(ratings, reverse=True), (
            "Should be sorted by rating when jopacoin and wins are tied"
        )

    def test_leaderboard_with_no_games(self, test_db):
        """Test leaderboard with players who have no games."""
        # Create players with no matches
        player_ids = [300101, 300102]
        for pid in player_ids:
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        # Get players and calculate stats
        rating_system = CamaRatingSystem()

        all_players = []
        for pid in player_ids:
            player = test_db.get_player(pid)
            if player:
                all_players.append(player)

        players_with_stats = []
        for player in all_players:
            total_games = player.wins + player.losses
            win_rate = (player.wins / total_games * 100) if total_games > 0 else 0.0
            cama_rating = (
                rating_system.rating_to_display(player.glicko_rating)
                if player.glicko_rating
                else None
            )
            players_with_stats.append((player, player.wins, player.losses, win_rate, cama_rating))

        # All should have 0 wins, 0 losses, 0% win rate
        for player, wins, losses, win_rate, _rating in players_with_stats:
            assert wins == 0, f"Player {player.name} should have 0 wins"
            assert losses == 0, f"Player {player.name} should have 0 losses"
            assert win_rate == 0.0, f"Player {player.name} should have 0% win rate"

    def test_leaderboard_large_dataset(self, test_db):
        """Test leaderboard with many players (top 20 limit)."""
        # Create 25 players
        player_ids = list(range(300201, 300226))
        for pid in player_ids:
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0 + (pid % 1000),
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )

        # Give them varying jopacoin balances
        conn = test_db.get_connection()
        cursor = conn.cursor()
        for i, pid in enumerate(player_ids):
            # Give jopacoin based on index (higher index = more jopacoin)
            jopacoin_balance = i * 10
            cursor.execute(
                "UPDATE players SET jopacoin_balance = ? WHERE discord_id = ?",
                (jopacoin_balance, pid),
            )
        conn.commit()
        conn.close()

        # Get all players and sort
        rating_system = CamaRatingSystem()

        conn = test_db.get_connection()
        cursor = conn.cursor()
        cursor.execute("SELECT * FROM players WHERE discord_id >= 300201 AND discord_id < 300226")
        rows = cursor.fetchall()
        conn.close()

        # Track players with their discord_id
        players_with_ids = []
        for row in rows:
            player = test_db.get_player(row["discord_id"])
            if player:
                players_with_ids.append((player, row["discord_id"]))

        players_with_stats = []
        for player, pid in players_with_ids:
            total_games = player.wins + player.losses
            win_rate = (player.wins / total_games * 100) if total_games > 0 else 0.0
            cama_rating = (
                rating_system.rating_to_display(player.glicko_rating)
                if player.glicko_rating
                else None
            )
            jopacoin_balance = test_db.get_player_balance(pid)
            players_with_stats.append(
                (player, jopacoin_balance, player.wins, player.losses, win_rate, cama_rating)
            )

        players_with_stats.sort(
            key=lambda x: (x[1], x[2], x[5] if x[5] is not None else 0), reverse=True
        )

        # Should have 25 players
        assert len(players_with_stats) == 25, "Should have 25 players"

        # Top player should have most jopacoin
        top_player_jopacoin = players_with_stats[0][1]
        assert top_player_jopacoin >= players_with_stats[-1][1], (
            "Top player should have at least as many jopacoin as bottom"
        )

    def test_leaderboard_sorts_by_jopacoin_then_wins_then_rating(self, test_db):
        """Test that leaderboard sorts correctly: jopacoin -> wins -> rating."""
        from rating_system import CamaRatingSystem

        rating_system = CamaRatingSystem()

        # Create players with different combinations of jopacoin, wins, and ratings
        # Player 1: High jopacoin, low wins, high rating
        # Player 2: High jopacoin, high wins, low rating
        # Player 3: Low jopacoin, high wins, high rating
        # Player 4: Same jopacoin as Player 1, same wins, different rating
        players = [
            {"id": 400001, "name": "Player1", "jopacoin": 100, "wins": 1, "rating": 2000.0},
            {"id": 400002, "name": "Player2", "jopacoin": 100, "wins": 5, "rating": 1500.0},
            {"id": 400003, "name": "Player3", "jopacoin": 50, "wins": 10, "rating": 2000.0},
            {"id": 400004, "name": "Player4", "jopacoin": 100, "wins": 1, "rating": 1800.0},
        ]

        for p in players:
            test_db.add_player(
                discord_id=p["id"],
                discord_username=p["name"],
                initial_mmr=1500,
                glicko_rating=p["rating"],
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )
            # Set wins and jopacoin
            conn = test_db.get_connection()
            cursor = conn.cursor()
            cursor.execute(
                "UPDATE players SET wins = ?, jopacoin_balance = ? WHERE discord_id = ?",
                (p["wins"], p["jopacoin"], p["id"]),
            )
            conn.commit()
            conn.close()

        # Simulate the leaderboard command logic
        with test_db.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT discord_id, discord_username, wins, losses, glicko_rating, COALESCE(jopacoin_balance, 0) as jopacoin_balance "
                "FROM players WHERE discord_id >= 400001 AND discord_id < 400005"
            )
            rows = cursor.fetchall()

        players_with_stats = []
        for row in rows:
            wins = row["wins"] or 0
            rating_value = row["glicko_rating"]
            cama_rating = (
                rating_system.rating_to_display(rating_value) if rating_value is not None else None
            )
            jopacoin_balance = row["jopacoin_balance"] or 0

            players_with_stats.append(
                {
                    "discord_id": row["discord_id"],
                    "username": row["discord_username"],
                    "wins": wins,
                    "rating": cama_rating,
                    "jopacoin_balance": jopacoin_balance,
                }
            )

        # Sort exactly like the leaderboard command does
        players_with_stats.sort(
            key=lambda x: (
                x["jopacoin_balance"],
                x["wins"],
                x["rating"] if x["rating"] is not None else 0,
            ),
            reverse=True,
        )

        # Expected order:
        # 1. Player2: 100 jopacoin, 5 wins, 1500 rating
        # 2. Player1: 100 jopacoin, 1 win, 2000 rating (higher rating than Player4)
        # 3. Player4: 100 jopacoin, 1 win, 1800 rating
        # 4. Player3: 50 jopacoin, 10 wins, 2000 rating

        assert len(players_with_stats) == 4, "Should have 4 players"

        # Top player should be Player2 (highest jopacoin, then highest wins)
        assert players_with_stats[0]["discord_id"] == 400002, (
            "Player2 should be first (100 jopacoin, 5 wins)"
        )
        assert players_with_stats[0]["jopacoin_balance"] == 100
        assert players_with_stats[0]["wins"] == 5

        # Second should be Player1 (100 jopacoin, 1 win, higher rating than Player4)
        assert players_with_stats[1]["discord_id"] == 400001, (
            "Player1 should be second (100 jopacoin, 1 win, 2000 rating)"
        )
        assert players_with_stats[1]["jopacoin_balance"] == 100
        assert players_with_stats[1]["wins"] == 1

        # Third should be Player4 (100 jopacoin, 1 win, lower rating than Player1)
        assert players_with_stats[2]["discord_id"] == 400004, (
            "Player4 should be third (100 jopacoin, 1 win, 1800 rating)"
        )
        assert players_with_stats[2]["jopacoin_balance"] == 100
        assert players_with_stats[2]["wins"] == 1

        # Fourth should be Player3 (50 jopacoin, even though has more wins)
        assert players_with_stats[3]["discord_id"] == 400003, (
            "Player3 should be fourth (50 jopacoin, despite 10 wins)"
        )
        assert players_with_stats[3]["jopacoin_balance"] == 50
        assert players_with_stats[3]["wins"] == 10

    def test_leaderboard_displays_jopacoin(self, test_db):
        """Test that leaderboard output includes jopacoin display."""
        from rating_system import CamaRatingSystem

        rating_system = CamaRatingSystem()

        # Create a test player
        player_id = 400101
        test_db.add_player(
            discord_id=player_id,
            discord_username="TestPlayer",
            initial_mmr=1500,
            glicko_rating=1600.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )

        # Set jopacoin balance
        conn = test_db.get_connection()
        cursor = conn.cursor()
        cursor.execute(
            "UPDATE players SET jopacoin_balance = 42 WHERE discord_id = ?", (player_id,)
        )
        conn.commit()
        conn.close()

        # Simulate the leaderboard command logic
        with test_db.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT discord_id, discord_username, wins, losses, glicko_rating, COALESCE(jopacoin_balance, 0) as jopacoin_balance "
                "FROM players WHERE discord_id = ?",
                (player_id,),
            )
            row = cursor.fetchone()

        wins = row["wins"] or 0
        losses = row["losses"] or 0
        total_games = wins + losses
        win_rate = (wins / total_games * 100) if total_games > 0 else 0.0
        rating_value = row["glicko_rating"]
        (rating_system.rating_to_display(rating_value) if rating_value is not None else None)
        jopacoin_balance = row["jopacoin_balance"] or 0

        # Format like the leaderboard command does
        stats = f"{wins}-{losses}"
        if wins + losses > 0:
            stats += f" ({win_rate:.0f}%)"
        jopacoin_display = f"{jopacoin_balance} {JOPACOIN_EMOTE}"

        # Verify jopacoin is in the display
        assert JOPACOIN_EMOTE in jopacoin_display, (
            "Jopacoin display should include the jopacoin emote"
        )
        assert str(jopacoin_balance) in jopacoin_display, (
            f"Jopacoin display should include balance {jopacoin_balance}"
        )
        assert jopacoin_balance == 42, "Jopacoin balance should be 42"

    def test_leaderboard_with_zero_jopacoin(self, test_db):
        """Test leaderboard with players having zero jopacoin."""
        from rating_system import CamaRatingSystem

        rating_system = CamaRatingSystem()

        # Create players with zero jopacoin but different wins
        player_ids = [400201, 400202, 400203]
        for i, pid in enumerate(player_ids):
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0 + i * 100,
                glicko_rd=350.0,
                glicko_volatility=0.06,
            )
            # Set wins but zero jopacoin
            conn = test_db.get_connection()
            cursor = conn.cursor()
            cursor.execute(
                "UPDATE players SET wins = ?, jopacoin_balance = 0 WHERE discord_id = ?",
                (i + 1, pid),  # Player1: 1 win, Player2: 2 wins, Player3: 3 wins
            )
            conn.commit()
            conn.close()

        # Simulate leaderboard sorting
        with test_db.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT discord_id, discord_username, wins, losses, glicko_rating, COALESCE(jopacoin_balance, 0) as jopacoin_balance "
                "FROM players WHERE discord_id >= 400201 AND discord_id < 400204"
            )
            rows = cursor.fetchall()

        players_with_stats = []
        for row in rows:
            wins = row["wins"] or 0
            rating_value = row["glicko_rating"]
            cama_rating = (
                rating_system.rating_to_display(rating_value) if rating_value is not None else None
            )
            jopacoin_balance = row["jopacoin_balance"] or 0

            players_with_stats.append(
                {
                    "discord_id": row["discord_id"],
                    "wins": wins,
                    "rating": cama_rating,
                    "jopacoin_balance": jopacoin_balance,
                }
            )

        players_with_stats.sort(
            key=lambda x: (
                x["jopacoin_balance"],
                x["wins"],
                x["rating"] if x["rating"] is not None else 0,
            ),
            reverse=True,
        )

        # All have 0 jopacoin, so should be sorted by wins (descending)
        # Player3 (3 wins) -> Player2 (2 wins) -> Player1 (1 win)
        assert players_with_stats[0]["discord_id"] == 400203, "Player3 should be first (3 wins)"
        assert players_with_stats[1]["discord_id"] == 400202, "Player2 should be second (2 wins)"
        assert players_with_stats[2]["discord_id"] == 400201, "Player1 should be third (1 win)"

        # All should have 0 jopacoin
        for player in players_with_stats:
            assert player["jopacoin_balance"] == 0, "All players should have 0 jopacoin"


# =============================================================================
# Section: Win/loss flow (from test_e2e_win_loss_flow.py)
# =============================================================================


def test_record_to_stats_and_leaderboard_flow(e2e_test_db):
    """End-to-end record → stats → leaderboard pass for a single match."""
    player_ids = _create_players(e2e_test_db)
    radiant = player_ids[:5]
    dire = player_ids[5:]

    # Record a Radiant win
    e2e_test_db.record_match(
        radiant_team_ids=radiant,
        dire_team_ids=dire,
        winning_team="radiant",
    )

    repo = PlayerRepository(e2e_test_db.db_path)
    service = PlayerService(repo)

    winner_stats = service.get_stats(radiant[0], guild_id=LEGACY_GUILD_ID)
    loser_stats = service.get_stats(dire[0], guild_id=LEGACY_GUILD_ID)

    assert winner_stats["player"].wins == 1
    assert winner_stats["player"].losses == 0
    assert winner_stats["win_rate"] == pytest.approx(100.0)

    assert loser_stats["player"].wins == 0
    assert loser_stats["player"].losses == 1
    assert loser_stats["win_rate"] == pytest.approx(0.0)

    # Leaderboard should place winners above losers when jopacoin is equal
    leaderboard = _sort_leaderboard_like_command(repo)
    top_wins = [entry[2] for entry in leaderboard[:5]]
    bottom_wins = [entry[2] for entry in leaderboard[5:]]

    assert all(w == 1 for w in top_wins)
    assert all(w == 0 for w in bottom_wins)


def test_multi_match_accumulation_and_nonparticipant(e2e_test_db):
    """Multi-match accumulation; non-participant bench player should stay at 0-0."""
    player_ids = _create_players(e2e_test_db, start_id=92001)
    radiant = player_ids[:5]
    dire = player_ids[5:]
    bench = 93001
    e2e_test_db.add_player(
        discord_id=bench,
        discord_username="BenchPlayer",
        initial_mmr=1500,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )

    # Two matches, alternating winners
    e2e_test_db.record_match(radiant_team_ids=radiant, dire_team_ids=dire, winning_team="radiant")
    e2e_test_db.record_match(radiant_team_ids=radiant, dire_team_ids=dire, winning_team="dire")

    repo = PlayerRepository(e2e_test_db.db_path)
    service = PlayerService(repo)

    for pid in radiant + dire:
        stats = service.get_stats(pid, guild_id=LEGACY_GUILD_ID)
        assert stats["player"].wins == 1
        assert stats["player"].losses == 1
        assert stats["win_rate"] == pytest.approx(50.0)

    bench_stats = service.get_stats(bench, guild_id=LEGACY_GUILD_ID)
    assert bench_stats["player"].wins == 0
    assert bench_stats["player"].losses == 0
    assert bench_stats["win_rate"] is None


# =============================================================================
# Section: Bug scenarios (from test_e2e_bug_scenarios.py)
# =============================================================================


class TestE2EExactBugScenario:
    """Test the exact bug scenario reported by user - Dire wins but recorded as loss."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

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
                glicko_volatility=0.06,
            )

        # Step 2: Simulate shuffle output (as stored in bot.last_shuffle)
        # This is what the shuffle command creates
        radiant_team_ids = player_ids[:5]  # First 5 = Radiant
        dire_team_ids = player_ids[5:]  # Last 5 = Dire

        # Team number assignment (randomly assigned in real shuffle)
        radiant_team_num = 1
        dire_team_num = 2

        # Step 3: Simulate /record command with "Dire won"
        winning_team_num = dire_team_num

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
            team1_ids=team1_ids_for_db, team2_ids=team2_ids_for_db, winning_team=winning_team_for_db
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
                assert wins == 1, (
                    f"BUG FIX: BugReporter should have 1 win (Dire won), got {wins}. "
                    f"This is the exact bug scenario that was reported!"
                )
                assert losses == 0, (
                    f"BUG FIX: BugReporter should have 0 losses (Dire won), got {losses}"
                )
                assert win_rate == 100.0, (
                    f"BUG FIX: BugReporter should have 100% win rate, got {win_rate:.1f}%"
                )
                break

        assert reporter_found, "BugReporter not found in leaderboard results"

        # Verify all Dire players have correct stats
        dire_names = [
            "BugReporter",
            "FakeUser923487",
            "FakeUser921510",
            "FakeUser920053",
            "FakeUser919197",
        ]
        for player, wins, losses, win_rate, rating in players_with_stats:
            if player.name in dire_names:
                assert wins == 1, f"Dire player {player.name} should have 1 win, got {wins}"
                assert losses == 0, f"Dire player {player.name} should have 0 losses, got {losses}"

        # Verify all Radiant players have correct stats
        radiant_names = [
            "FakeUser917762",
            "FakeUser924119",
            "FakeUser926408",
            "FakeUser921765",
            "FakeUser925589",
        ]
        for player, wins, losses, win_rate, rating in players_with_stats:
            if player.name in radiant_names:
                assert wins == 0, f"Radiant player {player.name} should have 0 wins, got {wins}"
                assert losses == 1, f"Radiant player {player.name} should have 1 loss, got {losses}"

        # Verify leaderboard order matches expected (winners on top)
        # In the bug report, winners were on top but with wrong stats
        # After fix, winners should be on top with correct stats
        top_5 = players_with_stats[:5]
        bottom_5 = players_with_stats[5:]

        # All top 5 should have 1 win (Dire players)
        for player, wins, losses, win_rate, rating in top_5:
            assert wins == 1, f"Top 5 player {player.name} should have 1 win, got {wins}"

        # All bottom 5 should have 0 wins (Radiant players)
        for player, wins, losses, win_rate, rating in bottom_5:
            assert wins == 0, f"Bottom 5 player {player.name} should have 0 wins, got {wins}"


class TestE2EProductionBugScenarios:
    """End-to-end tests for actual production bugs reported by users."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

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
                glicko_volatility=0.06,
            )

        # Simulate shuffle_from_pool: 10 players selected, 2 excluded
        # In real scenario, the algorithm selects the best 10
        # For this test, we'll simulate: first 10 in match, last 2 excluded
        match_player_ids = player_ids[:10]
        excluded_player_ids = player_ids[10:]  # BugReporter and FakeUser547520

        # Verify excluded players
        assert len(excluded_player_ids) == 2
        reporter_id = next(
            pid
            for pid, name in zip(player_ids, [p[0] for p in player_data])
            if name == "BugReporter"
        )
        assert reporter_id in excluded_player_ids, "BugReporter should be excluded"

        # Split match players into teams (simulate shuffle result)
        radiant_team_ids = match_player_ids[:5]
        dire_team_ids = match_player_ids[5:10]

        # Verify excluded players are NOT in match
        all_match_ids = set(radiant_team_ids + dire_team_ids)
        excluded_set = set(excluded_player_ids)
        assert all_match_ids.isdisjoint(excluded_set), (
            "Excluded players should not be in match teams"
        )

        # Simulate team number assignment
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
        assert len(excluded_in_match) == 0, (
            f"BUG: Excluded players found in match: {excluded_in_match}"
        )

        # Record the match
        match_id = test_db.record_match(
            team1_ids=team1_ids_for_db, team2_ids=team2_ids_for_db, winning_team=winning_team_for_db
        )

        assert match_id is not None

        # CRITICAL TEST: Excluded players should have 0-0
        for excluded_id in excluded_player_ids:
            player = test_db.get_player(excluded_id)
            player_name = player.name if player else f"Unknown({excluded_id})"
            assert player.wins == 0, (
                f"BUG: Excluded player {player_name} should have 0 wins, got {player.wins}"
            )
            assert player.losses == 0, (
                f"BUG: Excluded player {player_name} should have 0 losses, got {player.losses}. "
                f"This is the exact bug that was reported!"
            )

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
                glicko_volatility=0.06,
            )

        # Simulate shuffle: 10 players in match, 2 excluded
        # Based on the shuffle output from production:
        # Radiant: FakeUser542744, BugReporter, FakeUser548931, FakeUser545142, FakeUser541518
        # Dire: FakeUser551025, FakeUser543693, FakeUser546016, FakeUser539516, FakeUser549780
        # Excluded: FakeUser547520, FakeUser546625

        # Map names to IDs
        name_to_id = {name: pid for pid, (name, _) in zip(player_ids, player_data)}

        radiant_names = [
            "FakeUser542744",
            "BugReporter",
            "FakeUser548931",
            "FakeUser545142",
            "FakeUser541518",
        ]
        dire_names = [
            "FakeUser551025",
            "FakeUser543693",
            "FakeUser546016",
            "FakeUser539516",
            "FakeUser549780",
        ]
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
        assert all_match_ids.isdisjoint(excluded_set), (
            "Excluded players should not be in match teams"
        )

        # Record match - Radiant won
        radiant_team_num = 1
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
        assert len(excluded_in_final) == 0, (
            f"BUG: Excluded players in final match teams: {excluded_in_final}"
        )

        # Record the match
        match_id = test_db.record_match(
            team1_ids=team1_ids_for_db, team2_ids=team2_ids_for_db, winning_team=winning_team_for_db
        )

        assert match_id is not None

        # CRITICAL TEST: Excluded players should have 0-0
        for excluded_name in excluded_names:
            excluded_id = name_to_id[excluded_name]
            player = test_db.get_player(excluded_id)
            assert player.wins == 0, (
                f"BUG: Excluded player {excluded_name} should have 0 wins, got {player.wins}"
            )
            assert player.losses == 0, (
                f"BUG: Excluded player {excluded_name} should have 0 losses, got {player.losses}"
            )

        # Verify Radiant players (winners) have 1-0
        for radiant_name in radiant_names:
            radiant_id = name_to_id[radiant_name]
            player = test_db.get_player(radiant_id)
            assert player.wins == 1, (
                f"Radiant player {radiant_name} should have 1 win, got {player.wins}"
            )
            assert player.losses == 0, (
                f"Radiant player {radiant_name} should have 0 losses, got {player.losses}"
            )

        # Verify Dire players (losers) have 0-1
        for dire_name in dire_names:
            dire_id = name_to_id[dire_name]
            player = test_db.get_player(dire_id)
            assert player.wins == 0, (
                f"Dire player {dire_name} should have 0 wins, got {player.wins}"
            )
            assert player.losses == 1, (
                f"Dire player {dire_name} should have 1 loss, got {player.losses}"
            )

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
                glicko_volatility=0.06,
            )

        # Match 1: First 10 players, last 2 excluded
        match1_player_ids = player_ids[:10]
        match1_excluded = player_ids[10:]
        match1_radiant = match1_player_ids[:5]
        match1_dire = match1_player_ids[5:10]

        # Record match 1 - Radiant won
        test_db.record_match(team1_ids=match1_radiant, team2_ids=match1_dire, winning_team=1)

        # Verify excluded players still have 0-0
        for excluded_id in match1_excluded:
            player = test_db.get_player(excluded_id)
            assert player.wins == 0 and player.losses == 0, (
                f"After match 1: Excluded player {player.name} should have 0-0"
            )

        # Match 2: Different 10 players (rotate)
        match2_player_ids = player_ids[2:12]  # Skip first 2, include last 2
        match2_excluded = player_ids[:2]  # First 2 now excluded
        match2_radiant = match2_player_ids[:5]
        match2_dire = match2_player_ids[5:10]

        # Record match 2 - Dire won
        test_db.record_match(
            team1_ids=match2_dire,  # Dire goes to team1
            team2_ids=match2_radiant,  # Radiant goes to team2
            winning_team=1,  # team1 (Dire) won
        )

        # Verify previously excluded players (now in match) have correct stats
        for pid in match2_radiant:  # Radiant lost match 2
            player = test_db.get_player(pid)
            if pid in match1_radiant:  # Was in match 1 and won
                assert player.wins == 1 and player.losses == 1, (
                    f"Player {player.name} should have 1-1 (won match 1, lost match 2)"
                )
            elif pid in match1_dire:  # Was in match 1 and lost
                assert player.wins == 0 and player.losses == 2, (
                    f"Player {player.name} should have 0-2 (lost match 1 and match 2)"
                )
            else:  # Was excluded from match 1, new to match 2
                assert player.wins == 0 and player.losses == 1, (
                    f"Player {player.name} should have 0-1 (lost match 2)"
                )

        # Verify newly excluded players still have stats from match 1
        for excluded_id in match2_excluded:
            player = test_db.get_player(excluded_id)
            if excluded_id in match1_radiant:  # Was in match 1
                assert player.wins == 1 and player.losses == 0, (
                    f"Player {player.name} should have 1-0 (won match 1, excluded from match 2)"
                )
            else:  # Was excluded from match 1 too
                assert player.wins == 0 and player.losses == 0, (
                    f"Player {player.name} should have 0-0 (excluded from both matches)"
                )


# =============================================================================
# Section: Bot commands (from test_e2e_bot_commands.py)
# =============================================================================


class TestE2EBotCommands:
    """Test bot commands in isolation (mocked Discord interactions)."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

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
