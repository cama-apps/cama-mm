"""Tests for recent match penalty feature."""

import pytest

from config import SHUFFLER_SETTINGS
from domain.models.player import Player
from domain.models.team import Team
from shuffler import BalancedShuffler
from tests.conftest import TEST_GUILD_ID


@pytest.fixture
def shuffler():
    """Create a shuffler with default settings."""
    return BalancedShuffler()




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
        """Verify default recent_match_penalty_weight is 210.0."""
        assert "recent_match_penalty_weight" in SHUFFLER_SETTINGS
        assert SHUFFLER_SETTINGS["recent_match_penalty_weight"] == 210.0


class TestShufflerInit:
    """Test shuffler initialization with recent match penalty."""

    def test_shuffler_uses_default_from_config(self):
        """Shuffler should use default from config when not specified."""
        shuffler = BalancedShuffler()
        assert shuffler.recent_match_penalty_weight == 210.0

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

    def test_greedy_shuffle_prefers_excluding_recent_players(self, sample_players):
        """Recent match participants should be preferred for exclusion."""
        shuffler = BalancedShuffler(
            recent_match_penalty_weight=100.0,
            exclusion_penalty_weight=50.0,
        )
        recent_names = {sample_players[-2].name, sample_players[-1].name}
        exclusion_counts = {p.name: 5 for p in sample_players}

        _, _, excluded, _ = shuffler._greedy_shuffle(
            sample_players, exclusion_counts, recent_names
        )

        assert recent_names <= {p.name for p in excluded}

    def test_penalty_disabled_when_zero(self, sample_players):
        """Recent participants do not affect selection or score at zero weight."""
        shuffler = BalancedShuffler(
            recent_match_penalty_weight=0.0,
            exclusion_penalty_weight=50.0,
        )

        recent_names = {sample_players[0].name, sample_players[1].name}
        exclusion_counts = {p.name: 0 for p in sample_players}

        with_recent = shuffler._greedy_shuffle(
            sample_players, exclusion_counts, recent_names
        )
        without_recent = shuffler._greedy_shuffle(
            sample_players, exclusion_counts, set()
        )

        assert {p.name for p in with_recent[2]} == {p.name for p in without_recent[2]}
        assert with_recent[3] == pytest.approx(without_recent[3])


class TestRecentMatchPenaltyInGoodnessScore:
    """Test that recent match penalty is included in goodness score."""

    def test_greedy_shuffle_includes_recent_penalty(self, sample_players):
        """Greedy shuffle should factor in recent match penalty."""
        shuffler = BalancedShuffler(recent_match_penalty_weight=25.0)

        # All recent players
        all_names = {p.name for p in sample_players}
        exclusion_counts = {p.name: 0 for p in sample_players}

        *_, score_with_all = shuffler._greedy_shuffle(
            sample_players, exclusion_counts, all_names
        )

        # No recent players
        *_, score_with_none = shuffler._greedy_shuffle(
            sample_players, exclusion_counts, set()
        )

        assert score_with_all - score_with_none == pytest.approx(250.0)

    def test_branch_bound_prefers_excluding_recent_players(self, monkeypatch):
        """Branch-and-bound selection includes the recent-player penalty."""
        players = [
            Player(
                name=f"Player{i}",
                mmr=1500,
                glicko_rd=0,
                preferred_roles=["1", "2", "3", "4", "5"],
            )
            for i in range(14)
        ]
        shuffler = BalancedShuffler(
            use_glicko=False,
            recent_match_penalty_weight=50.0,
            exclusion_penalty_weight=0.0,
            rd_priority_weight=0.0,
        )
        roles = ["1", "2", "3", "4", "5"]
        greedy_result = (
            Team(players[:5], role_assignments=roles),
            Team(players[5:10], role_assignments=roles),
            players[10:],
            float("inf"),
        )
        forwarded_recent_names = []

        def greedy(_players, _exclusion_counts, recent_match_names, **_kwargs):
            forwarded_recent_names.append(recent_match_names)
            return greedy_result

        monkeypatch.setattr(shuffler, "_greedy_shuffle", greedy)
        monkeypatch.setattr(
            shuffler,
            "_optimize_role_assignments_for_matchup",
            lambda team1, team2, **kwargs: (
                Team(team1, role_assignments=roles),
                Team(team2, role_assignments=roles),
                0.0,
            ),
        )
        recent_names = {p.name for p in players[:4]}

        _, _, excluded = shuffler.shuffle_from_pool(
            players,
            {p.name: 0 for p in players},
            recent_names,
        )

        assert forwarded_recent_names == [recent_names]
        assert {p.name for p in excluded} == recent_names


class TestRecentMatchPenaltyWithExclusionCounts:
    """Test interaction between recent match penalty and exclusion counts."""

    def test_high_exclusion_count_overrides_recent_penalty(self, sample_players):
        """Players with very high exclusion count should still be prioritized."""
        shuffler = BalancedShuffler(
            recent_match_penalty_weight=25.0,
            exclusion_penalty_weight=50.0,
        )

        # Player0 played recently but has high exclusion count
        recent_names = {sample_players[0].name}

        # Give Player0 very high exclusion count
        exclusion_counts = {p.name: 0 for p in sample_players}
        exclusion_counts[sample_players[0].name] = 100  # Very high

        team1, team2, excluded, _ = shuffler._greedy_shuffle(
            sample_players, exclusion_counts, recent_names
        )

        selected_names = {p.name for p in team1.players + team2.players}

        assert sample_players[0].name in selected_names
        assert sample_players[0].name not in {p.name for p in excluded}
