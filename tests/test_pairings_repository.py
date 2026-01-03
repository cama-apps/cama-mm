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

    def test_best_worst_teammates_no_overlap(self, pairings_repo, player_repo):
        """Test that best and worst teammates never overlap - catches conflation bug."""
        players = list(range(1, 16))
        register_players(player_repo, players)

        # Create player 1's teammate history:
        # Player 2: 4 wins, 0 losses (100%) - should be in BEST only
        # Player 3: 3 wins, 1 loss (75%) - should be in BEST only
        # Player 4: 2 wins, 2 losses (50%) - should be in NEITHER
        # Player 5: 1 win, 3 losses (25%) - should be in WORST only
        # Player 6: 0 wins, 4 losses (0%) - should be in WORST only

        # Games with player 2 (all wins)
        for i in range(4):
            pairings_repo.update_pairings_for_match(
                match_id=100 + i,
                team1_ids=[1, 2, 7, 8, 9],
                team2_ids=[10, 11, 12, 13, 14],
                winning_team=1,
            )

        # Games with player 3 (3 wins, 1 loss)
        for i in range(3):
            pairings_repo.update_pairings_for_match(
                match_id=200 + i,
                team1_ids=[1, 3, 7, 8, 9],
                team2_ids=[10, 11, 12, 13, 14],
                winning_team=1,
            )
        pairings_repo.update_pairings_for_match(
            match_id=203,
            team1_ids=[1, 3, 7, 8, 9],
            team2_ids=[10, 11, 12, 13, 14],
            winning_team=2,
        )

        # Games with player 4 (2 wins, 2 losses = 50%)
        for i in range(2):
            pairings_repo.update_pairings_for_match(
                match_id=300 + i,
                team1_ids=[1, 4, 7, 8, 9],
                team2_ids=[10, 11, 12, 13, 14],
                winning_team=1,
            )
        for i in range(2):
            pairings_repo.update_pairings_for_match(
                match_id=302 + i,
                team1_ids=[1, 4, 7, 8, 9],
                team2_ids=[10, 11, 12, 13, 14],
                winning_team=2,
            )

        # Games with player 5 (1 win, 3 losses)
        pairings_repo.update_pairings_for_match(
            match_id=400,
            team1_ids=[1, 5, 7, 8, 9],
            team2_ids=[10, 11, 12, 13, 14],
            winning_team=1,
        )
        for i in range(3):
            pairings_repo.update_pairings_for_match(
                match_id=401 + i,
                team1_ids=[1, 5, 7, 8, 9],
                team2_ids=[10, 11, 12, 13, 14],
                winning_team=2,
            )

        # Games with player 6 (all losses)
        for i in range(4):
            pairings_repo.update_pairings_for_match(
                match_id=500 + i,
                team1_ids=[1, 6, 7, 8, 9],
                team2_ids=[10, 11, 12, 13, 14],
                winning_team=2,
            )

        best = pairings_repo.get_best_teammates(1, min_games=4, limit=10)
        worst = pairings_repo.get_worst_teammates(1, min_games=4, limit=10)

        best_ids = {t["teammate_id"] for t in best}
        worst_ids = {t["teammate_id"] for t in worst}

        # Critical: best and worst should NEVER overlap
        overlap = best_ids & worst_ids
        assert len(overlap) == 0, f"Best and worst teammates overlap: {overlap}"

        # Player 2 should be in best only (100% win rate)
        assert 2 in best_ids, "Player 2 (100%) should be in best teammates"
        assert 2 not in worst_ids, "Player 2 (100%) should NOT be in worst teammates"

        # Player 4 (50%) should be in NEITHER
        assert 4 not in best_ids, "Player 4 (50%) should NOT be in best teammates"
        assert 4 not in worst_ids, "Player 4 (50%) should NOT be in worst teammates"

        # Player 6 should be in worst only (0% win rate)
        assert 6 in worst_ids, "Player 6 (0%) should be in worst teammates"
        assert 6 not in best_ids, "Player 6 (0%) should NOT be in best teammates"

    def test_best_worst_matchups_no_overlap(self, pairings_repo, player_repo):
        """Test that best and worst matchups never overlap - catches conflation bug."""
        players = list(range(1, 16))
        register_players(player_repo, players)

        # Create player 1's opponent history:
        # Player 10: 4 wins against (100%) - should be in DOMINATES only
        # Player 11: 2 wins, 2 losses (50%) - should be in NEITHER
        # Player 12: 0 wins against (0%) - should be in STRUGGLES only

        # Games against player 10 (always win)
        for i in range(4):
            pairings_repo.update_pairings_for_match(
                match_id=100 + i,
                team1_ids=[1, 2, 3, 4, 5],
                team2_ids=[10, 6, 7, 8, 9],
                winning_team=1,
            )

        # Games against player 11 (split 50/50)
        for i in range(2):
            pairings_repo.update_pairings_for_match(
                match_id=200 + i,
                team1_ids=[1, 2, 3, 4, 5],
                team2_ids=[11, 6, 7, 8, 9],
                winning_team=1,
            )
        for i in range(2):
            pairings_repo.update_pairings_for_match(
                match_id=202 + i,
                team1_ids=[1, 2, 3, 4, 5],
                team2_ids=[11, 6, 7, 8, 9],
                winning_team=2,
            )

        # Games against player 12 (always lose)
        for i in range(4):
            pairings_repo.update_pairings_for_match(
                match_id=300 + i,
                team1_ids=[1, 2, 3, 4, 5],
                team2_ids=[12, 6, 7, 8, 9],
                winning_team=2,
            )

        best = pairings_repo.get_best_matchups(1, min_games=4, limit=10)
        worst = pairings_repo.get_worst_matchups(1, min_games=4, limit=10)

        best_ids = {m["opponent_id"] for m in best}
        worst_ids = {m["opponent_id"] for m in worst}

        # Critical: dominates and struggles should NEVER overlap
        overlap = best_ids & worst_ids
        assert len(overlap) == 0, f"Dominates and struggles overlap: {overlap}"

        # Player 10 should be in dominates only
        assert 10 in best_ids, "Player 10 (100%) should be in dominates"
        assert 10 not in worst_ids, "Player 10 (100%) should NOT be in struggles"

        # Player 11 (50%) should be in NEITHER
        assert 11 not in best_ids, "Player 11 (50%) should NOT be in dominates"
        assert 11 not in worst_ids, "Player 11 (50%) should NOT be in struggles"

        # Player 12 should be in struggles only
        assert 12 in worst_ids, "Player 12 (0%) should be in struggles"
        assert 12 not in best_ids, "Player 12 (0%) should NOT be in dominates"

    def test_win_rate_boundary_filtering(self, pairings_repo, player_repo):
        """Test that exactly 50% win rate appears in neither best nor worst."""
        players = list(range(1, 11))
        register_players(player_repo, players)

        # Player 1 + 2: exactly 50% (2 wins, 2 losses)
        for i in range(2):
            pairings_repo.update_pairings_for_match(
                match_id=100 + i,
                team1_ids=[1, 2, 3, 4, 5],
                team2_ids=[6, 7, 8, 9, 10],
                winning_team=1,
            )
        for i in range(2):
            pairings_repo.update_pairings_for_match(
                match_id=102 + i,
                team1_ids=[1, 2, 3, 4, 5],
                team2_ids=[6, 7, 8, 9, 10],
                winning_team=2,
            )

        # Verify the win rate is exactly 50%
        h2h = pairings_repo.get_head_to_head(1, 2)
        assert h2h["games_together"] == 4
        assert h2h["wins_together"] == 2
        win_rate = h2h["wins_together"] / h2h["games_together"]
        assert win_rate == 0.5

        # 50% should not appear in best (requires > 50%)
        best = pairings_repo.get_best_teammates(1, min_games=4, limit=10)
        best_ids = {t["teammate_id"] for t in best}
        assert 2 not in best_ids, "50% win rate should NOT appear in best teammates"

        # 50% should not appear in worst (requires < 50%)
        worst = pairings_repo.get_worst_teammates(1, min_games=4, limit=10)
        worst_ids = {t["teammate_id"] for t in worst}
        assert 2 not in worst_ids, "50% win rate should NOT appear in worst teammates"

    def test_get_most_played_with(self, pairings_repo, player_repo):
        """Test getting teammates sorted by games played together."""
        players = list(range(1, 20))
        register_players(player_repo, players)

        # Player 1 + 2: 6 games together
        for i in range(6):
            pairings_repo.update_pairings_for_match(
                match_id=100 + i,
                team1_ids=[1, 2, 6, 7, 8],
                team2_ids=[10, 11, 12, 13, 14],
                winning_team=1,
            )

        # Player 1 + 3: 3 games together (different teammates)
        for i in range(3):
            pairings_repo.update_pairings_for_match(
                match_id=200 + i,
                team1_ids=[1, 3, 15, 16, 17],
                team2_ids=[10, 11, 12, 13, 14],
                winning_team=1,
            )

        most_played = pairings_repo.get_most_played_with(1, min_games=3, limit=5)
        assert len(most_played) >= 2
        # Player 2 should be first (6 games vs 3 for player 3)
        assert most_played[0]["teammate_id"] == 2
        assert most_played[0]["games_together"] == 6

    def test_get_most_played_against(self, pairings_repo, player_repo):
        """Test getting opponents sorted by games played against."""
        players = list(range(1, 16))
        register_players(player_repo, players)

        # Player 1 vs 10: 5 games
        for i in range(5):
            pairings_repo.update_pairings_for_match(
                match_id=100 + i,
                team1_ids=[1, 2, 3, 4, 5],
                team2_ids=[10, 11, 12, 13, 14],
                winning_team=1,
            )

        # Player 1 vs 15: 3 games (different opponent set)
        for i in range(3):
            pairings_repo.update_pairings_for_match(
                match_id=200 + i,
                team1_ids=[1, 2, 3, 4, 5],
                team2_ids=[15, 6, 7, 8, 9],
                winning_team=1,
            )

        most_played = pairings_repo.get_most_played_against(1, min_games=3, limit=5)
        assert len(most_played) >= 2
        # Player 10 should be first (5 games vs 3)
        assert most_played[0]["opponent_id"] == 10
        assert most_played[0]["games_against"] == 5

    def test_get_evenly_matched_teammates(self, pairings_repo, player_repo):
        """Test getting teammates with exactly 50% win rate."""
        players = list(range(1, 11))
        register_players(player_repo, players)

        # Player 1 + 2: 4 games, 2 wins (50%)
        for i in range(2):
            pairings_repo.update_pairings_for_match(
                match_id=100 + i,
                team1_ids=[1, 2, 3, 4, 5],
                team2_ids=[6, 7, 8, 9, 10],
                winning_team=1,
            )
        for i in range(2):
            pairings_repo.update_pairings_for_match(
                match_id=102 + i,
                team1_ids=[1, 2, 3, 4, 5],
                team2_ids=[6, 7, 8, 9, 10],
                winning_team=2,
            )

        evenly = pairings_repo.get_evenly_matched_teammates(1, min_games=4, limit=5)
        evenly_ids = {t["teammate_id"] for t in evenly}
        assert 2 in evenly_ids, "Player 2 (50%) should be in evenly matched"

    def test_get_evenly_matched_opponents(self, pairings_repo, player_repo):
        """Test getting opponents with exactly 50% win rate."""
        players = list(range(1, 11))
        register_players(player_repo, players)

        # Player 1 vs 6: 4 games, 2 wins each way (50%)
        for i in range(2):
            pairings_repo.update_pairings_for_match(
                match_id=100 + i,
                team1_ids=[1, 2, 3, 4, 5],
                team2_ids=[6, 7, 8, 9, 10],
                winning_team=1,
            )
        for i in range(2):
            pairings_repo.update_pairings_for_match(
                match_id=102 + i,
                team1_ids=[1, 2, 3, 4, 5],
                team2_ids=[6, 7, 8, 9, 10],
                winning_team=2,
            )

        evenly = pairings_repo.get_evenly_matched_opponents(1, min_games=4, limit=5)
        evenly_ids = {o["opponent_id"] for o in evenly}
        assert 6 in evenly_ids, "Player 6 (50%) should be in evenly matched opponents"

    def test_get_pairing_counts(self, pairings_repo, player_repo):
        """Test getting total counts of unique teammates and opponents."""
        players = list(range(1, 11))
        register_players(player_repo, players)

        # Record a match - player 1 gets 4 teammates and 5 opponents
        pairings_repo.update_pairings_for_match(
            match_id=1,
            team1_ids=[1, 2, 3, 4, 5],
            team2_ids=[6, 7, 8, 9, 10],
            winning_team=1,
        )

        counts = pairings_repo.get_pairing_counts(1, min_games=1)
        assert counts["unique_teammates"] == 4  # Players 2, 3, 4, 5
        assert counts["unique_opponents"] == 5  # Players 6, 7, 8, 9, 10
