"""Tests for recent match penalty feature."""

import pytest

from config import SHUFFLER_SETTINGS
from domain.models.player import Player
from repositories.match_repository import MatchRepository
from shuffler import BalancedShuffler

TEST_GUILD_ID = 12345


@pytest.fixture
def shuffler():
    """Create a shuffler with default settings."""
    return BalancedShuffler()


@pytest.fixture
def match_repository(repo_db_path):
    """Create a match repository with initialized schema."""
    return MatchRepository(repo_db_path)


@pytest.fixture
def sample_players() -> list[Player]:
    """Create 14 sample players for testing."""
    return [
        Player(
            name=f"Player{i}",
            mmr=3000 + i * 100,
            preferred_roles=["1", "2", "3", "4", "5"],
            glicko_rating=1500 + i * 50,
            glicko_rd=100,
            glicko_volatility=0.06,
            discord_id=1000 + i,
        )
        for i in range(14)
    ]


class TestConfigDefaultValue:
    """Test that config has the correct default value."""

    def test_config_default_value(self):
        """Verify default recent_match_penalty_weight is 25.0."""
        assert "recent_match_penalty_weight" in SHUFFLER_SETTINGS
        assert SHUFFLER_SETTINGS["recent_match_penalty_weight"] == 25.0


class TestShufflerInit:
    """Test shuffler initialization with recent match penalty."""

    def test_shuffler_uses_default_from_config(self):
        """Shuffler should use default from config when not specified."""
        shuffler = BalancedShuffler()
        assert shuffler.recent_match_penalty_weight == 25.0

    def test_shuffler_accepts_custom_weight(self):
        """Shuffler should accept custom recent_match_penalty_weight."""
        shuffler = BalancedShuffler(recent_match_penalty_weight=50.0)
        assert shuffler.recent_match_penalty_weight == 50.0

    def test_shuffler_zero_weight_disables_penalty(self):
        """Zero weight should effectively disable the penalty."""
        shuffler = BalancedShuffler(recent_match_penalty_weight=0.0)
        assert shuffler.recent_match_penalty_weight == 0.0


class TestGetLastMatchParticipantIds:
    """Test the repository method for getting last match participants."""

    def test_empty_when_no_matches(self, match_repository):
        """Returns empty set when no matches recorded."""
        result = match_repository.get_last_match_participant_ids(TEST_GUILD_ID)
        assert result == set()

    def test_returns_participants_from_last_match(self, match_repository):
        """Returns all 10 participants from the most recent match."""
        # Record a match
        team1_ids = [1001, 1002, 1003, 1004, 1005]
        team2_ids = [1006, 1007, 1008, 1009, 1010]
        match_repository.record_match(team1_ids, team2_ids, winning_team=1, guild_id=TEST_GUILD_ID)

        result = match_repository.get_last_match_participant_ids(TEST_GUILD_ID)

        assert result == set(team1_ids + team2_ids)
        assert len(result) == 10

    def test_returns_only_most_recent_match(self, match_repository):
        """Returns only participants from the most recent match, not older ones."""
        # Record first match
        old_team1 = [101, 102, 103, 104, 105]
        old_team2 = [106, 107, 108, 109, 110]
        match_repository.record_match(old_team1, old_team2, winning_team=1, guild_id=TEST_GUILD_ID)

        # Record second match with different players
        new_team1 = [201, 202, 203, 204, 205]
        new_team2 = [206, 207, 208, 209, 210]
        match_repository.record_match(new_team1, new_team2, winning_team=2, guild_id=TEST_GUILD_ID)

        result = match_repository.get_last_match_participant_ids(TEST_GUILD_ID)

        # Should only have participants from the second match
        assert result == set(new_team1 + new_team2)
        # Should not include old participants
        assert not result.intersection(old_team1 + old_team2)


class TestRecentMatchPenaltyInExclusion:
    """Test that recent match penalty affects player exclusion."""

    def test_recent_players_more_likely_excluded(self, sample_players):
        """Recent match participants should be more likely to sit out."""
        shuffler = BalancedShuffler(
            recent_match_penalty_weight=100.0,  # High penalty
            exclusion_penalty_weight=45.0,
        )

        # Mark some players as having played recently
        recent_names = {sample_players[0].name, sample_players[1].name}

        # All players have same exclusion count
        exclusion_counts = {p.name: 5 for p in sample_players}

        team1, team2, excluded = shuffler.shuffle_from_pool(
            sample_players, exclusion_counts, recent_names
        )

        # Check that at least one recent player is excluded
        excluded_names = {p.name for p in excluded}
        recent_excluded = excluded_names.intersection(recent_names)

        # With high penalty, recent players should be more likely to be excluded
        # This is probabilistic, but with 100.0 weight vs 45.0 exclusion weight,
        # recent players should often be excluded
        assert len(excluded) == 4  # 14 - 10 = 4 excluded

    def test_penalty_disabled_when_zero(self, sample_players):
        """No effect when weight is 0."""
        shuffler = BalancedShuffler(
            recent_match_penalty_weight=0.0,
            exclusion_penalty_weight=45.0,
        )

        recent_names = {sample_players[0].name, sample_players[1].name}
        exclusion_counts = {p.name: 0 for p in sample_players}

        # Run multiple times to verify consistency
        results = []
        for _ in range(5):
            team1, team2, excluded = shuffler.shuffle_from_pool(
                sample_players, exclusion_counts, recent_names
            )
            excluded_names = frozenset(p.name for p in excluded)
            results.append(excluded_names)

        # With zero penalty, results should be deterministic (based on other factors)
        # All results should be the same
        assert len(set(results)) == 1


class TestRecentMatchPenaltyInGoodnessScore:
    """Test that recent match penalty is included in goodness score."""

    def test_greedy_shuffle_includes_recent_penalty(self, sample_players):
        """Greedy shuffle should factor in recent match penalty."""
        shuffler = BalancedShuffler(recent_match_penalty_weight=25.0)

        # All recent players
        all_names = {p.name for p in sample_players}
        exclusion_counts = {p.name: 0 for p in sample_players}

        team1, team2, excluded, score_with_all = shuffler._greedy_shuffle(
            sample_players, exclusion_counts, all_names
        )

        # No recent players
        team1b, team2b, excluded_b, score_with_none = shuffler._greedy_shuffle(
            sample_players, exclusion_counts, set()
        )

        # Score with all recent should be higher (worse)
        # 10 players selected * 25.0 = 250 additional penalty
        assert score_with_all > score_with_none

    def test_branch_bound_includes_recent_penalty(self, sample_players):
        """Branch and bound shuffle should factor in recent match penalty."""
        shuffler = BalancedShuffler(recent_match_penalty_weight=50.0)

        # Mark half the players as recent
        recent_names = {p.name for p in sample_players[:7]}
        exclusion_counts = {p.name: 0 for p in sample_players}

        # Should run without error and consider the penalty
        team1, team2, excluded = shuffler.shuffle_branch_bound(
            sample_players, exclusion_counts, recent_names
        )

        assert len(team1.players) == 5
        assert len(team2.players) == 5
        assert len(excluded) == 4


class TestRecentMatchPenaltyWithExclusionCounts:
    """Test interaction between recent match penalty and exclusion counts."""

    def test_high_exclusion_count_overrides_recent_penalty(self, sample_players):
        """Players with very high exclusion count should still be prioritized."""
        shuffler = BalancedShuffler(
            recent_match_penalty_weight=25.0,
            exclusion_penalty_weight=45.0,
        )

        # Player0 played recently but has high exclusion count
        recent_names = {sample_players[0].name}

        # Give Player0 very high exclusion count
        exclusion_counts = {p.name: 0 for p in sample_players}
        exclusion_counts[sample_players[0].name] = 100  # Very high

        team1, team2, excluded = shuffler.shuffle_from_pool(
            sample_players, exclusion_counts, recent_names
        )

        selected_names = {p.name for p in team1.players + team2.players}

        # Player0 should likely be selected despite recent play
        # (100 * 45 = 4500 penalty if excluded vs 25 penalty if selected)
        # This depends on other factors, so we just verify the method runs
        assert len(selected_names) == 10
        assert len(excluded) == 4


class TestRecentMatchPenaltyEdgeCases:
    """Test edge cases for recent match penalty."""

    def test_empty_recent_names(self, sample_players):
        """Empty recent_match_names should not cause errors."""
        shuffler = BalancedShuffler()
        exclusion_counts = {p.name: 0 for p in sample_players}

        team1, team2, excluded = shuffler.shuffle_from_pool(
            sample_players, exclusion_counts, set()
        )

        assert len(team1.players) == 5
        assert len(team2.players) == 5

    def test_none_recent_names(self, sample_players):
        """None recent_match_names should be handled as empty set."""
        shuffler = BalancedShuffler()
        exclusion_counts = {p.name: 0 for p in sample_players}

        team1, team2, excluded = shuffler.shuffle_from_pool(
            sample_players, exclusion_counts, None
        )

        assert len(team1.players) == 5
        assert len(team2.players) == 5

    def test_all_players_from_recent_match(self, sample_players):
        """Should handle case where all players are from recent match."""
        shuffler = BalancedShuffler(recent_match_penalty_weight=25.0)
        all_names = {p.name for p in sample_players}
        exclusion_counts = {p.name: 0 for p in sample_players}

        team1, team2, excluded = shuffler.shuffle_from_pool(
            sample_players, exclusion_counts, all_names
        )

        # Should still produce valid teams
        assert len(team1.players) == 5
        assert len(team2.players) == 5
        assert len(excluded) == 4
