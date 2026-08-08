"""
End-to-end core tests: complete user workflows, leaderboard sorting,
win/loss recording, and exclusion bug scenarios.

Sections:
  - Workflow (from test_e2e_workflow.py)
  - Leaderboard (from test_e2e_leaderboard.py)
  - Win/loss flow (from test_e2e_win_loss_flow.py)
  - Bug scenarios (from test_e2e_bug_scenarios.py)
"""

import pytest

from rating_system import CamaRatingSystem
from repositories.lobby_repository import LobbyRepository
from repositories.player_repository import PlayerRepository
from services.lobby_manager_service import LobbyManagerService as LobbyManager
from services.player_service import PlayerService
from shuffler import BalancedShuffler
from tests.repository_harness import RepositoryTestDatabase as Database

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


# =============================================================================
# Section: Leaderboard (from test_e2e_leaderboard.py)
# =============================================================================


class TestE2ELeaderboardEdgeCases:
    """Tests for leaderboard edge cases."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

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


# =============================================================================
# Section: Win/loss flow (from test_e2e_win_loss_flow.py)
# =============================================================================


def test_record_to_stats_and_leaderboard_flow(test_db_with_schema):
    """End-to-end record → stats → leaderboard pass for a single match."""
    player_ids = _create_players(test_db_with_schema)
    radiant = player_ids[:5]
    dire = player_ids[5:]

    # Record a Radiant win
    test_db_with_schema.record_match(
        radiant_team_ids=radiant,
        dire_team_ids=dire,
        winning_team="radiant",
    )

    repo = PlayerRepository(test_db_with_schema.db_path)
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


def test_multi_match_accumulation_and_nonparticipant(test_db_with_schema):
    """Multi-match accumulation; non-participant bench player should stay at 0-0."""
    player_ids = _create_players(test_db_with_schema, start_id=92001)
    radiant = player_ids[:5]
    dire = player_ids[5:]
    bench = 93001
    test_db_with_schema.add_player(
        discord_id=bench,
        discord_username="BenchPlayer",
        initial_mmr=1500,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )

    # Two matches, alternating winners
    test_db_with_schema.record_match(radiant_team_ids=radiant, dire_team_ids=dire, winning_team="radiant")
    test_db_with_schema.record_match(radiant_team_ids=radiant, dire_team_ids=dire, winning_team="dire")

    repo = PlayerRepository(test_db_with_schema.db_path)
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


class TestE2EProductionBugScenarios:
    """End-to-end tests for actual production bugs reported by users."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

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


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
