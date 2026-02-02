"""
Tests for streak-based rating adjustments.

Streak-based adjustments apply a delta multiplier to Glicko-2 rating updates
when players are on win/loss streaks of 3+ games. This helps correct ratings
faster when a player's skill has changed.

Multiplier formula: 1.0 + 0.20 * max(0, streak_length - 2)
- Streaks 1-2: 1.00x (normal)
- Streak 3: 1.20x (+20%)
- Streak 4: 1.40x (+40%)
- Streak 5: 1.60x (+60%)
- etc. (uncapped)
"""

import pytest

from config import STREAK_THRESHOLD, STREAK_MULTIPLIER_PER_GAME
from rating_system import CamaRatingSystem


class TestStreakMultiplierCalculation:
    """Tests for streak detection and multiplier calculation."""

    @pytest.fixture
    def rating_system(self):
        return CamaRatingSystem()

    def test_no_streak_returns_multiplier_1(self, rating_system):
        """A single game (no streak history) returns 1.0 multiplier."""
        # No previous games - this is the first game
        recent_outcomes = []
        streak_length, multiplier = rating_system.calculate_streak_multiplier(
            recent_outcomes, won=True
        )
        assert streak_length == 1
        assert multiplier == 1.0

    def test_two_game_streak_returns_multiplier_1(self, rating_system):
        """A 2-game streak returns 1.0 multiplier (threshold is 3)."""
        # 1 previous win + current win = 2-game streak
        recent_outcomes = [True]
        streak_length, multiplier = rating_system.calculate_streak_multiplier(
            recent_outcomes, won=True
        )
        assert streak_length == 2
        assert multiplier == 1.0

    def test_three_game_streak_returns_multiplier_1_20(self, rating_system):
        """A 3-game streak returns 1.20x multiplier."""
        # 2 previous wins + current win = 3-game streak
        recent_outcomes = [True, True]
        streak_length, multiplier = rating_system.calculate_streak_multiplier(
            recent_outcomes, won=True
        )
        assert streak_length == 3
        assert multiplier == pytest.approx(1.20)

    def test_four_game_streak_returns_multiplier_1_40(self, rating_system):
        """A 4-game streak returns 1.40x multiplier."""
        # 3 previous wins + current win = 4-game streak
        recent_outcomes = [True, True, True]
        streak_length, multiplier = rating_system.calculate_streak_multiplier(
            recent_outcomes, won=True
        )
        assert streak_length == 4
        assert multiplier == pytest.approx(1.40)

    def test_five_game_streak_returns_multiplier_1_60(self, rating_system):
        """A 5-game streak returns 1.60x multiplier."""
        # 4 previous wins + current win = 5-game streak
        recent_outcomes = [True, True, True, True]
        streak_length, multiplier = rating_system.calculate_streak_multiplier(
            recent_outcomes, won=True
        )
        assert streak_length == 5
        assert multiplier == pytest.approx(1.60)

    def test_ten_game_streak_returns_multiplier_2_60(self, rating_system):
        """A 10-game streak returns 2.60x multiplier (uncapped)."""
        # 9 previous wins + current win = 10-game streak
        recent_outcomes = [True] * 9
        streak_length, multiplier = rating_system.calculate_streak_multiplier(
            recent_outcomes, won=True
        )
        assert streak_length == 10
        assert multiplier == pytest.approx(2.60)

    def test_loss_streak_works_same_as_win_streak(self, rating_system):
        """Loss streaks also get multiplied when continuing."""
        # 3 previous losses + current loss = 4-game loss streak
        recent_outcomes = [False, False, False]
        streak_length, multiplier = rating_system.calculate_streak_multiplier(
            recent_outcomes, won=False
        )
        assert streak_length == 4
        assert multiplier == pytest.approx(1.40)

    def test_streak_broken_returns_multiplier_1(self, rating_system):
        """A loss that breaks a win streak gets no boost."""
        recent_outcomes = [True, True, True, True]  # Was on 4-game win streak
        # But this game is a LOSS - breaks the streak
        streak_length, multiplier = rating_system.calculate_streak_multiplier(
            recent_outcomes, won=False
        )
        # Streak resets to 1 (this single loss)
        assert streak_length == 1
        assert multiplier == 1.0

    def test_win_breaks_loss_streak_returns_multiplier_1(self, rating_system):
        """A win that breaks a loss streak gets no boost."""
        recent_outcomes = [False, False, False]  # Was on 3-game loss streak
        # But this game is a WIN - breaks the streak
        streak_length, multiplier = rating_system.calculate_streak_multiplier(
            recent_outcomes, won=True
        )
        assert streak_length == 1
        assert multiplier == 1.0

    def test_mixed_history_finds_current_streak(self, rating_system):
        """Correctly identifies streak from mixed history."""
        # Recent first: L, W, W, W, L, L (reading left to right = most recent first)
        # 1 previous loss + current loss = 2-game loss streak
        recent_outcomes = [False, True, True, True, False, False]
        streak_length, multiplier = rating_system.calculate_streak_multiplier(
            recent_outcomes, won=False  # Another loss continues the streak
        )
        # 1 previous loss + current game = 2-game loss streak
        assert streak_length == 2
        assert multiplier == 1.0

    def test_continuing_streak_from_history(self, rating_system):
        """Correctly continues an existing streak."""
        # Recent first: W, W, W, L, L (reading left to right)
        # 3 previous wins + current win = 4-win streak
        recent_outcomes = [True, True, True, False, False]
        streak_length, multiplier = rating_system.calculate_streak_multiplier(
            recent_outcomes, won=True  # Win continues the streak to 4
        )
        assert streak_length == 4
        assert multiplier == pytest.approx(1.40)

    def test_empty_history_returns_streak_of_1(self, rating_system):
        """First game ever returns streak of 1."""
        recent_outcomes = []
        streak_length, multiplier = rating_system.calculate_streak_multiplier(
            recent_outcomes, won=True
        )
        assert streak_length == 1
        assert multiplier == 1.0

    def test_config_constants_have_expected_values(self):
        """Verify config constants are set correctly."""
        assert STREAK_THRESHOLD == 3
        assert STREAK_MULTIPLIER_PER_GAME == pytest.approx(0.20)


class TestStreakInRatingUpdate:
    """Tests for streak multiplier integration in rating updates."""

    @pytest.fixture
    def rating_system(self):
        return CamaRatingSystem()

    def test_update_player_rating_applies_streak_multiplier(self, rating_system):
        """_update_player_rating applies streak multiplier to delta."""
        from glicko2 import Player

        player = Player(rating=1500, rd=100, vol=0.06)
        team_rating = 1500
        opponent_rating = 1500
        opponent_rd = 100
        result = 1.0  # Win

        # Without streak multiplier
        new_rating_base, _, _ = rating_system._update_player_rating(
            player, team_rating, opponent_rating, opponent_rd, result
        )
        base_delta = new_rating_base - player.rating

        # With 1.30x streak multiplier
        new_rating_streak, _, _ = rating_system._update_player_rating(
            player, team_rating, opponent_rating, opponent_rd, result,
            streak_multiplier=1.30
        )
        streak_delta = new_rating_streak - player.rating

        # Streak delta should be approximately 1.30x the base delta
        assert streak_delta == pytest.approx(base_delta * 1.30, rel=0.01)

    def test_streak_multiplier_1_has_no_effect(self, rating_system):
        """streak_multiplier=1.0 should have no effect on delta."""
        from glicko2 import Player

        player = Player(rating=1500, rd=100, vol=0.06)

        new_rating_default, _, _ = rating_system._update_player_rating(
            player, 1500, 1500, 100, 1.0
        )

        new_rating_explicit, _, _ = rating_system._update_player_rating(
            player, 1500, 1500, 100, 1.0, streak_multiplier=1.0
        )

        assert new_rating_default == pytest.approx(new_rating_explicit)


class TestMatchRepositoryRecentOutcomes:
    """Tests for fetching recent match outcomes from database."""

    def test_get_player_recent_outcomes_returns_booleans(
        self, player_repository, match_repository
    ):
        """get_player_recent_outcomes returns list of booleans (True=win)."""
        discord_id = 12345
        player_repository.add(
            discord_id=discord_id,
            discord_username="TestPlayer",
            glicko_rating=1500,
            glicko_rd=100,
            glicko_volatility=0.06,
        )

        # Record 3 matches with alternating results
        for i, won in enumerate([True, False, True]):
            match_id = match_repository.record_match(
                team1_ids=[discord_id],
                team2_ids=[99999 + i],
                winning_team=1 if won else 2,
            )
            match_repository.add_rating_history(
                discord_id=discord_id,
                rating=1500 + i * 10,
                match_id=match_id,
                won=won,
            )

        outcomes = match_repository.get_player_recent_outcomes(discord_id, limit=10)

        assert isinstance(outcomes, list)
        assert all(isinstance(o, bool) for o in outcomes)
        # Most recent first: True, False, True (reverse chronological)
        assert outcomes == [True, False, True]

    def test_get_player_recent_outcomes_respects_limit(
        self, player_repository, match_repository
    ):
        """get_player_recent_outcomes respects the limit parameter."""
        discord_id = 12345
        player_repository.add(
            discord_id=discord_id,
            discord_username="TestPlayer",
            glicko_rating=1500,
            glicko_rd=100,
            glicko_volatility=0.06,
        )

        # Record 10 matches
        for i in range(10):
            match_id = match_repository.record_match(
                team1_ids=[discord_id],
                team2_ids=[99999 + i],
                winning_team=1,
            )
            match_repository.add_rating_history(
                discord_id=discord_id,
                rating=1500 + i * 10,
                match_id=match_id,
                won=True,
            )

        outcomes = match_repository.get_player_recent_outcomes(discord_id, limit=5)
        assert len(outcomes) == 5

    def test_get_player_recent_outcomes_empty_for_new_player(
        self, player_repository, match_repository
    ):
        """get_player_recent_outcomes returns empty list for player with no matches."""
        discord_id = 12345
        player_repository.add(
            discord_id=discord_id,
            discord_username="TestPlayer",
        )

        outcomes = match_repository.get_player_recent_outcomes(discord_id, limit=10)
        assert outcomes == []

    def test_get_player_recent_outcomes_returns_most_recent_first(
        self, player_repository, match_repository
    ):
        """get_player_recent_outcomes returns outcomes in reverse chronological order."""
        discord_id = 12345
        player_repository.add(
            discord_id=discord_id,
            discord_username="TestPlayer",
            glicko_rating=1500,
            glicko_rd=100,
            glicko_volatility=0.06,
        )

        # Record 5 matches: W, W, L, L, W (chronologically)
        results = [True, True, False, False, True]
        for i, won in enumerate(results):
            match_id = match_repository.record_match(
                team1_ids=[discord_id],
                team2_ids=[99999 + i],
                winning_team=1 if won else 2,
            )
            match_repository.add_rating_history(
                discord_id=discord_id,
                rating=1500 + i * 10,
                match_id=match_id,
                won=won,
            )

        outcomes = match_repository.get_player_recent_outcomes(discord_id, limit=10)
        # Should be reversed: most recent first
        assert outcomes == list(reversed(results))


class TestStreakIntegration:
    """Integration tests for streak multiplier in full rating update flow."""

    @pytest.fixture
    def rating_system(self):
        return CamaRatingSystem()

    def test_win_streak_amplifies_rating_gain(self, rating_system):
        """Verify a 5-game win streak results in ~1.60x rating delta."""
        from glicko2 import Player

        # Create balanced teams
        team1_player = Player(rating=1500, rd=100, vol=0.06)
        team2_player = Player(rating=1500, rd=100, vol=0.06)

        team1_players = [(team1_player, 1)]
        team2_players = [(team2_player, 2)]

        # Base case: no streak (multiplier = 1.0)
        team1_no_streak, _ = rating_system.update_ratings_after_match(
            team1_players, team2_players, winning_team=1,
            streak_multipliers={}
        )
        base_delta = team1_no_streak[0][0] - team1_player.rating

        # With 5-game streak (multiplier = 1.60 at 20% per game)
        # Recreate fresh players since glicko2 mutates them
        team1_player = Player(rating=1500, rd=100, vol=0.06)
        team2_player = Player(rating=1500, rd=100, vol=0.06)
        team1_players = [(team1_player, 1)]
        team2_players = [(team2_player, 2)]

        team1_with_streak, _ = rating_system.update_ratings_after_match(
            team1_players, team2_players, winning_team=1,
            streak_multipliers={1: 1.60}
        )
        streak_delta = team1_with_streak[0][0] - 1500

        # Streak delta should be ~1.60x the base delta
        assert streak_delta == pytest.approx(base_delta * 1.60, rel=0.01)

    def test_loss_streak_amplifies_rating_loss(self, rating_system):
        """Verify a 4-game loss streak results in ~1.40x rating loss."""
        from glicko2 import Player

        team1_player = Player(rating=1500, rd=100, vol=0.06)
        team2_player = Player(rating=1500, rd=100, vol=0.06)

        # Base case: loss without streak
        team1_players = [(team1_player, 1)]
        team2_players = [(team2_player, 2)]

        team1_no_streak, _ = rating_system.update_ratings_after_match(
            team1_players, team2_players, winning_team=2,  # Team 1 loses
            streak_multipliers={}
        )
        base_delta = team1_no_streak[0][0] - team1_player.rating  # Negative

        # With 4-game loss streak (multiplier = 1.40 at 20% per game)
        team1_player = Player(rating=1500, rd=100, vol=0.06)
        team2_player = Player(rating=1500, rd=100, vol=0.06)
        team1_players = [(team1_player, 1)]
        team2_players = [(team2_player, 2)]

        team1_with_streak, _ = rating_system.update_ratings_after_match(
            team1_players, team2_players, winning_team=2,  # Team 1 loses
            streak_multipliers={1: 1.40}
        )
        streak_delta = team1_with_streak[0][0] - 1500

        # Streak delta should be ~1.40x the base delta (both negative)
        assert streak_delta == pytest.approx(base_delta * 1.40, rel=0.01)


class TestRatingHistoryStreakColumns:
    """Tests for streak data storage in rating_history table."""

    def test_add_rating_history_with_streak_data(
        self, player_repository, match_repository
    ):
        """add_rating_history can store streak_length and streak_multiplier."""
        discord_id = 12345
        player_repository.add(
            discord_id=discord_id,
            discord_username="TestPlayer",
            glicko_rating=1500,
            glicko_rd=100,
            glicko_volatility=0.06,
        )

        match_id = match_repository.record_match(
            team1_ids=[discord_id],
            team2_ids=[99999],
            winning_team=1,
        )

        match_repository.add_rating_history(
            discord_id=discord_id,
            rating=1520,
            match_id=match_id,
            rating_before=1500,
            won=True,
            streak_length=5,
            streak_multiplier=1.30,
        )

        history = match_repository.get_rating_history(discord_id, limit=1)
        assert len(history) == 1
        assert history[0]["streak_length"] == 5
        assert history[0]["streak_multiplier"] == pytest.approx(1.30)

    def test_streak_columns_default_to_none(
        self, player_repository, match_repository
    ):
        """Streak columns default to NULL when not provided."""
        discord_id = 12345
        player_repository.add(
            discord_id=discord_id,
            discord_username="TestPlayer",
            glicko_rating=1500,
            glicko_rd=100,
            glicko_volatility=0.06,
        )

        match_id = match_repository.record_match(
            team1_ids=[discord_id],
            team2_ids=[99999],
            winning_team=1,
        )

        match_repository.add_rating_history(
            discord_id=discord_id,
            rating=1520,
            match_id=match_id,
            won=True,
        )

        history = match_repository.get_rating_history(discord_id, limit=1)
        assert len(history) == 1
        assert history[0].get("streak_length") is None
        assert history[0].get("streak_multiplier") is None
