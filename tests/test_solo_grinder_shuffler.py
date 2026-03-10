"""Tests for solo grinder mix penalty in the shuffler."""

import pytest

from domain.models.player import Player
from shuffler import BalancedShuffler


def _make_player(name, discord_id, mmr=3000, roles=None, is_grinder=False):
    return Player(
        name=name,
        mmr=mmr,
        discord_id=discord_id,
        preferred_roles=roles or ["1", "2", "3", "4", "5"],
        glicko_rating=1500.0,
        glicko_rd=100.0,
        glicko_volatility=0.06,
        is_solo_grinder=is_grinder,
    )


class TestSoloGrinderPenalty:
    """Test _calculate_solo_grinder_penalty directly."""

    def test_no_grinders_returns_zero(self):
        shuffler = BalancedShuffler()
        team1_ids = {1, 2, 3, 4, 5}
        team2_ids = {6, 7, 8, 9, 10}
        assert shuffler._calculate_solo_grinder_penalty(team1_ids, team2_ids, None) == 0.0
        assert shuffler._calculate_solo_grinder_penalty(team1_ids, team2_ids, set()) == 0.0

    def test_pure_grinder_team_returns_zero(self):
        """All grinders on one team, all casuals on the other = 0 penalty."""
        shuffler = BalancedShuffler()
        team1_ids = {1, 2, 3, 4, 5}
        team2_ids = {6, 7, 8, 9, 10}
        grinder_ids = {1, 2, 3, 4, 5}  # All on team1
        assert shuffler._calculate_solo_grinder_penalty(team1_ids, team2_ids, grinder_ids) == 0.0

    def test_one_mixed_team_returns_100(self):
        """One team mixed, one team pure = 100 penalty."""
        shuffler = BalancedShuffler()
        team1_ids = {1, 2, 3, 4, 5}
        team2_ids = {6, 7, 8, 9, 10}
        grinder_ids = {1, 2, 3}  # 3 grinders on team1 (mixed), 0 on team2 (pure casual)
        assert shuffler._calculate_solo_grinder_penalty(team1_ids, team2_ids, grinder_ids) == 110.0

    def test_both_mixed_teams_returns_220(self):
        """Both teams mixed = 220 penalty."""
        shuffler = BalancedShuffler()
        team1_ids = {1, 2, 3, 4, 5}
        team2_ids = {6, 7, 8, 9, 10}
        grinder_ids = {1, 2, 7, 8}  # 2 on each team, both mixed
        assert shuffler._calculate_solo_grinder_penalty(team1_ids, team2_ids, grinder_ids) == 220.0

    def test_custom_penalty_value(self):
        shuffler = BalancedShuffler(solo_grinder_mix_penalty=50.0)
        team1_ids = {1, 2, 3, 4, 5}
        team2_ids = {6, 7, 8, 9, 10}
        grinder_ids = {1, 7}  # One on each team, both mixed
        assert shuffler._calculate_solo_grinder_penalty(team1_ids, team2_ids, grinder_ids) == 100.0


class TestSoloGrinderShuffleIntegration:
    """Test that grinder penalty influences shuffle outcome."""

    def test_shuffle_prefers_pure_teams(self):
        """With a high penalty, shuffler should prefer grouping grinders together."""
        # Create 5 grinders and 5 casuals with equal ratings
        players = []
        for i in range(5):
            players.append(_make_player(f"Grinder{i}", discord_id=i + 1, mmr=3000, is_grinder=True))
        for i in range(5):
            players.append(_make_player(f"Casual{i}", discord_id=i + 6, mmr=3000, is_grinder=False))

        grinder_ids = {p.discord_id for p in players if p.is_solo_grinder}

        # With high penalty, should prefer pure teams
        shuffler = BalancedShuffler(solo_grinder_mix_penalty=500.0)
        team1, team2 = shuffler.shuffle(players, grinder_ids=grinder_ids)

        t1_ids = {p.discord_id for p in team1.players}
        t2_ids = {p.discord_id for p in team2.players}

        # With equal MMR and high penalty, teams should be pure
        t1_grinders = len(grinder_ids & t1_ids)
        t2_grinders = len(grinder_ids & t2_ids)

        # One team should have all 5 grinders, the other none
        assert (t1_grinders == 5 and t2_grinders == 0) or (t1_grinders == 0 and t2_grinders == 5)

    def test_shuffle_without_grinder_ids_works(self):
        """Shuffle should work normally when grinder_ids is None."""
        players = [_make_player(f"P{i}", discord_id=i + 1, mmr=3000 + i * 100) for i in range(10)]
        shuffler = BalancedShuffler()
        team1, team2 = shuffler.shuffle(players, grinder_ids=None)
        assert len(team1.players) == 5
        assert len(team2.players) == 5

    def test_shuffle_from_pool_threads_grinder_ids(self):
        """shuffle_from_pool should accept and use grinder_ids."""
        players = [_make_player(f"P{i}", discord_id=i + 1, mmr=3000 + i * 100) for i in range(10)]
        grinder_ids = {1, 2, 3}
        shuffler = BalancedShuffler()
        team1, team2, excluded = shuffler.shuffle_from_pool(players, grinder_ids=grinder_ids)
        assert len(team1.players) == 5
        assert len(team2.players) == 5
        assert len(excluded) == 0
