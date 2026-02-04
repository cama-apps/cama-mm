"""
Tests for the repository layer.
"""

from config import NEW_PLAYER_EXCLUSION_BOOST

# Test guild ID used for all repository tests
TEST_GUILD_ID = 12345


def _expected_after_exclusions(exclusions: int) -> int:
    return NEW_PLAYER_EXCLUSION_BOOST + exclusions * 4


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

        # Increment twice (4 per exclusion)
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

        match = match_repository.get_match(match_id)
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

    def test_get_lobby_type_stats_empty(self, match_repository):
        """Test lobby type stats with no data returns empty list."""
        stats = match_repository.get_lobby_type_stats()
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

        stats = match_repository.get_lobby_type_stats()
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

        stats = match_repository.get_lobby_type_stats()
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
