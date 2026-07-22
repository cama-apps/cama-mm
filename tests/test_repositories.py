"""
Tests for the repository layer.
"""

import sqlite3

from config import NEW_PLAYER_EXCLUSION_BOOST
from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY


def _expected_after_exclusions(exclusions: int) -> int:
    return NEW_PLAYER_EXCLUSION_BOOST + exclusions * 6


class TestPlayerRepository:
    """Tests for PlayerRepository."""

    def test_add_and_get_player(self, player_repository):
        """Test adding and retrieving a player."""
        player_repository.add(
            discord_id=12345,
            discord_username="TestPlayer",
            guild_id=TEST_GUILD_ID,
            initial_mmr=3000,
            preferred_roles=["1", "2"],
            glicko_rating=1500,
            glicko_rd=350,
            glicko_volatility=0.06,
        )

        player = player_repository.get_by_id(12345, TEST_GUILD_ID)
        assert player is not None
        assert player.name == "TestPlayer"
        assert player.mmr == 3000
        assert player.preferred_roles == ["1", "2"]

    def test_player_not_found(self, player_repository):
        """Test getting a non-existent player."""
        player = player_repository.get_by_id(99999, TEST_GUILD_ID)
        assert player is None

    def test_exists(self, player_repository):
        """Test player existence check."""
        player_repository.add(
            discord_id=12345,
            discord_username="TestPlayer",
            guild_id=TEST_GUILD_ID,
        )

        assert player_repository.exists(12345, TEST_GUILD_ID) is True
        assert player_repository.exists(99999, TEST_GUILD_ID) is False

    def test_update_roles(self, player_repository):
        """Test updating player roles."""
        player_repository.add(
            discord_id=12345,
            discord_username="TestPlayer",
            guild_id=TEST_GUILD_ID,
        )

        player_repository.update_roles(12345, TEST_GUILD_ID, ["3", "4", "5"])

        player = player_repository.get_by_id(12345, TEST_GUILD_ID)
        assert player.preferred_roles == ["3", "4", "5"]

    def test_update_glicko_rating(self, player_repository):
        """Test updating Glicko rating."""
        player_repository.add(
            discord_id=12345,
            discord_username="TestPlayer",
            guild_id=TEST_GUILD_ID,
            glicko_rating=1500,
            glicko_rd=350,
            glicko_volatility=0.06,
        )

        player_repository.update_glicko_rating(12345, TEST_GUILD_ID, 1600, 300, 0.05)

        rating = player_repository.get_glicko_rating(12345, TEST_GUILD_ID)
        assert rating[0] == 1600
        assert rating[1] == 300
        assert rating[2] == 0.05

    def test_get_match_rating_inputs_bulk(self, player_repository):
        player_repository.add(
            discord_id=12345,
            discord_username="MatchPlayer",
            guild_id=TEST_GUILD_ID,
            initial_mmr=3200,
            glicko_rating=1550,
            glicko_rd=120,
            glicko_volatility=0.05,
            os_mu=27.5,
            os_sigma=7.25,
        )
        player_repository.add(
            discord_id=12345,
            discord_username="OtherGuildPlayer",
            guild_id=TEST_GUILD_ID + 1,
            initial_mmr=9000,
            glicko_rating=2500,
            glicko_rd=50,
            glicko_volatility=0.03,
        )
        with sqlite3.connect(player_repository.db_path) as connection:
            connection.execute(
                """
                UPDATE players
                SET last_match_date = ?, first_calibrated_at = ?
                WHERE discord_id = ? AND guild_id = ?
                """,
                ("2026-07-20T12:00:00+00:00", 1_750_000_000, 12345, TEST_GUILD_ID),
            )

        inputs = player_repository.get_match_rating_inputs(
            [12345, 99999, 12345], TEST_GUILD_ID
        )

        assert list(inputs) == [12345]
        assert inputs[12345]["current_mmr"] == 3200
        assert inputs[12345]["glicko_rating"] == 1550
        assert inputs[12345]["glicko_rd"] == 120
        assert inputs[12345]["glicko_volatility"] == 0.05
        assert inputs[12345]["os_mu"] == 27.5
        assert inputs[12345]["os_sigma"] == 7.25
        assert inputs[12345]["last_match_date"] == "2026-07-20T12:00:00+00:00"
        assert inputs[12345]["first_calibrated_at"] == 1_750_000_000
        assert player_repository.get_match_rating_inputs([], TEST_GUILD_ID) == {}

    def test_get_balances_bulk_is_single_query_and_guild_scoped(
        self, player_repository, monkeypatch
    ):
        player_ids = [12346, 12347]
        for pid, balance in zip(player_ids, [75, -25], strict=True):
            player_repository.add(
                discord_id=pid,
                discord_username=f"BalancePlayer{pid}",
                guild_id=TEST_GUILD_ID,
            )
            player_repository.update_balance(pid, TEST_GUILD_ID, balance)
        player_repository.add(
            discord_id=player_ids[0],
            discord_username="OtherGuildBalancePlayer",
            guild_id=TEST_GUILD_ID_SECONDARY,
        )
        player_repository.update_balance(
            player_ids[0], TEST_GUILD_ID_SECONDARY, 999
        )

        connection_count = 0
        original_get_connection = player_repository.get_connection

        def counted_get_connection():
            nonlocal connection_count
            connection_count += 1
            return original_get_connection()

        monkeypatch.setattr(
            player_repository, "get_connection", counted_get_connection
        )

        balances = player_repository.get_balances_bulk(
            [player_ids[1], player_ids[0], 99999, player_ids[1]],
            TEST_GUILD_ID,
        )

        assert balances == {
            player_ids[1]: -25,
            player_ids[0]: 75,
            99999: 0,
        }
        assert connection_count == 1
        assert player_repository.get_balances_bulk([], TEST_GUILD_ID) == {}
        assert connection_count == 1

    def test_update_personal_best_win_streaks_is_atomic_and_guild_scoped(
        self, player_repository, monkeypatch
    ):
        player_ids = [12351, 12352, 12353]
        for pid in player_ids:
            player_repository.add(
                discord_id=pid,
                discord_username=f"StreakPlayer{pid}",
                guild_id=TEST_GUILD_ID,
            )
        player_repository.add(
            discord_id=player_ids[0],
            discord_username="OtherGuildStreakPlayer",
            guild_id=TEST_GUILD_ID_SECONDARY,
        )
        player_repository.update_personal_best_win_streak(
            player_ids[0], TEST_GUILD_ID, 4
        )
        player_repository.update_personal_best_win_streak(
            player_ids[1], TEST_GUILD_ID, 8
        )
        player_repository.update_personal_best_win_streak(
            player_ids[0], TEST_GUILD_ID_SECONDARY, 10
        )

        connection_count = 0
        original_get_connection = player_repository.get_connection

        def counted_get_connection():
            nonlocal connection_count
            connection_count += 1
            return original_get_connection()

        monkeypatch.setattr(
            player_repository, "get_connection", counted_get_connection
        )

        previous = player_repository.update_personal_best_win_streaks(
            {
                player_ids[0]: 6,
                player_ids[1]: 7,
                player_ids[2]: 5,
                99999: 12,
            },
            TEST_GUILD_ID,
        )

        assert previous == {player_ids[0]: 4, player_ids[2]: 0}
        assert connection_count == 1
        assert (
            player_repository.get_personal_best_win_streak(
                player_ids[0], TEST_GUILD_ID
            )
            == 6
        )
        assert (
            player_repository.get_personal_best_win_streak(
                player_ids[1], TEST_GUILD_ID
            )
            == 8
        )
        assert (
            player_repository.get_personal_best_win_streak(
                player_ids[2], TEST_GUILD_ID
            )
            == 5
        )
        assert (
            player_repository.get_personal_best_win_streak(
                player_ids[0], TEST_GUILD_ID_SECONDARY
            )
            == 10
        )

    def test_get_by_ids_preserves_order(self, player_repository):
        """Test that get_by_ids preserves input order."""
        for i in range(5):
            player_repository.add(
                discord_id=1000 + i,
                discord_username=f"Player{i}",
                guild_id=TEST_GUILD_ID,
            )

        ids = [1003, 1001, 1004, 1000, 1002]
        players = player_repository.get_by_ids(ids, TEST_GUILD_ID)

        assert len(players) == 5
        assert [p.name for p in players] == ["Player3", "Player1", "Player4", "Player0", "Player2"]

    def test_get_by_username_partial_case_insensitive(self, player_repository):
        """Test username lookup supports partial and case-insensitive matching."""
        player_repository.add(discord_id=2001, discord_username="AlphaUser", guild_id=TEST_GUILD_ID)
        player_repository.add(discord_id=2002, discord_username="betaUser", guild_id=TEST_GUILD_ID)
        player_repository.add(discord_id=2003, discord_username="GammaTester", guild_id=TEST_GUILD_ID)

        matches = player_repository.get_by_username("alpha", TEST_GUILD_ID)
        assert len(matches) == 1
        assert matches[0]["discord_id"] == 2001

        matches = player_repository.get_by_username("USER", TEST_GUILD_ID)
        assert {m["discord_id"] for m in matches} == {2001, 2002}

        matches = player_repository.get_by_username("gamma", TEST_GUILD_ID)
        assert len(matches) == 1
        assert matches[0]["discord_username"] == "GammaTester"

    def test_exclusion_counts(self, player_repository):
        """Test exclusion count operations."""
        player_repository.add(
            discord_id=12345,
            discord_username="TestPlayer",
            guild_id=TEST_GUILD_ID,
        )

        # Initial count should match the configured boost
        counts = player_repository.get_exclusion_counts([12345], TEST_GUILD_ID)
        assert counts[12345] == NEW_PLAYER_EXCLUSION_BOOST

        # Increment twice (6 per exclusion)
        player_repository.increment_exclusion_count(12345, TEST_GUILD_ID)
        player_repository.increment_exclusion_count(12345, TEST_GUILD_ID)
        counts = player_repository.get_exclusion_counts([12345], TEST_GUILD_ID)
        expected = _expected_after_exclusions(2)
        assert counts[12345] == expected

        # Decay (halves the count)
        player_repository.decay_exclusion_count(12345, TEST_GUILD_ID)
        counts = player_repository.get_exclusion_counts([12345], TEST_GUILD_ID)
        expected //= 2
        assert counts[12345] == expected

    def test_delete_player(self, player_repository):
        """Test deleting a player."""
        player_repository.add(
            discord_id=12345,
            discord_username="TestPlayer",
            guild_id=TEST_GUILD_ID,
        )

        assert player_repository.delete(12345, TEST_GUILD_ID) is True
        assert player_repository.get_by_id(12345, TEST_GUILD_ID) is None
        assert player_repository.delete(12345, TEST_GUILD_ID) is False  # Already deleted

    def test_get_player_above_returns_higher_balance(self, player_repository):
        """Test get_player_above returns the player ranked one position higher."""
        # Add players with different balances
        player_repository.add(
            discord_id=1001,
            discord_username="Poor",
            guild_id=TEST_GUILD_ID,
        )
        player_repository.add(
            discord_id=1002,
            discord_username="Middle",
            guild_id=TEST_GUILD_ID,
        )
        player_repository.add(
            discord_id=1003,
            discord_username="Rich",
            guild_id=TEST_GUILD_ID,
        )

        # Set balances: Poor=10, Middle=50, Rich=100
        player_repository.update_balance(1001, TEST_GUILD_ID, 10)
        player_repository.update_balance(1002, TEST_GUILD_ID, 50)
        player_repository.update_balance(1003, TEST_GUILD_ID, 100)

        # Poor's player above should be Middle
        above = player_repository.get_player_above(1001, TEST_GUILD_ID)
        assert above is not None
        assert above.discord_id == 1002

        # Middle's player above should be Rich
        above = player_repository.get_player_above(1002, TEST_GUILD_ID)
        assert above is not None
        assert above.discord_id == 1003

        # Rich has no player above (they're #1)
        above = player_repository.get_player_above(1003, TEST_GUILD_ID)
        assert above is None

    def test_get_player_above_skips_players_below_minimum_balance(self, player_repository):
        """The next eligible player can be above an ineligible adjacent player."""
        player_repository.add(
            discord_id=1101,
            discord_username="Spinner",
            guild_id=TEST_GUILD_ID,
        )
        player_repository.add(
            discord_id=1102,
            discord_username="Protected",
            guild_id=TEST_GUILD_ID,
        )
        player_repository.add(
            discord_id=1103,
            discord_username="Eligible",
            guild_id=TEST_GUILD_ID,
        )

        player_repository.update_balance(1101, TEST_GUILD_ID, 2)
        player_repository.update_balance(1102, TEST_GUILD_ID, 10)
        player_repository.update_balance(1103, TEST_GUILD_ID, 100)

        above = player_repository.get_player_above(
            1101,
            TEST_GUILD_ID,
            min_balance=50,
        )

        assert above is not None
        assert above.discord_id == 1103

    def test_get_player_above_handles_ties(self, player_repository):
        """Test get_player_above handles tied balances correctly."""
        # Add players with tied balances
        player_repository.add(
            discord_id=2001,
            discord_username="Tied1",
            guild_id=TEST_GUILD_ID,
        )
        player_repository.add(
            discord_id=2002,
            discord_username="Tied2",
            guild_id=TEST_GUILD_ID,
        )

        # Both have balance 50
        player_repository.update_balance(2001, TEST_GUILD_ID, 50)
        player_repository.update_balance(2002, TEST_GUILD_ID, 50)

        # Player with higher discord_id (2002) should see player with lower discord_id (2001) as above
        above = player_repository.get_player_above(2002, TEST_GUILD_ID)
        assert above is not None
        assert above.discord_id == 2001

        # Player with lower discord_id (2001) has no one above at the same balance
        above = player_repository.get_player_above(2001, TEST_GUILD_ID)
        assert above is None

    def test_get_player_above_nonexistent_player(self, player_repository):
        """Test get_player_above returns None for non-existent player."""
        above = player_repository.get_player_above(99999, TEST_GUILD_ID)
        assert above is None

    def test_get_player_below_returns_lower_balance(self, player_repository):
        """Test get_player_below returns the player ranked one position lower."""
        player_repository.add(discord_id=3001, discord_username="Poor", guild_id=TEST_GUILD_ID)
        player_repository.add(discord_id=3002, discord_username="Middle", guild_id=TEST_GUILD_ID)
        player_repository.add(discord_id=3003, discord_username="Rich", guild_id=TEST_GUILD_ID)

        player_repository.update_balance(3001, TEST_GUILD_ID, 10)
        player_repository.update_balance(3002, TEST_GUILD_ID, 50)
        player_repository.update_balance(3003, TEST_GUILD_ID, 100)

        # Rich's player below should be Middle
        below = player_repository.get_player_below(3003, TEST_GUILD_ID)
        assert below is not None
        assert below.discord_id == 3002

        # Middle's player below should be Poor
        below = player_repository.get_player_below(3002, TEST_GUILD_ID)
        assert below is not None
        assert below.discord_id == 3001

        # Poor has no player below (they're last)
        below = player_repository.get_player_below(3001, TEST_GUILD_ID)
        assert below is None

    def test_get_player_below_handles_ties(self, player_repository):
        """Test get_player_below uses the inverse discord_id tiebreaker as get_player_above."""
        player_repository.add(discord_id=4001, discord_username="Tied1", guild_id=TEST_GUILD_ID)
        player_repository.add(discord_id=4002, discord_username="Tied2", guild_id=TEST_GUILD_ID)

        player_repository.update_balance(4001, TEST_GUILD_ID, 50)
        player_repository.update_balance(4002, TEST_GUILD_ID, 50)

        # Player with lower discord_id (4001) should see player with higher discord_id (4002) as below
        below = player_repository.get_player_below(4001, TEST_GUILD_ID)
        assert below is not None
        assert below.discord_id == 4002

        # Player with higher discord_id (4002) has no one below at the same balance
        below = player_repository.get_player_below(4002, TEST_GUILD_ID)
        assert below is None

    def test_get_player_below_nonexistent_player(self, player_repository):
        """Test get_player_below returns None for non-existent player."""
        below = player_repository.get_player_below(99999, TEST_GUILD_ID)
        assert below is None

    def test_balance_neighbors_match_wins_and_rating_tiebreakers(
        self, player_repository
    ):
        """Wheel neighbors must follow the same tie order as the leaderboard."""
        for discord_id, rating in ((5003, 1600), (5002, 1400), (5001, 1400)):
            player_repository.add(
                discord_id=discord_id,
                discord_username=f"Player {discord_id}",
                guild_id=TEST_GUILD_ID,
                glicko_rating=rating,
            )
            player_repository.update_balance(discord_id, TEST_GUILD_ID, 50)

        player_repository.increment_wins(5001, TEST_GUILD_ID)
        player_repository.increment_wins(5002, TEST_GUILD_ID)

        leaderboard = player_repository.get_leaderboard(TEST_GUILD_ID, limit=3)
        assert [player.discord_id for player in leaderboard] == [5001, 5002, 5003]
        assert player_repository.get_player_above(5002, TEST_GUILD_ID).discord_id == 5001
        assert player_repository.get_player_below(5002, TEST_GUILD_ID).discord_id == 5003
        assert player_repository.get_player_above(5003, TEST_GUILD_ID).discord_id == 5002

    def test_balance_neighbors_use_rating_before_discord_id(self, player_repository):
        """Glicko decides equal-balance/equal-win neighbors before ID does."""
        player_repository.add(
            discord_id=5101,
            discord_username="Lower rating",
            guild_id=TEST_GUILD_ID,
            glicko_rating=1400,
        )
        player_repository.add(
            discord_id=5102,
            discord_username="Higher rating",
            guild_id=TEST_GUILD_ID,
            glicko_rating=1600,
        )
        player_repository.update_balance(5101, TEST_GUILD_ID, 50)
        player_repository.update_balance(5102, TEST_GUILD_ID, 50)

        assert player_repository.get_player_above(5101, TEST_GUILD_ID).discord_id == 5102
        assert player_repository.get_player_below(5102, TEST_GUILD_ID).discord_id == 5101

    def test_player_leaderboards_use_id_for_exact_ties(self, player_repository):
        """Exact metric ties have a stable final Discord-ID ordering."""
        for discord_id in (5202, 5201):
            player_repository.add(
                discord_id=discord_id,
                discord_username=f"Player {discord_id}",
                guild_id=TEST_GUILD_ID,
                glicko_rating=1500,
                os_mu=25,
            )

        for leaderboard in (
            player_repository.get_leaderboard(TEST_GUILD_ID, limit=2),
            player_repository.get_leaderboard_by_glicko(TEST_GUILD_ID, limit=2),
            player_repository.get_leaderboard_by_openskill(TEST_GUILD_ID, limit=2),
        ):
            assert [player.discord_id for player in leaderboard] == [5201, 5202]

    def test_steal_atomic_transfers_coins(self, player_repository):
        """Test steal_atomic atomically transfers coins from victim to thief."""
        # Add thief and victim
        player_repository.add(
            discord_id=3001,
            discord_username="Thief",
            guild_id=TEST_GUILD_ID,
        )
        player_repository.add(
            discord_id=3002,
            discord_username="Victim",
            guild_id=TEST_GUILD_ID,
        )

        # Set balances: Thief=50, Victim=100
        player_repository.update_balance(3001, TEST_GUILD_ID, 50)
        player_repository.update_balance(3002, TEST_GUILD_ID, 100)

        # Steal 10 coins
        result = player_repository.steal_atomic(
            thief_discord_id=3001,
            victim_discord_id=3002,
            guild_id=TEST_GUILD_ID,
            amount=10,
        )

        assert result["amount"] == 10
        assert result["thief_new_balance"] == 60
        assert result["victim_new_balance"] == 90

        # Verify actual balances
        thief = player_repository.get_by_id(3001, TEST_GUILD_ID)
        victim = player_repository.get_by_id(3002, TEST_GUILD_ID)
        assert thief.jopacoin_balance == 60
        assert victim.jopacoin_balance == 90

    def test_steal_atomic_can_push_victim_negative(self, player_repository):
        """Test steal_atomic can push victim below zero (intentional for shell mechanic)."""
        # Add thief and victim
        player_repository.add(
            discord_id=4001,
            discord_username="Thief",
            guild_id=TEST_GUILD_ID,
        )
        player_repository.add(
            discord_id=4002,
            discord_username="Victim",
            guild_id=TEST_GUILD_ID,
        )

        # Set balances: Thief=0, Victim=5
        player_repository.update_balance(4001, TEST_GUILD_ID, 0)
        player_repository.update_balance(4002, TEST_GUILD_ID, 5)

        # Steal 10 coins (more than victim has)
        result = player_repository.steal_atomic(
            thief_discord_id=4001,
            victim_discord_id=4002,
            guild_id=TEST_GUILD_ID,
            amount=10,
        )

        assert result["victim_new_balance"] == -5  # Pushed negative
        assert result["thief_new_balance"] == 10

        # Verify actual balances
        victim = player_repository.get_by_id(4002, TEST_GUILD_ID)
        assert victim.jopacoin_balance == -5

    def test_steal_atomic_tracks_lowest_balance(self, player_repository):
        """Test steal_atomic tracks lowest_balance_ever for victim."""
        # Add thief and victim
        player_repository.add(
            discord_id=5001,
            discord_username="Thief",
            guild_id=TEST_GUILD_ID,
        )
        player_repository.add(
            discord_id=5002,
            discord_username="Victim",
            guild_id=TEST_GUILD_ID,
        )

        # Steal twice. The second steal pushes the victim to a new low; the
        # tracker should record THAT low rather than the intermediate value.
        player_repository.update_balance(5001, TEST_GUILD_ID, 0)
        player_repository.update_balance(5002, TEST_GUILD_ID, 100)

        player_repository.steal_atomic(
            thief_discord_id=5001,
            victim_discord_id=5002,
            guild_id=TEST_GUILD_ID,
            amount=50,
        )
        # After first steal, victim sits at 50 — that's also the new low.
        assert player_repository.get_lowest_balance(5002, TEST_GUILD_ID) == 50

        # Bump victim back up between steals; lowest should NOT change.
        player_repository.update_balance(5002, TEST_GUILD_ID, 100)
        assert player_repository.get_lowest_balance(5002, TEST_GUILD_ID) == 50

        # Second steal pushes victim below the old low.
        player_repository.steal_atomic(
            thief_discord_id=5001,
            victim_discord_id=5002,
            guild_id=TEST_GUILD_ID,
            amount=70,
        )
        victim = player_repository.get_by_id(5002, TEST_GUILD_ID)
        assert victim.jopacoin_balance == 30
        # Lowest should now reflect the deepest dip (30), not the prior 50.
        assert player_repository.get_lowest_balance(5002, TEST_GUILD_ID) == 30


class TestMatchRepository:
    """Tests for MatchRepository."""

    def test_record_match(self, match_repository):
        """Test recording a match."""
        match_id = match_repository.record_match(
            team1_ids=[1, 2, 3, 4, 5],
            team2_ids=[6, 7, 8, 9, 10],
            winning_team=1,
            guild_id=TEST_GUILD_ID,
        )

        assert match_id > 0

        match = match_repository.get_match(match_id, TEST_GUILD_ID)
        assert match is not None
        assert match["team1_players"] == [1, 2, 3, 4, 5]
        assert match["team2_players"] == [6, 7, 8, 9, 10]
        assert match["winning_team"] == 1

    def test_get_player_matches(self, match_repository):
        """Test getting player match history."""
        # Record a few matches
        match_repository.record_match(
            team1_ids=[1, 2, 3, 4, 5],
            team2_ids=[6, 7, 8, 9, 10],
            winning_team=1,
            guild_id=TEST_GUILD_ID,
        )
        match_repository.record_match(
            team1_ids=[1, 6, 3, 8, 5],
            team2_ids=[2, 7, 4, 9, 10],
            winning_team=2,
            guild_id=TEST_GUILD_ID,
        )

        matches = match_repository.get_player_matches(1, TEST_GUILD_ID, limit=10)
        assert len(matches) == 2

    def test_get_player_recent_outcomes_bulk_matches_point_reads(self, match_repository):
        histories = {
            101: [True, False, True, True],
            202: [False, False, True],
        }
        for discord_id, outcomes in histories.items():
            for won in outcomes:
                match_repository.add_rating_history(
                    discord_id,
                    TEST_GUILD_ID,
                    rating=1500,
                    won=won,
                )
        match_repository.add_rating_history(
            101,
            TEST_GUILD_ID + 1,
            rating=1500,
            won=False,
        )

        bulk = match_repository.get_player_recent_outcomes_bulk(
            [202, 101, 303, 202], TEST_GUILD_ID, limit=3
        )

        assert list(bulk) == [202, 101, 303]
        assert bulk[101] == match_repository.get_player_recent_outcomes(
            101, TEST_GUILD_ID, limit=3
        )
        assert bulk[202] == match_repository.get_player_recent_outcomes(
            202, TEST_GUILD_ID, limit=3
        )
        assert bulk[303] == []
        assert match_repository.get_player_recent_outcomes_bulk([], TEST_GUILD_ID) == {}

    def test_get_player_outcomes_before_match_bulk_is_capped_and_single_query(
        self, match_repository, monkeypatch
    ):
        for won in [True, False, True, True]:
            match_repository.add_rating_history(
                101, TEST_GUILD_ID, rating=1500, won=won
            )
        for won in [False, True]:
            match_repository.add_rating_history(
                202, TEST_GUILD_ID, rating=1500, won=won
            )
        match_repository.add_rating_history(
            101, TEST_GUILD_ID_SECONDARY, rating=1500, won=False
        )
        target_match_id = match_repository.record_match(
            team1_ids=[101],
            team2_ids=[202],
            winning_team=1,
            guild_id=TEST_GUILD_ID,
        )
        match_repository.add_rating_history(
            101,
            TEST_GUILD_ID,
            rating=1510,
            won=True,
            match_id=target_match_id,
        )
        match_repository.add_rating_history(
            202,
            TEST_GUILD_ID,
            rating=1490,
            won=False,
            match_id=target_match_id,
        )
        # Rows written after the target match must not leak into its window.
        match_repository.add_rating_history(
            101, TEST_GUILD_ID, rating=1520, won=False
        )
        match_repository.add_rating_history(
            202, TEST_GUILD_ID, rating=1480, won=True
        )

        connection_count = 0
        original_get_connection = match_repository.get_connection

        def counted_get_connection():
            nonlocal connection_count
            connection_count += 1
            return original_get_connection()

        monkeypatch.setattr(
            match_repository, "get_connection", counted_get_connection
        )

        outcomes = match_repository.get_player_outcomes_before_match_bulk(
            [202, 101, 303, 202],
            TEST_GUILD_ID,
            target_match_id,
            limit=3,
        )

        assert list(outcomes) == [202, 101, 303]
        assert outcomes == {
            202: [True, False],
            101: [True, True, False],
            303: [],
        }
        assert connection_count == 1
        assert (
            match_repository.get_player_outcomes_before_match_bulk(
                [], TEST_GUILD_ID, target_match_id
            )
            == {}
        )
        assert connection_count == 1

    def test_get_os_ratings_for_matches_bulk_and_guild_scoped(self, match_repository):
        match_ids = [
            match_repository.record_match(
                team1_ids=[1, 2, 3, 4, 5],
                team2_ids=[6, 7, 8, 9, 10],
                winning_team=1,
                guild_id=TEST_GUILD_ID,
            )
            for _ in range(2)
        ]
        for match_id in match_ids:
            for discord_id, team_number in ((1, 1), (2, 1), (6, 2), (7, 2)):
                match_repository.add_rating_history(
                    discord_id,
                    TEST_GUILD_ID,
                    rating=1500,
                    match_id=match_id,
                    team_number=team_number,
                    os_mu_before=float(discord_id),
                    os_sigma_before=5.0,
                )
        match_repository.add_rating_history(
            99,
            TEST_GUILD_ID + 1,
            rating=1500,
            match_id=match_ids[0],
            team_number=1,
            os_mu_before=99.0,
            os_sigma_before=5.0,
        )

        ratings = match_repository.get_os_ratings_for_matches(
            [match_ids[1], match_ids[0], 999999, match_ids[1]],
            TEST_GUILD_ID,
        )

        assert list(ratings) == [match_ids[1], match_ids[0], 999999]
        for match_id in match_ids:
            assert sorted(ratings[match_id]["team1"]) == [(1.0, 5.0), (2.0, 5.0)]
            assert sorted(ratings[match_id]["team2"]) == [(6.0, 5.0), (7.0, 5.0)]
        assert ratings[999999] == {"team1": [], "team2": []}
        assert match_repository.get_os_ratings_for_matches([], TEST_GUILD_ID) == {}

    def test_get_os_ratings_for_matches_chunks_on_one_connection(
        self, match_repository, monkeypatch
    ):
        match_ids = list(range(1, 1002))
        connection_count = 0
        original_get_connection = match_repository.get_connection

        def counted_get_connection():
            nonlocal connection_count
            connection_count += 1
            return original_get_connection()

        monkeypatch.setattr(
            match_repository, "get_connection", counted_get_connection
        )

        ratings = match_repository.get_os_ratings_for_matches(
            match_ids, TEST_GUILD_ID
        )

        assert len(ratings) == len(match_ids)
        assert all(
            value == {"team1": [], "team2": []}
            for value in ratings.values()
        )
        assert connection_count == 1

    def test_match_summary_reads_do_not_load_enrichment_data(
        self, match_repository, monkeypatch
    ):
        """Ordinary match reads must not materialize the large enrichment JSON column."""
        team1 = [1, 2, 3, 4, 5]
        team2 = [6, 7, 8, 9, 10]
        match_id = match_repository.record_match(
            team1_ids=team1,
            team2_ids=team2,
            winning_team=1,
            guild_id=TEST_GUILD_ID,
            dotabuff_match_id="dotabuff-123",
            notes="summary contract",
            lobby_type="draft",
            balancing_rating_system="openskill",
        )
        enrichment_payload = "x" * 1_000_000
        match_repository.update_match_enrichment(
            match_id=match_id,
            valve_match_id=8181518332,
            duration_seconds=2400,
            radiant_score=35,
            dire_score=22,
            game_mode=2,
            enrichment_data=enrichment_payload,
        )

        with sqlite3.connect(match_repository.db_path) as conn:
            stored_size = conn.execute(
                "SELECT length(enrichment_data) FROM matches WHERE match_id = ?",
                (match_id,),
            ).fetchone()[0]
        assert stored_size == len(enrichment_payload)

        original_get_connection = match_repository.get_connection
        columns_read: list[tuple[str | None, str | None]] = []

        def get_guarded_connection():
            conn = original_get_connection()

            def deny_enrichment_reads(action, table, column, _database, _trigger):
                if action == sqlite3.SQLITE_READ:
                    columns_read.append((table, column))
                    if table == "matches" and column == "enrichment_data":
                        return sqlite3.SQLITE_DENY
                return sqlite3.SQLITE_OK

            conn.set_authorizer(deny_enrichment_reads)
            return conn

        monkeypatch.setattr(match_repository, "get_connection", get_guarded_connection)

        match = match_repository.get_match(match_id, TEST_GUILD_ID)
        player_matches = match_repository.get_player_matches(1, TEST_GUILD_ID, limit=10)
        most_recent = match_repository.get_most_recent_match(TEST_GUILD_ID)

        assert match == {
            "match_id": match_id,
            "team1_players": team1,
            "team2_players": team2,
            "winning_team": 1,
            "match_date": match["match_date"],
            "dotabuff_match_id": "dotabuff-123",
            "notes": "summary contract",
            "valve_match_id": 8181518332,
            "duration_seconds": 2400,
            "radiant_score": 35,
            "dire_score": 22,
            "game_mode": 2,
            "lobby_type": "draft",
            "balancing_rating_system": "openskill",
        }
        assert player_matches == [
            {
                "match_id": match_id,
                "team1_players": team1,
                "team2_players": team2,
                "winning_team": 1,
                "match_date": match["match_date"],
                "player_team": 1,
                "player_won": True,
                "side": "radiant",
                "valve_match_id": 8181518332,
                "lobby_type": "draft",
            }
        ]
        assert most_recent == {
            "match_id": match_id,
            "team1_players": team1,
            "team2_players": team2,
            "winning_team": 1,
            "match_date": match["match_date"],
            "dotabuff_match_id": "dotabuff-123",
            "valve_match_id": 8181518332,
            "notes": "summary contract",
        }
        assert ("matches", "enrichment_data") not in columns_read

    def test_get_lobby_type_stats_empty(self, match_repository):
        """Test lobby type stats with no data returns empty list."""
        stats = match_repository.get_lobby_type_stats(TEST_GUILD_ID)
        assert stats == []

    def test_get_lobby_type_stats_shuffle_only(self, match_repository):
        """Test lobby type stats with only shuffle matches."""
        # Record a shuffle match
        match_id = match_repository.record_match(
            team1_ids=[1, 2, 3, 4, 5],
            team2_ids=[6, 7, 8, 9, 10],
            winning_team=1,
            guild_id=TEST_GUILD_ID,
            lobby_type="shuffle",
        )

        # Add rating history for the match
        match_repository.add_rating_history(
            discord_id=1,
            guild_id=TEST_GUILD_ID,
            rating=1520,
            match_id=match_id,
            rating_before=1500,
            expected_team_win_prob=0.55,
            won=True,
        )
        match_repository.add_rating_history(
            discord_id=6,
            guild_id=TEST_GUILD_ID,
            rating=1480,
            match_id=match_id,
            rating_before=1500,
            expected_team_win_prob=0.45,
            won=False,
        )

        stats = match_repository.get_lobby_type_stats(TEST_GUILD_ID)
        assert len(stats) == 1
        assert stats[0]["lobby_type"] == "shuffle"
        assert stats[0]["games"] == 2
        assert stats[0]["avg_swing"] == 20.0  # |1520-1500| and |1480-1500| both = 20
        assert stats[0]["actual_win_rate"] == 0.5  # 1 win, 1 loss
        assert stats[0]["expected_win_rate"] == 0.5  # (0.55 + 0.45) / 2

    def test_get_lobby_type_stats_both_types(self, match_repository):
        """Test lobby type stats with both shuffle and draft matches."""
        # Record a shuffle match
        shuffle_match_id = match_repository.record_match(
            team1_ids=[1, 2, 3, 4, 5],
            team2_ids=[6, 7, 8, 9, 10],
            winning_team=1,
            guild_id=TEST_GUILD_ID,
            lobby_type="shuffle",
        )
        match_repository.add_rating_history(
            discord_id=1,
            guild_id=TEST_GUILD_ID,
            rating=1520,
            match_id=shuffle_match_id,
            rating_before=1500,
            expected_team_win_prob=0.50,
            won=True,
        )

        # Record a draft match with larger swing
        draft_match_id = match_repository.record_match(
            team1_ids=[1, 2, 3, 4, 5],
            team2_ids=[6, 7, 8, 9, 10],
            winning_team=2,
            guild_id=TEST_GUILD_ID,
            lobby_type="draft",
        )
        match_repository.add_rating_history(
            discord_id=1,
            guild_id=TEST_GUILD_ID,
            rating=1470,
            match_id=draft_match_id,
            rating_before=1520,
            expected_team_win_prob=0.60,
            won=False,
        )

        stats = match_repository.get_lobby_type_stats(TEST_GUILD_ID)
        assert len(stats) == 2

        shuffle_stats = next(s for s in stats if s["lobby_type"] == "shuffle")
        draft_stats = next(s for s in stats if s["lobby_type"] == "draft")

        assert shuffle_stats["avg_swing"] == 20.0
        assert shuffle_stats["games"] == 1
        assert shuffle_stats["actual_win_rate"] == 1.0  # Won

        assert draft_stats["avg_swing"] == 50.0  # |1470-1520| = 50
        assert draft_stats["games"] == 1
        assert draft_stats["actual_win_rate"] == 0.0  # Lost

    def test_get_player_lobby_type_stats_empty(self, match_repository):
        """Test player lobby type stats with no data returns empty list."""
        stats = match_repository.get_player_lobby_type_stats(discord_id=999, guild_id=TEST_GUILD_ID)
        assert stats == []

    def test_get_player_lobby_type_stats_filters_by_player(self, match_repository):
        """Test player lobby type stats only returns data for the specified player."""
        # Record matches with rating history for multiple players
        match_id = match_repository.record_match(
            team1_ids=[1, 2, 3, 4, 5],
            team2_ids=[6, 7, 8, 9, 10],
            winning_team=1,
            guild_id=TEST_GUILD_ID,
            lobby_type="shuffle",
        )

        # Player 1 rating history
        match_repository.add_rating_history(
            discord_id=1,
            guild_id=TEST_GUILD_ID,
            rating=1530,
            match_id=match_id,
            rating_before=1500,
            expected_team_win_prob=0.55,
            won=True,
        )

        # Player 6 rating history
        match_repository.add_rating_history(
            discord_id=6,
            guild_id=TEST_GUILD_ID,
            rating=1490,
            match_id=match_id,
            rating_before=1500,
            expected_team_win_prob=0.45,
            won=False,
        )

        # Get stats for player 1 only
        stats = match_repository.get_player_lobby_type_stats(discord_id=1, guild_id=TEST_GUILD_ID)
        assert len(stats) == 1
        assert stats[0]["lobby_type"] == "shuffle"
        assert stats[0]["games"] == 1
        assert stats[0]["avg_swing"] == 30.0  # |1530-1500| = 30
        assert stats[0]["actual_win_rate"] == 1.0

        # Get stats for player 6
        stats_p6 = match_repository.get_player_lobby_type_stats(discord_id=6, guild_id=TEST_GUILD_ID)
        assert len(stats_p6) == 1
        assert stats_p6[0]["avg_swing"] == 10.0  # |1490-1500| = 10
        assert stats_p6[0]["actual_win_rate"] == 0.0

    def test_get_player_lobby_type_stats_both_types(self, match_repository):
        """Test player lobby type stats with both shuffle and draft for same player."""
        # Shuffle match
        shuffle_id = match_repository.record_match(
            team1_ids=[1, 2, 3, 4, 5],
            team2_ids=[6, 7, 8, 9, 10],
            winning_team=1,
            guild_id=TEST_GUILD_ID,
            lobby_type="shuffle",
        )
        match_repository.add_rating_history(
            discord_id=1,
            guild_id=TEST_GUILD_ID,
            rating=1520,
            match_id=shuffle_id,
            rating_before=1500,
            expected_team_win_prob=0.50,
            won=True,
        )

        # Draft match
        draft_id = match_repository.record_match(
            team1_ids=[1, 2, 3, 4, 5],
            team2_ids=[6, 7, 8, 9, 10],
            winning_team=1,
            guild_id=TEST_GUILD_ID,
            lobby_type="draft",
        )
        match_repository.add_rating_history(
            discord_id=1,
            guild_id=TEST_GUILD_ID,
            rating=1560,
            match_id=draft_id,
            rating_before=1520,
            expected_team_win_prob=0.45,
            won=True,
        )

        stats = match_repository.get_player_lobby_type_stats(discord_id=1, guild_id=TEST_GUILD_ID)
        assert len(stats) == 2

        shuffle_stats = next(s for s in stats if s["lobby_type"] == "shuffle")
        draft_stats = next(s for s in stats if s["lobby_type"] == "draft")

        assert shuffle_stats["avg_swing"] == 20.0
        assert shuffle_stats["actual_win_rate"] == 1.0

        assert draft_stats["avg_swing"] == 40.0  # |1560-1520| = 40
        assert draft_stats["actual_win_rate"] == 1.0
