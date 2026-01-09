"""
Tests for the repository layer.
"""


class TestPlayerRepository:
    """Tests for PlayerRepository."""

    def test_add_and_get_player(self, player_repository):
        """Test adding and retrieving a player."""
        player_repository.add(
            discord_id=12345,
            discord_username="TestPlayer",
            initial_mmr=3000,
            preferred_roles=["1", "2"],
            glicko_rating=1500,
            glicko_rd=350,
            glicko_volatility=0.06,
        )

        player = player_repository.get_by_id(12345)
        assert player is not None
        assert player.name == "TestPlayer"
        assert player.mmr == 3000
        assert player.preferred_roles == ["1", "2"]

    def test_player_not_found(self, player_repository):
        """Test getting a non-existent player."""
        player = player_repository.get_by_id(99999)
        assert player is None

    def test_exists(self, player_repository):
        """Test player existence check."""
        player_repository.add(
            discord_id=12345,
            discord_username="TestPlayer",
        )

        assert player_repository.exists(12345) is True
        assert player_repository.exists(99999) is False

    def test_update_roles(self, player_repository):
        """Test updating player roles."""
        player_repository.add(
            discord_id=12345,
            discord_username="TestPlayer",
        )

        player_repository.update_roles(12345, ["3", "4", "5"])

        player = player_repository.get_by_id(12345)
        assert player.preferred_roles == ["3", "4", "5"]

    def test_update_glicko_rating(self, player_repository):
        """Test updating Glicko rating."""
        player_repository.add(
            discord_id=12345,
            discord_username="TestPlayer",
            glicko_rating=1500,
            glicko_rd=350,
            glicko_volatility=0.06,
        )

        player_repository.update_glicko_rating(12345, 1600, 300, 0.05)

        rating = player_repository.get_glicko_rating(12345)
        assert rating[0] == 1600
        assert rating[1] == 300
        assert rating[2] == 0.05

    def test_get_by_ids_preserves_order(self, player_repository):
        """Test that get_by_ids preserves input order."""
        for i in range(5):
            player_repository.add(
                discord_id=1000 + i,
                discord_username=f"Player{i}",
            )

        ids = [1003, 1001, 1004, 1000, 1002]
        players = player_repository.get_by_ids(ids)

        assert len(players) == 5
        assert [p.name for p in players] == ["Player3", "Player1", "Player4", "Player0", "Player2"]

    def test_get_by_username_partial_case_insensitive(self, player_repository):
        """Test username lookup supports partial and case-insensitive matching."""
        player_repository.add(discord_id=2001, discord_username="AlphaUser")
        player_repository.add(discord_id=2002, discord_username="betaUser")
        player_repository.add(discord_id=2003, discord_username="GammaTester")

        matches = player_repository.get_by_username("alpha")
        assert len(matches) == 1
        assert matches[0]["discord_id"] == 2001

        matches = player_repository.get_by_username("USER")
        assert {m["discord_id"] for m in matches} == {2001, 2002}

        matches = player_repository.get_by_username("gamma")
        assert len(matches) == 1
        assert matches[0]["discord_username"] == "GammaTester"

    def test_exclusion_counts(self, player_repository):
        """Test exclusion count operations."""
        player_repository.add(
            discord_id=12345,
            discord_username="TestPlayer",
        )

        # Initial count should be 0
        counts = player_repository.get_exclusion_counts([12345])
        assert counts[12345] == 0

        # Increment twice (4 per exclusion)
        player_repository.increment_exclusion_count(12345)
        player_repository.increment_exclusion_count(12345)
        counts = player_repository.get_exclusion_counts([12345])
        assert counts[12345] == 8

        # Decay (halves the count)
        player_repository.decay_exclusion_count(12345)
        counts = player_repository.get_exclusion_counts([12345])
        assert counts[12345] == 4

    def test_delete_player(self, player_repository):
        """Test deleting a player."""
        player_repository.add(
            discord_id=12345,
            discord_username="TestPlayer",
        )

        assert player_repository.delete(12345) is True
        assert player_repository.get_by_id(12345) is None
        assert player_repository.delete(12345) is False  # Already deleted


class TestMatchRepository:
    """Tests for MatchRepository."""

    def test_record_match(self, match_repository):
        """Test recording a match."""
        match_id = match_repository.record_match(
            team1_ids=[1, 2, 3, 4, 5],
            team2_ids=[6, 7, 8, 9, 10],
            winning_team=1,
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
        )
        match_repository.record_match(
            team1_ids=[1, 6, 3, 8, 5],
            team2_ids=[2, 7, 4, 9, 10],
            winning_team=2,
        )

        matches = match_repository.get_player_matches(1, limit=10)
        assert len(matches) == 2
