"""
Tests for Glicko-2 rating updates after matches.
"""

import pytest

from database import Database
from rating_system import CamaRatingSystem


class TestRatingUpdates:
    """Test Glicko-2 rating updates after matches."""

    @pytest.fixture
    def test_db(self, repo_db_path):
        """Create a test database using centralized fast fixture."""
        return Database(repo_db_path)

    def test_rating_update_after_match(self, test_db):
        """Test that ratings are updated after a match."""
        rating_system = CamaRatingSystem()

        # Create two players with initial ratings
        player1_id = 2001
        player2_id = 2002

        test_db.add_player(
            discord_id=player1_id,
            discord_username="Player1",
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )

        test_db.add_player(
            discord_id=player2_id,
            discord_username="Player2",
            initial_mmr=1500,
            glicko_rating=1500.0,
            glicko_rd=350.0,
            glicko_volatility=0.06,
        )

        # Get initial ratings
        initial_rating1, _, _ = test_db.get_player_glicko_rating(player1_id)
        initial_rating2, _, _ = test_db.get_player_glicko_rating(player2_id)

        # Create Glicko-2 players
        player1_glicko = rating_system.create_player_from_rating(initial_rating1, 350.0, 0.06)
        player2_glicko = rating_system.create_player_from_rating(initial_rating2, 350.0, 0.06)

        # Simulate a match where player1 wins
        # In a 1v1, player1's opponent is player2
        player1_glicko.update_player([player2_glicko.rating], [player2_glicko.rd], [1.0])
        player2_glicko.update_player([player1_glicko.rating], [player1_glicko.rd], [0.0])

        # Update ratings in database
        test_db.update_player_glicko_rating(
            player1_id, player1_glicko.rating, player1_glicko.rd, player1_glicko.vol
        )
        test_db.update_player_glicko_rating(
            player2_id, player2_glicko.rating, player2_glicko.rd, player2_glicko.vol
        )

        # Check that ratings changed
        new_rating1, _, _ = test_db.get_player_glicko_rating(player1_id)
        new_rating2, _, _ = test_db.get_player_glicko_rating(player2_id)

        # Winner's rating should increase, loser's should decrease
        assert new_rating1 > initial_rating1
        assert new_rating2 < initial_rating2


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
