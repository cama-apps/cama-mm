"""
Tests for streak-based rating adjustments.

Streak-based adjustments apply a delta multiplier to Glicko-2 rating updates
when players are on win/loss streaks of 3+ games. This helps correct ratings
faster when a player's skill has changed.

Multiplier formula: 1.0 + 0.25 * max(0, streak_length - 2)
- Streaks 1-2: 1.00x (normal)
- Streak 3: 1.25x (+25%)
- Streak 4: 1.50x (+50%)
- Streak 5: 1.75x (+75%)
- etc. (uncapped)
"""

import pytest

from config import STREAK_MULTIPLIER_PER_GAME, STREAK_THRESHOLD
from rating_system import (
    CamaRatingSystem,
    recorded_streak_rate,
    recorded_streak_threshold,
)
from tests.conftest import TEST_GUILD_ID


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

    def test_three_game_streak_returns_multiplier_1_25(self, rating_system):
        """A 3-game streak returns 1.25x multiplier."""
        # 2 previous wins + current win = 3-game streak
        recent_outcomes = [True, True]
        streak_length, multiplier = rating_system.calculate_streak_multiplier(
            recent_outcomes, won=True
        )
        assert streak_length == 3
        assert multiplier == pytest.approx(1.25)

    def test_four_game_streak_returns_multiplier_1_50(self, rating_system):
        """A 4-game streak returns 1.50x multiplier."""
        # 3 previous wins + current win = 4-game streak
        recent_outcomes = [True, True, True]
        streak_length, multiplier = rating_system.calculate_streak_multiplier(
            recent_outcomes, won=True
        )
        assert streak_length == 4
        assert multiplier == pytest.approx(1.50)

    def test_five_game_streak_returns_multiplier_1_75(self, rating_system):
        """A 5-game streak returns 1.75x multiplier."""
        # 4 previous wins + current win = 5-game streak
        recent_outcomes = [True, True, True, True]
        streak_length, multiplier = rating_system.calculate_streak_multiplier(
            recent_outcomes, won=True
        )
        assert streak_length == 5
        assert multiplier == pytest.approx(1.75)

    def test_ten_game_streak_returns_multiplier_3_00(self, rating_system):
        """A 10-game streak returns 3.00x multiplier (uncapped)."""
        # 9 previous wins + current win = 10-game streak
        recent_outcomes = [True] * 9
        streak_length, multiplier = rating_system.calculate_streak_multiplier(
            recent_outcomes, won=True
        )
        assert streak_length == 10
        assert multiplier == pytest.approx(3.00)

    def test_loss_streak_works_same_as_win_streak(self, rating_system):
        """Loss streaks also get multiplied when continuing."""
        # 3 previous losses + current loss = 4-game loss streak
        recent_outcomes = [False, False, False]
        streak_length, multiplier = rating_system.calculate_streak_multiplier(
            recent_outcomes, won=False
        )
        assert streak_length == 4
        assert multiplier == pytest.approx(1.50)

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
        assert multiplier == pytest.approx(1.50)

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
        assert pytest.approx(0.25) == STREAK_MULTIPLIER_PER_GAME

    def test_threshold_override_suppresses_boost_below_recorded_gate(self, rating_system):
        """A recorded threshold above the streak length yields no multiplier."""
        recent_outcomes = [True, True]  # 3-game streak with the current win
        streak_length, multiplier = rating_system.calculate_streak_multiplier(
            recent_outcomes, won=True, streak_threshold=4
        )
        assert streak_length == 3
        assert multiplier == 1.0

    def test_threshold_override_matching_default_keeps_boost(self, rating_system):
        """An explicit threshold of 3 reproduces the default gate."""
        recent_outcomes = [True, True]
        streak_length, multiplier = rating_system.calculate_streak_multiplier(
            recent_outcomes, won=True, streak_threshold=3
        )
        assert streak_length == 3
        assert multiplier == pytest.approx(1.25)

    def test_recorded_streak_helpers_fall_back_to_legacy_values(self):
        """Missing or malformed stored values parse to the legacy curve."""
        assert recorded_streak_rate(None) == pytest.approx(0.20)
        assert recorded_streak_rate("not-a-number") == pytest.approx(0.20)
        assert recorded_streak_rate(0.30) == pytest.approx(0.30)
        assert recorded_streak_threshold(None) == 3
        assert recorded_streak_threshold("not-a-number") == 3
        assert recorded_streak_threshold(4) == 4


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
            guild_id=TEST_GUILD_ID,
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
                guild_id=TEST_GUILD_ID,
            )
            match_repository.add_rating_history(
                discord_id=discord_id,
                guild_id=TEST_GUILD_ID,
                rating=1500 + i * 10,
                match_id=match_id,
                won=won,
            )

        outcomes = match_repository.get_player_recent_outcomes(discord_id, guild_id=TEST_GUILD_ID, limit=10)

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
            guild_id=TEST_GUILD_ID,
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
                guild_id=TEST_GUILD_ID,
            )
            match_repository.add_rating_history(
                discord_id=discord_id,
                guild_id=TEST_GUILD_ID,
                rating=1500 + i * 10,
                match_id=match_id,
                won=True,
            )

        outcomes = match_repository.get_player_recent_outcomes(discord_id, guild_id=TEST_GUILD_ID, limit=5)
        assert len(outcomes) == 5

    def test_get_player_recent_outcomes_empty_for_new_player(
        self, player_repository, match_repository
    ):
        """get_player_recent_outcomes returns empty list for player with no matches."""
        discord_id = 12345
        player_repository.add(
            discord_id=discord_id,
            discord_username="TestPlayer",
            guild_id=TEST_GUILD_ID,
        )

        outcomes = match_repository.get_player_recent_outcomes(discord_id, guild_id=TEST_GUILD_ID, limit=10)
        assert outcomes == []

    def test_get_player_recent_outcomes_returns_most_recent_first(
        self, player_repository, match_repository
    ):
        """get_player_recent_outcomes returns outcomes in reverse chronological order."""
        discord_id = 12345
        player_repository.add(
            discord_id=discord_id,
            discord_username="TestPlayer",
            guild_id=TEST_GUILD_ID,
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
                guild_id=TEST_GUILD_ID,
            )
            match_repository.add_rating_history(
                discord_id=discord_id,
                guild_id=TEST_GUILD_ID,
                rating=1500 + i * 10,
                match_id=match_id,
                won=won,
            )

        outcomes = match_repository.get_player_recent_outcomes(discord_id, guild_id=TEST_GUILD_ID, limit=10)
        # Should be reversed: most recent first
        assert outcomes == list(reversed(results))


class TestStreakIntegration:
    """Integration tests for streak multiplier in full rating update flow."""

    @pytest.fixture
    def rating_system(self):
        return CamaRatingSystem()

    def test_win_streak_amplifies_rating_gain(self, rating_system):
        """Verify a 5-game win streak results in ~1.75x rating delta."""
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

        # With 5-game streak (multiplier = 1.75 at 25% per game)
        # Recreate fresh players since glicko2 mutates them
        team1_player = Player(rating=1500, rd=100, vol=0.06)
        team2_player = Player(rating=1500, rd=100, vol=0.06)
        team1_players = [(team1_player, 1)]
        team2_players = [(team2_player, 2)]

        team1_with_streak, _ = rating_system.update_ratings_after_match(
            team1_players, team2_players, winning_team=1,
            streak_multipliers={1: 1.75}
        )
        streak_delta = team1_with_streak[0][0] - 1500

        # Streak delta should be ~1.75x the base delta
        assert streak_delta == pytest.approx(base_delta * 1.75, rel=0.01)

    def test_loss_streak_amplifies_rating_loss(self, rating_system):
        """Verify a 4-game loss streak results in ~1.50x rating loss."""
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

        # With 4-game loss streak (multiplier = 1.50 at 25% per game)
        team1_player = Player(rating=1500, rd=100, vol=0.06)
        team2_player = Player(rating=1500, rd=100, vol=0.06)
        team1_players = [(team1_player, 1)]
        team2_players = [(team2_player, 2)]

        team1_with_streak, _ = rating_system.update_ratings_after_match(
            team1_players, team2_players, winning_team=2,  # Team 1 loses
            streak_multipliers={1: 1.50}
        )
        streak_delta = team1_with_streak[0][0] - 1500

        # Streak delta should be ~1.50x the base delta (both negative)
        assert streak_delta == pytest.approx(base_delta * 1.50, rel=0.01)


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
            guild_id=TEST_GUILD_ID,
            glicko_rating=1500,
            glicko_rd=100,
            glicko_volatility=0.06,
        )

        match_id = match_repository.record_match(
            team1_ids=[discord_id],
            team2_ids=[99999],
            winning_team=1,
            guild_id=TEST_GUILD_ID,
        )

        match_repository.add_rating_history(
            discord_id=discord_id,
            guild_id=TEST_GUILD_ID,
            rating=1520,
            match_id=match_id,
            rating_before=1500,
            won=True,
            streak_length=5,
            streak_multiplier=1.30,
        )

        history = match_repository.get_rating_history(discord_id, guild_id=TEST_GUILD_ID, limit=1)
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
            guild_id=TEST_GUILD_ID,
            glicko_rating=1500,
            glicko_rd=100,
            glicko_volatility=0.06,
        )

        match_id = match_repository.record_match(
            team1_ids=[discord_id],
            team2_ids=[99999],
            winning_team=1,
            guild_id=TEST_GUILD_ID,
        )

        match_repository.add_rating_history(
            discord_id=discord_id,
            guild_id=TEST_GUILD_ID,
            rating=1520,
            match_id=match_id,
            won=True,
        )

        history = match_repository.get_rating_history(discord_id, guild_id=TEST_GUILD_ID, limit=1)
        assert len(history) == 1
        assert history[0].get("streak_length") is None
        assert history[0].get("streak_multiplier") is None


class TestChronologicalOutcomeWindows:
    """Outcome windows must follow match chronology, not rating_history.id.

    The OpenSkill replay backfills missing rating_history rows for legacy
    matches, giving them brand-new autoincrement ids. If the streak windows
    ordered by id, those years-old outcomes would surface at the head of the
    recency window and corrupt the next streak computation.
    """

    def _setup_history(self, player_repository, match_repository):
        discord_id = 31337
        player_repository.add(
            discord_id=discord_id,
            discord_username="BackfillPlayer",
            guild_id=TEST_GUILD_ID,
            glicko_rating=1500,
            glicko_rd=100,
            glicko_volatility=0.06,
        )

        # Three chronological matches: m1 (oldest, won) then m2/m3 (lost).
        match_ids = []
        for i, won in enumerate([True, False, False]):
            match_ids.append(
                match_repository.record_match(
                    team1_ids=[discord_id],
                    team2_ids=[99999 + i],
                    winning_team=1 if won else 2,
                    guild_id=TEST_GUILD_ID,
                )
            )
        m1, m2, m3 = match_ids

        # History rows for m2 and m3 exist first; m1's row is backfilled by a
        # replay LAST, so it receives the highest rating_history.id.
        for match_id, won in ((m2, False), (m3, False), (m1, True)):
            match_repository.add_rating_history(
                discord_id=discord_id,
                guild_id=TEST_GUILD_ID,
                rating=1500,
                match_id=match_id,
                won=won,
            )
        return discord_id, (m1, m2, m3)

    def test_recent_outcomes_follow_match_chronology(
        self, player_repository, match_repository
    ):
        discord_id, _match_ids = self._setup_history(player_repository, match_repository)

        outcomes = match_repository.get_player_recent_outcomes(
            discord_id, guild_id=TEST_GUILD_ID, limit=10
        )
        # Most recent first: m3 (loss), m2 (loss), m1 (win) — even though m1's
        # row was inserted last.
        assert outcomes == [False, False, True]

    def test_recent_outcomes_bulk_follow_match_chronology(
        self, player_repository, match_repository
    ):
        discord_id, _match_ids = self._setup_history(player_repository, match_repository)

        outcomes = match_repository.get_player_recent_outcomes_bulk(
            [discord_id], TEST_GUILD_ID, limit=10
        )
        assert outcomes[discord_id] == [False, False, True]

    def test_outcomes_before_match_follow_match_chronology(
        self, player_repository, match_repository
    ):
        discord_id, (m1, _m2, m3) = self._setup_history(player_repository, match_repository)

        outcomes = match_repository.get_player_outcomes_before_match(
            discord_id, TEST_GUILD_ID, m3, limit=10
        )
        # Before m3: m2 (loss) then m1 (win). The id-ordered cutoff would have
        # dropped m1 entirely (its row id is larger than m3's).
        assert outcomes == [False, True]

        # Nothing before the oldest match.
        assert (
            match_repository.get_player_outcomes_before_match(
                discord_id, TEST_GUILD_ID, m1, limit=10
            )
            == []
        )

    def test_outcomes_before_match_bulk_follow_match_chronology(
        self, player_repository, match_repository
    ):
        discord_id, (_m1, _m2, m3) = self._setup_history(player_repository, match_repository)

        outcomes = match_repository.get_player_outcomes_before_match_bulk(
            [discord_id], TEST_GUILD_ID, m3, limit=10
        )
        assert outcomes[discord_id] == [False, True]

    def test_outcomes_before_match_empty_without_history_row(
        self, player_repository, match_repository
    ):
        """A player with no rating_history row for the match gets no window."""
        discord_id, _match_ids = self._setup_history(player_repository, match_repository)

        extra_match = match_repository.record_match(
            team1_ids=[discord_id],
            team2_ids=[88888],
            winning_team=1,
            guild_id=TEST_GUILD_ID,
        )
        # No rating_history row was written for extra_match.
        assert (
            match_repository.get_player_outcomes_before_match(
                discord_id, TEST_GUILD_ID, extra_match, limit=10
            )
            == []
        )
        bulk = match_repository.get_player_outcomes_before_match_bulk(
            [discord_id], TEST_GUILD_ID, extra_match, limit=10
        )
        assert bulk[discord_id] == []
