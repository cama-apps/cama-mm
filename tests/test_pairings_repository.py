"""
Tests for PairingsRepository - pairwise player statistics.
"""

import pytest
from repositories.pairings_repository import PairingsRepository
from repositories.player_repository import PlayerRepository
from repositories.match_repository import MatchRepository


@pytest.fixture
def temp_db_path(tmp_path):
    """Create a temporary database path for testing."""
    return str(tmp_path / "test_pairings.db")


@pytest.fixture
def pairings_repo(temp_db_path):
    """Create a PairingsRepository instance."""
    return PairingsRepository(temp_db_path)


@pytest.fixture
def player_repo(temp_db_path):
    """Create a PlayerRepository instance."""
    return PlayerRepository(temp_db_path)


@pytest.fixture
def match_repo(temp_db_path):
    """Create a MatchRepository instance."""
    return MatchRepository(temp_db_path)


def register_players(player_repo, player_ids):
    """Helper to register test players."""
    for pid in player_ids:
        player_repo.add(
            discord_id=pid,
            discord_username=f"Player{pid}",
            initial_mmr=3000,
        )


class TestPairingsRepository:
    """Tests for PairingsRepository."""

    def test_update_pairings_for_match_teammates(self, pairings_repo, player_repo):
        """Test that teammates stats are updated correctly."""
        # Register players
        team1 = [1, 2, 3, 4, 5]
        team2 = [6, 7, 8, 9, 10]
        register_players(player_repo, team1 + team2)

        # Record match with team1 winning
        pairings_repo.update_pairings_for_match(
            match_id=1,
            team1_ids=team1,
            team2_ids=team2,
            winning_team=1,
        )

        # Check teammates on winning team
        h2h = pairings_repo.get_head_to_head(1, 2)
        assert h2h is not None
        assert h2h["games_together"] == 1
        assert h2h["wins_together"] == 1

        # Check teammates on losing team
        h2h_losers = pairings_repo.get_head_to_head(6, 7)
        assert h2h_losers is not None
        assert h2h_losers["games_together"] == 1
        assert h2h_losers["wins_together"] == 0

    def test_update_pairings_for_match_opponents(self, pairings_repo, player_repo):
        """Test that opponent stats are updated correctly."""
        team1 = [1, 2, 3, 4, 5]
        team2 = [6, 7, 8, 9, 10]
        register_players(player_repo, team1 + team2)

        pairings_repo.update_pairings_for_match(
            match_id=1,
            team1_ids=team1,
            team2_ids=team2,
            winning_team=1,
        )

        # Check opponent pairing
        h2h = pairings_repo.get_head_to_head(1, 6)
        assert h2h is not None
        assert h2h["games_against"] == 1
        # Player 1 won against player 6
        assert h2h["queried_player_wins_against"] == 1

        # Check from player 6's perspective
        h2h_6 = pairings_repo.get_head_to_head(6, 1)
        assert h2h_6["queried_player_wins_against"] == 0

    def test_multiple_matches_accumulate(self, pairings_repo, player_repo):
        """Test that multiple matches accumulate stats correctly."""
        team1 = [1, 2, 3, 4, 5]
        team2 = [6, 7, 8, 9, 10]
        register_players(player_repo, team1 + team2)

        # Match 1: team1 wins
        pairings_repo.update_pairings_for_match(
            match_id=1,
            team1_ids=team1,
            team2_ids=team2,
            winning_team=1,
        )

        # Match 2: team2 wins
        pairings_repo.update_pairings_for_match(
            match_id=2,
            team1_ids=team1,
            team2_ids=team2,
            winning_team=2,
        )

        # Match 3: team1 wins again
        pairings_repo.update_pairings_for_match(
            match_id=3,
            team1_ids=team1,
            team2_ids=team2,
            winning_team=1,
        )

        # Check teammates on team1
        h2h = pairings_repo.get_head_to_head(1, 2)
        assert h2h["games_together"] == 3
        assert h2h["wins_together"] == 2  # Won 2 out of 3

        # Check opponent stats
        h2h_opp = pairings_repo.get_head_to_head(1, 6)
        assert h2h_opp["games_against"] == 3
        # Player 1 beat player 6 in matches 1 and 3
        assert h2h_opp["queried_player_wins_against"] == 2

    def test_get_best_teammates(self, pairings_repo, player_repo):
        """Test getting best teammates by win rate."""
        players = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
        register_players(player_repo, players)

        # Create varied teammate histories for player 1
        # Player 1 + 2: 3 games, 3 wins (100%)
        for i in range(3):
            pairings_repo.update_pairings_for_match(
                match_id=100 + i,
                team1_ids=[1, 2, 3, 4, 5],
                team2_ids=[6, 7, 8, 9, 10],
                winning_team=1,
            )

        # Player 1 + 3: already has 3 games from above, add losses
        for i in range(3):
            pairings_repo.update_pairings_for_match(
                match_id=200 + i,
                team1_ids=[1, 3, 6, 7, 8],
                team2_ids=[2, 4, 5, 9, 10],
                winning_team=2,
            )

        best = pairings_repo.get_best_teammates(1, min_games=3, limit=5)
        assert len(best) >= 1
        # Player 2 should be the best teammate for player 1
        assert best[0]["teammate_id"] == 2
        assert best[0]["win_rate"] == 1.0  # 100%

    def test_get_worst_teammates(self, pairings_repo, player_repo):
        """Test getting worst teammates by win rate."""
        players = list(range(1, 11))
        register_players(player_repo, players)

        # Player 1 + 2: 4 games, all losses
        for i in range(4):
            pairings_repo.update_pairings_for_match(
                match_id=100 + i,
                team1_ids=[1, 2, 3, 4, 5],
                team2_ids=[6, 7, 8, 9, 10],
                winning_team=2,
            )

        # Player 1 + 6: 4 games, all wins (opponents)
        for i in range(4):
            pairings_repo.update_pairings_for_match(
                match_id=200 + i,
                team1_ids=[1, 6, 7, 8, 9],
                team2_ids=[2, 3, 4, 5, 10],
                winning_team=1,
            )

        worst = pairings_repo.get_worst_teammates(1, min_games=3, limit=5)
        assert len(worst) >= 1
        # Player 2 should be worst (lost all 4 games together)
        assert worst[0]["teammate_id"] == 2
        assert worst[0]["win_rate"] == 0.0

    def test_get_best_matchups(self, pairings_repo, player_repo):
        """Test getting best matchups (players you dominate)."""
        players = list(range(1, 11))
        register_players(player_repo, players)

        # Player 1 beats player 6 every time
        for i in range(4):
            pairings_repo.update_pairings_for_match(
                match_id=100 + i,
                team1_ids=[1, 2, 3, 4, 5],
                team2_ids=[6, 7, 8, 9, 10],
                winning_team=1,
            )

        best = pairings_repo.get_best_matchups(1, min_games=3, limit=5)
        assert len(best) >= 1
        # All opponents should be dominated equally
        assert best[0]["win_rate"] == 1.0

    def test_get_worst_matchups(self, pairings_repo, player_repo):
        """Test getting worst matchups (players you struggle against)."""
        players = list(range(1, 11))
        register_players(player_repo, players)

        # Player 1 loses to player 6's team every time
        for i in range(4):
            pairings_repo.update_pairings_for_match(
                match_id=100 + i,
                team1_ids=[1, 2, 3, 4, 5],
                team2_ids=[6, 7, 8, 9, 10],
                winning_team=2,
            )

        worst = pairings_repo.get_worst_matchups(1, min_games=3, limit=5)
        assert len(worst) >= 1
        assert worst[0]["win_rate"] == 0.0

    def test_min_games_filter(self, pairings_repo, player_repo):
        """Test that min_games filter works correctly."""
        players = list(range(1, 11))
        register_players(player_repo, players)

        # Only 2 games together
        for i in range(2):
            pairings_repo.update_pairings_for_match(
                match_id=100 + i,
                team1_ids=[1, 2, 3, 4, 5],
                team2_ids=[6, 7, 8, 9, 10],
                winning_team=1,
            )

        # Should return empty with min_games=3
        best = pairings_repo.get_best_teammates(1, min_games=3, limit=5)
        assert len(best) == 0

        # Should return results with min_games=2
        best = pairings_repo.get_best_teammates(1, min_games=2, limit=5)
        assert len(best) > 0

    def test_canonical_ordering(self, pairings_repo, player_repo):
        """Test that player IDs are stored canonically (smaller first)."""
        register_players(player_repo, [100, 200])

        # Record with 200 first in team, 100 second
        pairings_repo.update_pairings_for_match(
            match_id=1,
            team1_ids=[200, 100, 3, 4, 5],
            team2_ids=[6, 7, 8, 9, 10],
            winning_team=1,
        )

        # Should be stored as (100, 200)
        h2h = pairings_repo.get_head_to_head(100, 200)
        assert h2h is not None
        assert h2h["player1_id"] == 100
        assert h2h["player2_id"] == 200

        # Query in reverse order should work too
        h2h_reverse = pairings_repo.get_head_to_head(200, 100)
        assert h2h_reverse is not None
        assert h2h_reverse["player1_id"] == 100

    def test_rebuild_all_pairings(self, pairings_repo, player_repo, match_repo):
        """Test rebuilding pairings from match history."""
        players = list(range(1, 11))
        register_players(player_repo, players)

        # Record some matches in match_repo
        team1 = [1, 2, 3, 4, 5]
        team2 = [6, 7, 8, 9, 10]

        for i in range(3):
            match_repo.record_match(
                team1_ids=team1,
                team2_ids=team2,
                winning_team=1 if i % 2 == 0 else 2,
            )

        # Now rebuild pairings
        count = pairings_repo.rebuild_all_pairings()
        assert count > 0

        # Verify pairings exist
        h2h = pairings_repo.get_head_to_head(1, 2)
        assert h2h is not None
        assert h2h["games_together"] == 3

    def test_get_pairings_for_player(self, pairings_repo, player_repo):
        """Test getting all pairings for a player."""
        players = list(range(1, 11))
        register_players(player_repo, players)

        # Record a match
        pairings_repo.update_pairings_for_match(
            match_id=1,
            team1_ids=[1, 2, 3, 4, 5],
            team2_ids=[6, 7, 8, 9, 10],
            winning_team=1,
        )

        pairings = pairings_repo.get_pairings_for_player(1)
        # Player 1 should have pairings with all 9 other players
        # 4 teammates + 5 opponents = 9
        assert len(pairings) == 9
