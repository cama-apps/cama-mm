"""
Unit tests for the shuffler algorithm and team balancing logic.
"""

import math
import random
from types import SimpleNamespace

import pytest

from domain.models.player import Player
from domain.models.team import Team
from domain.services.team_balancing_service import TeamBalancingService
from shuffler import BalancedShuffler, _PoolMatchup
from utils.region import region_split_mismatches, resolve_region


def test_low_priority_penalty_is_500_per_selected_player():
    shuffler = BalancedShuffler()

    assert shuffler.calculate_low_priority_penalty(
        {100, 101, 102},
        {100, 101, 999},
    ) == 1000.0


def test_pool_shuffle_prefers_excluding_low_priority_player():
    players = [
        Player(
            name=f"Player{i}",
            mmr=3000,
            preferred_roles=["1", "2", "3", "4", "5"],
            discord_id=100 + i,
        )
        for i in range(11)
    ]
    shuffler = BalancedShuffler(
        use_glicko=False,
        exclusion_penalty_weight=0.0,
        recent_match_penalty_weight=0.0,
        rating_spread_divisor=10.0,
        rd_priority_weight=0.0,
    )

    _, _, excluded = shuffler.shuffle_from_pool(
        players,
        low_priority_ids={100},
        rng=random.Random(0),
    )

    assert {player.discord_id for player in excluded} == {100}


@pytest.mark.parametrize(("player_count", "long_waiter_id"), [(11, 109), (14, 100)])
def test_pool_shuffle_includes_player_with_longer_lobby_wait(
    player_count: int,
    long_waiter_id: int,
):
    players = [
        Player(
            name=f"Player{i}",
            discord_id=100 + i,
            mmr=1500,
            preferred_roles=["1", "2", "3", "4", "5"],
        )
        for i in range(player_count)
    ]
    shuffler = BalancedShuffler(
        use_glicko=False,
        exclusion_penalty_weight=0.0,
        recent_match_penalty_weight=0.0,
        rd_priority_weight=0.0,
    )

    team1, team2, _ = shuffler.shuffle_from_pool(
        players,
        lobby_wait_minutes={long_waiter_id: 30},
    )

    selected_ids = {player.discord_id for player in team1.players + team2.players}
    assert long_waiter_id in selected_ids


class TestPlayer:
    """Test Player class functionality."""

    def test_player_value_rating_sources(self):
        """get_value uses Glicko-2 when requested, falls back to MMR otherwise,
        and returns 0 with no rating data at all."""
        player = Player(name="TestPlayer", mmr=2000, glicko_rating=1800, wins=5, losses=3)
        assert player.get_value(use_glicko=True) == 1800
        assert player.get_value(use_glicko=False) == 2000
        assert Player(name="Unrated").get_value() == 0

    def test_glicko_and_openskill_values_agree_at_same_mmr(self):
        """A Glicko-rated player and an OpenSkill-rated player seeded from the
        same MMR should produce comparable ``get_value`` outputs, proving the
        two rating systems share a common 0-3000 display scale.
        """
        from openskill_rating_system import CamaOpenSkillSystem
        from rating_system import CamaRatingSystem

        glicko = CamaRatingSystem()
        os_system = CamaOpenSkillSystem()

        for mmr in (0, 3000, 6000, 9000, 12000):
            glicko_player = Player(
                name=f"G{mmr}",
                mmr=mmr,
                glicko_rating=glicko.mmr_to_rating(mmr),
            )
            os_player = Player(
                name=f"O{mmr}",
                mmr=mmr,
                os_mu=os_system.mmr_to_os_mu(mmr),
            )

            glicko_value = glicko_player.get_value(use_glicko=True)
            os_value = os_player.get_value(use_openskill=True)

            assert abs(glicko_value - os_value) <= 1.0, (
                f"Glicko and OpenSkill values diverge at MMR {mmr}: "
                f"glicko={glicko_value}, openskill={os_value}"
            )


class TestTeam:
    """Test Team class functionality."""

    def test_team_creation(self):
        """Test team creation with 5 players."""
        players = [Player(name=f"Player{i}", mmr=1500) for i in range(5)]
        team = Team(players)
        assert len(team.players) == 5

    def test_team_creation_wrong_size(self):
        """Test that team creation fails with wrong number of players."""
        players = [Player(name=f"Player{i}", mmr=1500) for i in range(4)]
        with pytest.raises(ValueError):
            Team(players)

    def test_get_team_value_requires_role_assignments(self):
        """get_team_value must raise when role_assignments were never set."""
        players = [
            Player(name="P1", mmr=2000, preferred_roles=["1"]),
            Player(name="P2", mmr=1800, preferred_roles=["2"]),
            Player(name="P3", mmr=1600, preferred_roles=["3"]),
            Player(name="P4", mmr=1400, preferred_roles=["4"]),
            Player(name="P5", mmr=1200, preferred_roles=["5"]),
        ]
        team = Team(players)
        with pytest.raises(ValueError, match="role_assignments"):
            team.get_team_value(use_glicko=False)
        with pytest.raises(ValueError, match="role_assignments"):
            team.get_off_role_count()
        with pytest.raises(ValueError, match="role_assignments"):
            team.get_player_by_role("1", use_glicko=False)

    def test_ensure_role_assignments_populates_and_is_idempotent(self):
        """ensure_role_assignments fills in optimal assignments the first time
        and is a no-op afterwards.
        """
        players = [
            Player(name="P1", mmr=2000, preferred_roles=["1"]),
            Player(name="P2", mmr=1800, preferred_roles=["2"]),
            Player(name="P3", mmr=1600, preferred_roles=["3"]),
            Player(name="P4", mmr=1400, preferred_roles=["4"]),
            Player(name="P5", mmr=1200, preferred_roles=["5"]),
        ]
        team = Team(players)
        first = team.ensure_role_assignments()
        assert first == team.role_assignments
        assert set(first) == {"1", "2", "3", "4", "5"}

        # Idempotent: second call should return the same reference without
        # recomputing (role_assignments is already set).
        team.role_assignments = ["5", "4", "3", "2", "1"]
        again = team.ensure_role_assignments()
        assert again == ["5", "4", "3", "2", "1"]

    def test_team_value_and_off_role_count(self):
        """Team value applies the off-role multiplier only to off-role players,
        and get_off_role_count counts them (0 when everyone is on-role)."""
        players = [
            Player(name="P1", mmr=2000, preferred_roles=["1"]),
            Player(name="P2", mmr=1800, preferred_roles=["2"]),
            Player(name="P3", mmr=1600, preferred_roles=["3"]),
            Player(name="P4", mmr=1400, preferred_roles=["4"]),
            Player(name="P5", mmr=1200, preferred_roles=["5"]),
        ]
        on_role = Team(players, role_assignments=["1", "2", "3", "4", "5"])
        # All on-role, so full value: 2000 + 1800 + 1600 + 1400 + 1200 = 8000
        assert on_role.get_team_value(use_glicko=False, off_role_multiplier=0.9) == 8000
        assert on_role.get_off_role_count() == 0

        # P1 playing role 2 (off-role), P2 playing role 1 (off-role)
        off_role = Team(players, role_assignments=["2", "1", "3", "4", "5"])
        # P1 (2000) and P2 (1800) are off-role: 2000*0.9 + 1800*0.9 = 3420
        # P3, P4, P5 on-role: 1600 + 1400 + 1200 = 4200
        # Total: 3420 + 4200 = 7620
        assert off_role.get_team_value(
            use_glicko=False, off_role_multiplier=0.9
        ) == pytest.approx(7620)
        assert off_role.get_off_role_count() == 2

    def test_role_assignment_optimal(self):
        """Test that optimal role assignment minimizes off-roles."""
        players = [
            Player(name="P1", mmr=2000, preferred_roles=["1", "2"]),
            Player(name="P2", mmr=1800, preferred_roles=["2", "3"]),
            Player(name="P3", mmr=1600, preferred_roles=["3"]),
            Player(name="P4", mmr=1400, preferred_roles=["4", "5"]),
            Player(name="P5", mmr=1200, preferred_roles=["5"]),
        ]
        team = Team(players)
        # Should assign roles optimally
        assignments = team.ensure_role_assignments()
        assert len(assignments) == 5
        assert set(assignments) == {"1", "2", "3", "4", "5"}
        # Check that off-role count is minimized
        off_roles = team.get_off_role_count()
        assert off_roles <= 2  # Should be able to assign most players to preferred roles

    def test_get_player_by_role_effective_value(self):
        """Ensure get_player_by_role returns correct player and value."""
        players = [
            Player(name="Carry", mmr=2000, preferred_roles=["1"]),
            Player(name="Mid", mmr=1800, preferred_roles=["2"]),
            Player(name="Offlane", mmr=1600, preferred_roles=["3"]),
            Player(name="Support1", mmr=1400, preferred_roles=["4"]),
            Player(name="Support2", mmr=1200, preferred_roles=["5"]),
        ]
        team = Team(players, role_assignments=["1", "2", "3", "4", "5"])

        player, value = team.get_player_by_role("1", use_glicko=False, off_role_multiplier=0.5)
        assert player.name == "Carry"
        assert value == 2000

        # Move mid to off-role to trigger multiplier
        team.role_assignments = ["3", "1", "2", "4", "5"]
        player, value = team.get_player_by_role("1", use_glicko=False, off_role_multiplier=0.5)
        assert player.name == "Mid"
        assert value == pytest.approx(1800 * 0.5)


class TestShuffler:
    """Test BalancedShuffler algorithm."""

    @staticmethod
    def _mock_pool_matchup(
        team1_players: list[Player],
        team2_players: list[Player],
        numeric_prefix: tuple[float, float, int],
    ) -> _PoolMatchup:
        total_score, value_diff, total_off_roles = numeric_prefix
        roles = ("1", "2", "3", "4", "5")
        team1 = Team(team1_players, role_assignments=roles)
        team2 = Team(team2_players, role_assignments=roles)
        return _PoolMatchup(
            team1=team1,
            team2=team2,
            total_score=total_score,
            value_diff=value_diff,
            total_off_roles=total_off_roles,
            log_entry=(
                total_score,
                value_diff,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                0,
                0,
                [],
                team1,
                team2,
            ),
        )

    def test_get_cached_role_assignments_reuses_immutable_cache_value(
        self, monkeypatch
    ):
        players = [
            Player(name=f"Player{i}", preferred_roles=[str(i + 1)])
            for i in range(5)
        ]
        cached_assignments = (("1", "2", "3", "4", "5"),)
        received_keys = []

        def get_cached_assignments(player_roles_key):
            received_keys.append(player_roles_key)
            return cached_assignments

        monkeypatch.setattr(
            "shuffler.get_cached_role_assignments", get_cached_assignments
        )
        shuffler = BalancedShuffler()

        first_result = shuffler._get_cached_role_assignments(players)
        second_result = shuffler._get_cached_role_assignments(players)

        expected_key = (("1",), ("2",), ("3",), ("4",), ("5",))
        assert received_keys == [expected_key, expected_key]
        assert first_result is cached_assignments
        assert second_result is cached_assignments
        assert isinstance(first_result, tuple)
        assert all(isinstance(assignment, tuple) for assignment in first_result)

    def test_pool_rating_bonus_keeps_high_rating_outlier(self):
        ratings = [1000, 1400, 1450, 1475, 1500, 1525, 1550, 1575, 1600, 1650, 2200]
        players = [
            Player(
                name=f"Player{i}",
                discord_id=i + 1,
                glicko_rating=rating,
                preferred_roles=["1", "2", "3", "4", "5"],
            )
            for i, rating in enumerate(ratings)
        ]
        shuffler = BalancedShuffler(
            use_glicko=True,
            off_role_flat_penalty=0.0,
            role_matchup_delta_weight=0.0,
            exclusion_penalty_weight=0.0,
            rd_priority_weight=0.0,
            recent_match_penalty_weight=0.0,
            rating_spread_divisor=10.0,
        )

        _, _, excluded = shuffler.shuffle_from_pool(players)

        assert [player.glicko_rating for player in excluded] == [1475]

    def test_shuffle_evaluates_duplicate_display_names_by_identity(self, monkeypatch):
        """Duplicate Discord names must not collapse distinct team splits."""
        players = [
            Player(name="SameName", mmr=1500 + i, discord_id=i + 1)
            for i in range(10)
        ]
        shuffler = BalancedShuffler(use_glicko=False)
        evaluated_splits = []
        roles = ["1", "2", "3", "4", "5"]

        def optimize(team1_players, team2_players, **_kwargs):
            evaluated_splits.append(
                frozenset(player.discord_id for player in team1_players)
            )
            return (
                Team(team1_players, role_assignments=roles),
                Team(team2_players, role_assignments=roles),
                1.0,
            )

        monkeypatch.setattr(shuffler, "_optimize_role_assignments_for_matchup", optimize)

        shuffler.shuffle(players)

        assert len(evaluated_splits) == 126
        assert len(set(evaluated_splits)) == 126
        assert all(1 in split for split in evaluated_splits)

    def test_rd_priority_bonus_scales_with_sum(self):
        """RD bonus should reflect the sum of player RD values times the weight."""
        players = [
            Player(name="HighRD", mmr=1500, glicko_rd=200.0),
            Player(name="MidRD", mmr=1500, glicko_rd=50.0),
        ]
        weight = 0.1
        shuffler = BalancedShuffler(rd_priority_weight=weight)

        bonus = shuffler._calculate_rd_priority(players)
        assert bonus == pytest.approx((200.0 + 50.0) * weight)

    def test_shuffle_reuses_rd_priority_for_same_player_selection(self, monkeypatch):
        players = [
            Player(
                name=f"Player{i}",
                discord_id=i + 1,
                glicko_rating=1300.0 + i * 40,
                glicko_rd=75.0 + i,
                preferred_roles=["1", "2", "3", "4", "5"],
            )
            for i in range(10)
        ]
        shuffler = BalancedShuffler(rd_priority_weight=0.137)
        priority_calls = 0
        original = shuffler._calculate_rd_priority

        def counted(selected_players):
            nonlocal priority_calls
            priority_calls += 1
            return original(selected_players)

        monkeypatch.setattr(shuffler, "_calculate_rd_priority", counted)

        shuffler.shuffle(players)

        assert priority_calls == 1

    def test_rd_priority_favors_high_rd_players_in_pool(self):
        """With high RD weight, high-RD players should be favored for inclusion."""
        # 11 players: 10 active (low RD) and 1 inactive/new (high RD)
        # All equal skill, so the only differentiator is RD priority
        players = [
            Player(name=f"Active{i}", mmr=1500, glicko_rating=1500.0, glicko_rd=50.0)
            for i in range(10)
        ]
        high_rd_player = Player(name="HighRD", mmr=1500, glicko_rating=1500.0, glicko_rd=350.0)
        players.append(high_rd_player)

        # With high RD weight, the high-RD player should be included
        # RD difference: 350 vs 50 = 300 extra per player
        # With weight=1.0, that's 300 bonus for including high-RD player
        shuffler = BalancedShuffler(rd_priority_weight=1.0)
        team1, team2, excluded = shuffler.shuffle_from_pool(players)

        included_names = {p.name for p in team1.players + team2.players}
        excluded_names = {p.name for p in excluded}

        # High RD player should be included (not excluded)
        assert "HighRD" in included_names, "High RD player should be favored for inclusion"
        assert "HighRD" not in excluded_names

    def test_shuffle_wrong_number_of_players(self):
        """Test that shuffling fails with wrong number of players."""
        players = [Player(name=f"Player{i}", mmr=1500) for i in range(9)]
        shuffler = BalancedShuffler()
        with pytest.raises(ValueError):
            shuffler.shuffle(players)

    def test_shuffle_balanced_teams(self):
        """A 10-player shuffle assigns everyone to two teams of five and finds
        the optimal-value split."""
        # Create 10 players with varying MMRs
        players = [
            Player(name="P1", mmr=2000),
            Player(name="P2", mmr=1900),
            Player(name="P3", mmr=1800),
            Player(name="P4", mmr=1700),
            Player(name="P5", mmr=1600),
            Player(name="P6", mmr=1500),
            Player(name="P7", mmr=1400),
            Player(name="P8", mmr=1300),
            Player(name="P9", mmr=1200),
            Player(name="P10", mmr=1100),
        ]
        shuffler = BalancedShuffler(use_glicko=False, off_role_flat_penalty=50.0)
        team1, team2 = shuffler.shuffle(players)

        assert len(team1.players) == 5
        assert len(team2.players) == 5
        # Compare by name since Player objects aren't hashable
        assert {p.name for p in team1.players + team2.players} == {
            p.name for p in players
        }

        value1 = team1.get_team_value(use_glicko=False, off_role_multiplier=1.0)
        value2 = team2.get_team_value(use_glicko=False, off_role_multiplier=1.0)

        diff = abs(value1 - value2)
        # The optimal split of {2000,1900,...,1100} into two teams of five is a
        # 100-point gap (e.g. 7800 vs 7700); the next-best achievable split is
        # 300. A correct optimizer must find the optimum here, so any diff above
        # 100 means it degraded to a sub-optimal split. (The old <1000 bound
        # tolerated the 9th-worst split.) Deterministic: the shuffle has no role
        # preferences to randomize and the search is exhaustive at this size.
        assert diff <= 100, f"shuffle degraded: team diff {diff} > optimal 100"

    def test_shuffle_with_roles(self):
        """Test shuffling with role preferences."""
        players = [
            Player(name="P1", mmr=2000, preferred_roles=["1"]),
            Player(name="P2", mmr=1900, preferred_roles=["2"]),
            Player(name="P3", mmr=1800, preferred_roles=["3"]),
            Player(name="P4", mmr=1700, preferred_roles=["4"]),
            Player(name="P5", mmr=1600, preferred_roles=["5"]),
            Player(name="P6", mmr=1500, preferred_roles=["1"]),
            Player(name="P7", mmr=1400, preferred_roles=["2"]),
            Player(name="P8", mmr=1300, preferred_roles=["3"]),
            Player(name="P9", mmr=1200, preferred_roles=["4"]),
            Player(name="P10", mmr=1100, preferred_roles=["5"]),
        ]
        shuffler = BalancedShuffler(use_glicko=False, off_role_flat_penalty=50.0)
        team1, team2 = shuffler.shuffle(players)

        # Both teams should have role assignments
        assert team1.role_assignments is not None
        assert team2.role_assignments is not None
        assert len(team1.role_assignments) == 5
        assert len(team2.role_assignments) == 5

    def test_region_split_mode_separates_usw_and_use(self):
        """Region mode should prefer a clean US West vs US East split."""
        players = []
        all_roles = ["1", "2", "3", "4", "5"]
        for i in range(5):
            players.append(
                Player(
                    name=f"USW{i}",
                    mmr=1500,
                    preferred_roles=all_roles,
                    preferred_region="USW",
                )
            )
            players.append(
                Player(
                    name=f"USE{i}",
                    mmr=1500,
                    preferred_roles=all_roles,
                    preferred_region="USE",
                )
            )

        shuffler = BalancedShuffler(
            use_glicko=False,
            off_role_flat_penalty=0.0,
            role_matchup_delta_weight=0.0,
            rd_priority_weight=0.0,
            region_split=True,
            region_split_penalty=1000.0,
        )

        team1, team2 = shuffler.shuffle(players)

        assert region_split_mismatches(team1.players, team2.players) == 0
        team_region_sets = {
            frozenset(resolve_region(player) for player in team.players)
            for team in (team1, team2)
        }
        assert team_region_sets == {frozenset({"USW"}), frozenset({"USE"})}

    @pytest.mark.parametrize("pool_size", [12, 13])
    def test_shuffle_from_pool(self, pool_size):
        """Test shuffling from a pool of more than 10 players.

        Both an even and an odd oversized pool: 12 sits out 2, 13 sits out 3.
        """
        players = [
            Player(name=f"Player{i}", mmr=1500 + i * 10) for i in range(pool_size)
        ]
        shuffler = BalancedShuffler(use_glicko=False, off_role_flat_penalty=50.0)
        team1, team2, excluded = shuffler.shuffle_from_pool(players)

        assert len(team1.players) == 5
        assert len(team2.players) == 5
        assert len(excluded) == pool_size - 10
        # All players should be accounted for
        all_players = team1.players + team2.players + excluded
        assert len(all_players) == pool_size
        # Compare by name since Player objects aren't hashable
        all_player_names = {p.name for p in all_players}
        player_names = {p.name for p in players}
        assert all_player_names == player_names

    def test_large_pool_bounds_full_role_matchup_evaluations(self, monkeypatch):
        roles = [
            ["1"],
            ["2"],
            ["3"],
            ["4"],
            ["5"],
            ["1", "2"],
            ["3", "4"],
            ["4", "5"],
        ]
        players = [
            Player(
                name=f"Player{i}",
                discord_id=1000 + i,
                mmr=1500 + i * 40,
                glicko_rating=1500.0 + i * 20,
                glicko_rd=50.0 + i,
                preferred_roles=roles[i % len(roles)],
            )
            for i in range(20)
        ]
        shuffler = BalancedShuffler()
        original_evaluate = shuffler._evaluate_pool_matchup
        evaluations = 0

        def counted_evaluate(*args, **kwargs):
            nonlocal evaluations
            evaluations += 1
            return original_evaluate(*args, **kwargs)

        monkeypatch.setattr(shuffler, "_evaluate_pool_matchup", counted_evaluate)

        team1, team2, excluded = shuffler.shuffle_from_pool(
            players,
            rng=random.Random(42),
        )

        assert len(team1.players) == len(team2.players) == 5
        assert len(excluded) == 10
        assert evaluations <= 4000

    def test_pool_signature_only_built_for_numeric_improvements(self, monkeypatch):
        players = [
            Player(
                name=f"Player{i}",
                mmr=1500 + i,
                preferred_roles=["1", "2", "3", "4", "5"],
            )
            for i in range(11)
        ]
        shuffler = BalancedShuffler(use_glicko=False)
        numeric_prefixes = [(30.0, 3.0, 1), (40.0, 1.0, 0), (20.0, 10.0, 2)]
        evaluate_calls = 0
        expected_team1_names = None

        def evaluate(team1_players, team2_players, *args, **kwargs):
            nonlocal evaluate_calls, expected_team1_names
            if evaluate_calls < len(numeric_prefixes):
                total_score, value_diff, total_off_roles = numeric_prefixes[evaluate_calls]
            else:
                total_score, value_diff, total_off_roles = (50.0, 0.0, 0)

            matchup = self._mock_pool_matchup(
                team1_players,
                team2_players,
                (total_score, value_diff, total_off_roles),
            )
            if evaluate_calls == 2:
                expected_team1_names = tuple(
                    player.name for player in matchup.team1.players
                )
            evaluate_calls += 1
            return matchup

        original_signature = shuffler._pool_matchup_signature
        signature_calls = 0

        def counted_signature(*args, **kwargs):
            nonlocal signature_calls
            signature_calls += 1
            return original_signature(*args, **kwargs)

        monkeypatch.setattr(shuffler, "_evaluate_pool_matchup", evaluate)
        monkeypatch.setattr(shuffler, "_pool_matchup_signature", counted_signature)

        team1, _, _ = shuffler.shuffle_from_pool(players)

        assert signature_calls == 2
        assert tuple(player.name for player in team1.players) == expected_team1_names

    def test_pool_signature_built_for_exact_numeric_tie(self, monkeypatch):
        players = [
            Player(
                name=f"Player{i}",
                mmr=1500 + i,
                preferred_roles=["1", "2", "3", "4", "5"],
            )
            for i in range(11)
        ]
        shuffler = BalancedShuffler(use_glicko=False)
        evaluate_calls = 0
        expected_team1_names = None

        def evaluate(team1_players, team2_players, *args, **kwargs):
            nonlocal evaluate_calls, expected_team1_names
            if evaluate_calls < 2:
                total_score, value_diff, total_off_roles = (10.0, 2.0, 1)
            else:
                total_score, value_diff, total_off_roles = (20.0, 0.0, 0)

            matchup = self._mock_pool_matchup(
                team1_players,
                team2_players,
                (total_score, value_diff, total_off_roles),
            )
            if evaluate_calls == 1:
                expected_team1_names = tuple(
                    player.name for player in matchup.team1.players
                )
            evaluate_calls += 1
            return matchup

        signature_calls = 0

        def ordered_signature(*args, **kwargs):
            nonlocal signature_calls
            signature_calls += 1
            marker = "z" if signature_calls == 1 else "a"
            return ((marker,), (), ())

        monkeypatch.setattr(shuffler, "_evaluate_pool_matchup", evaluate)
        monkeypatch.setattr(shuffler, "_pool_matchup_signature", ordered_signature)

        team1, _, _ = shuffler.shuffle_from_pool(players)

        assert signature_calls == 2
        assert tuple(player.name for player in team1.players) == expected_team1_names

    def test_shuffle_from_pool_less_than_10(self):
        """Test that shuffle_from_pool fails with less than 10 players."""
        players = [Player(name=f"Player{i}", mmr=1500) for i in range(9)]
        shuffler = BalancedShuffler()
        with pytest.raises(ValueError):
            shuffler.shuffle_from_pool(players)

    def test_off_role_penalty_applied(self):
        """Off-role penalty steers selection away from off-role compositions."""
        # Putting both carries (900/1100) on one team gives a perfectly balanced
        # split (value diff 0) but forces 2 off-roles; separating the carries
        # costs a 200 value diff but plays everyone on-role. With a high
        # off-role penalty the shuffle must pick the on-role split.
        players = [
            Player(name="CarryA", mmr=900, preferred_roles=["1"]),
            Player(name="CarryB", mmr=1100, preferred_roles=["1"]),
        ]
        for i, role in enumerate(["2", "2", "3", "3", "4", "4", "5", "5"]):
            players.append(Player(name=f"Filler{i}", mmr=1000, preferred_roles=[role]))

        shuffler = BalancedShuffler(
            use_glicko=False,
            off_role_flat_penalty=1000.0,
            role_matchup_delta_weight=0.0,
        )
        team1, team2 = shuffler.shuffle(players)

        total_off_roles = team1.get_off_role_count() + team2.get_off_role_count()
        assert total_off_roles == 0, (
            "With a high off-role penalty, the shuffle must choose the on-role "
            "split even though an off-role split has a smaller rating difference"
        )

    def test_role_assignments_consider_matchup_delta(self):
        """Higher-MMR cores should land in mid when matchups tie on off-role count."""
        high_mid = Player(name="HighMid", mmr=1791, preferred_roles=["2", "1", "5"])
        flex_mid = Player(name="FlexMid", mmr=1409, preferred_roles=["1", "2", "3", "4", "5"])
        team1_players = [
            high_mid,
            flex_mid,
            Player(name="Offlane", mmr=1560, preferred_roles=["3"]),
            Player(name="Soft", mmr=1464, preferred_roles=["4"]),
            Player(name="Hard", mmr=1791, preferred_roles=["5"]),
        ]

        team2_players = [
            Player(name="DireCarry", mmr=1837, preferred_roles=["1"]),
            Player(name="DireMid", mmr=1973, preferred_roles=["2"]),
            Player(name="DireOfflane", mmr=1462, preferred_roles=["3"]),
            Player(name="DireSoft", mmr=1234, preferred_roles=["4"]),
            Player(name="DireHard", mmr=1070, preferred_roles=["5"]),
        ]

        team1 = Team(team1_players)
        team2 = Team(team2_players)
        service = TeamBalancingService(use_glicko=False, off_role_multiplier=1.0)

        best_score = float("inf")
        best_team1_roles = None

        team1_assignments = team1.get_all_optimal_role_assignments()
        team2_assignments = team2.get_all_optimal_role_assignments()

        for t1_roles in team1_assignments:
            for t2_roles in team2_assignments:
                team1_assigned = Team(team1_players, role_assignments=t1_roles)
                team2_assigned = Team(team2_players, role_assignments=t2_roles)
                score = service.calculate_matchup_score(team1_assigned, team2_assigned)

                if score < best_score:
                    best_score = score
                    best_team1_roles = t1_roles

        high_mid_index = team1_players.index(high_mid)
        flex_mid_index = team1_players.index(flex_mid)

        assert best_team1_roles is not None
        assert best_team1_roles[high_mid_index] == "2"
        assert best_team1_roles[flex_mid_index] != "2"

    def test_shuffle_from_pool_with_exclusion_counts(self):
        """Exclusion counts influence selection: the frequently-excluded players
        are protected and the never-excluded player sits out."""
        # 11 identical players; only exclusion counts differ, so the exclusion
        # penalty is the sole discriminator between candidate exclusions.
        players = [
            Player(name=f"Player{i}", mmr=1500, preferred_roles=["1", "2", "3", "4", "5"])
            for i in range(11)
        ]
        exclusion_counts = {p.name: 5 for p in players}
        exclusion_counts["Player0"] = 0  # never sat out -> should be excluded

        shuffler = BalancedShuffler(use_glicko=False, exclusion_penalty_weight=500.0)

        team1, team2, excluded = shuffler.shuffle_from_pool(players, exclusion_counts)

        assert len(team1.players) == 5
        assert len(team2.players) == 5

        # Excluding any protected (count=5) player costs 5 * exclusion_penalty_weight
        # more than excluding Player0, so Player0 must be the one sitting out.
        assert [p.name for p in excluded] == ["Player0"], (
            "The player with the lowest exclusion count should be excluded"
        )

    def test_greedy_shuffle_excludes_lowest_effective_exclusion_count(self):
        """Regression: the greedy path must exclude the players with the
        LOWEST effective exclusion counts, not simply the lowest-rated ones."""
        players = []
        exclusion_counts = {}
        for i in range(12):
            player = Player(
                name=f"Player{i}", mmr=1000 + i * 200, preferred_roles=[str(i % 5 + 1)]
            )
            players.append(player)
            exclusion_counts[player.name] = 5  # sat out often -> protected

        # Two mid/high-rated players have never sat out (count 0); the greedy
        # path should exclude them instead of the lowest-rated players.
        exclusion_counts["Player8"] = 0  # mmr 2600
        exclusion_counts["Player9"] = 0  # mmr 2800

        shuffler = BalancedShuffler(
            use_glicko=False, off_role_flat_penalty=50.0, exclusion_penalty_weight=5.0
        )
        team1, team2, excluded, _score = shuffler._greedy_shuffle(players, exclusion_counts)

        assert {p.name for p in excluded} == {"Player8", "Player9"}
        assert len(team1.players) == 5
        assert len(team2.players) == 5

    def test_zero_exclusion_penalty_weight_with_recent_participants(self):
        """Regression: exclusion_penalty_weight=0 disables exclusion weighting,
        so the >10-player greedy path must give recent participants NO reduction
        rather than dividing by the zero weight. Before the divisor guard this
        raised ZeroDivisionError for any >10 pool with recent participants."""
        players = []
        exclusion_counts = {}
        for i in range(12):
            player = Player(
                name=f"Player{i}", mmr=1000 + i * 200, preferred_roles=[str(i % 5 + 1)]
            )
            players.append(player)
            exclusion_counts[player.name] = i

        shuffler = BalancedShuffler(
            use_glicko=False, off_role_flat_penalty=50.0, exclusion_penalty_weight=0.0
        )
        # Some of the pool participated in the most recent match — the exact
        # input that drove the old code into the zero divisor.
        recent = {"Player0", "Player3", "Player7"}

        team1, team2, excluded, _score = shuffler._greedy_shuffle(
            players, exclusion_counts, recent_match_names=recent
        )

        assert len(team1.players) == 5
        assert len(team2.players) == 5
        assert len(excluded) == 2


class TestRoleMatchupDelta:
    """Tests for the role matchup delta scoring additions."""

    def test_default_role_matchup_delta_weight_is_point_seventeen(self):
        """Lane and role parity should use the requested shared default weight."""
        assert BalancedShuffler().role_matchup_delta_weight == pytest.approx(0.17)

    def test_role_matchup_delta_calculation(self):
        """Role matchup delta should return the sum of the critical matchups,
        and role_matchup_delta_weight should scale both parity deltas when
        computing matchup scores."""
        team1_players = [
            Player(name="Carry1", mmr=2000, preferred_roles=["1"]),
            Player(name="Mid1", mmr=1500, preferred_roles=["2"]),
            Player(name="Offlane1", mmr=1200, preferred_roles=["3"]),
            Player(name="Sup1", mmr=1100, preferred_roles=["4"]),
            Player(name="Sup2", mmr=1000, preferred_roles=["5"]),
        ]
        team2_players = [
            Player(name="Carry2", mmr=1000, preferred_roles=["1"]),
            Player(name="Mid2", mmr=1500, preferred_roles=["2"]),
            Player(name="Offlane2", mmr=1900, preferred_roles=["3"]),
            Player(name="Sup3", mmr=1050, preferred_roles=["4"]),
            Player(name="Sup4", mmr=950, preferred_roles=["5"]),
        ]

        team1 = Team(team1_players, role_assignments=["1", "2", "3", "4", "5"])
        team2 = Team(team2_players, role_assignments=["1", "2", "3", "4", "5"])

        service = TeamBalancingService(use_glicko=False, off_role_multiplier=1.0)
        delta = service.calculate_role_matchup_delta(team1, team2)

        # carry1 vs offlane2 = |2000 - 1900| = 100
        # carry2 vs offlane1 = |1000 - 1200| = 200
        # mid vs mid = |1500 - 1500| = 0
        # pos4 cross = |1100 - 950| = 150
        # pos4 cross = |1050 - 1000| = 50
        # sum = 100 + 200 + 0 + 150 + 50 = 500
        assert delta == 500

        role_parity_delta = service.calculate_role_parity_delta(team1, team2)
        # same roles = |2000-1000| + |1500-1500| + |1200-1900|
        #            + |1100-1050| + |1000-950| = 1800
        assert role_parity_delta == 1800

        score = service.calculate_matchup_score(team1, team2)
        # value difference = |6800 - 6400| = 400
        # off-role penalty = 0
        # lane delta = 500; same-role parity delta = 1800
        # total should therefore be 400 + 500 + 1800 = 2700
        assert score == pytest.approx(2700)

        weighted_service = TeamBalancingService(
            use_glicko=False, off_role_multiplier=1.0, role_matchup_delta_weight=0.5
        )
        assert weighted_service.calculate_role_matchup_delta(team1, team2) == 500
        weighted_score = weighted_service.calculate_matchup_score(team1, team2)
        # value difference = 400; weighted lane and role deltas = (500 + 1800) * 0.5
        assert weighted_score == pytest.approx(1550)

    def test_matchup_score_uses_requested_jopacoin_values(self):
        """Every matchup-score component should use the requested rating mode."""
        roles = ["1", "2", "3", "4", "5"]
        team1 = Team(
            [
                Player(
                    name=f"Team1Role{role}",
                    mmr=1500,
                    preferred_roles=[role],
                    jopacoin_balance=100,
                )
                for role in roles
            ],
            role_assignments=roles,
        )
        team2 = Team(
            [
                Player(
                    name=f"Team2Role{role}",
                    mmr=1500,
                    preferred_roles=[role],
                    jopacoin_balance=50 if role == "1" else 100,
                )
                for role in roles
            ],
            role_assignments=roles,
        )
        service = TeamBalancingService(
            use_glicko=False,
            off_role_multiplier=1.0,
            off_role_flat_penalty=0.0,
            role_matchup_delta_weight=0.5,
        )

        assert service.calculate_matchup_score(team1, team2) == pytest.approx(0)
        # Team-value difference = 50. Lane delta = 50. Role parity = 50.
        assert service.calculate_matchup_score(
            team1, team2, use_jopacoin=True
        ) == pytest.approx(100)

    def test_role_matchup_delta_weight_applied_in_shuffler_scoring(self):
        """BalancedShuffler should apply the weight when scoring matchups."""
        # Constrain roles to avoid off-role permutations.
        team1_players = [
            Player(name="RadiantCarry", mmr=2000, preferred_roles=["1"]),
            Player(name="RadiantMid", mmr=1500, preferred_roles=["2"]),
            Player(name="RadiantOfflane", mmr=1000, preferred_roles=["3"]),
            Player(name="RadiantSoft", mmr=1000, preferred_roles=["4"]),
            Player(name="RadiantHard", mmr=1000, preferred_roles=["5"]),
        ]
        team2_players = [
            Player(name="DireCarry", mmr=1400, preferred_roles=["1"]),
            Player(name="DireMid", mmr=1500, preferred_roles=["2"]),
            Player(name="DireOfflane", mmr=1900, preferred_roles=["3"]),
            Player(name="DireSoft", mmr=1000, preferred_roles=["4"]),
            Player(name="DireHard", mmr=1000, preferred_roles=["5"]),
        ]

        # With fixed roles, there is exactly one assignment per team.
        shuffler_full_weight = BalancedShuffler(
            use_glicko=False,
            consider_roles=True,
            off_role_multiplier=1.0,
            off_role_flat_penalty=0.0,
            role_matchup_delta_weight=1.0,
        )
        _, _, score_full = shuffler_full_weight._optimize_role_assignments_for_matchup(
            team1_players, team2_players, max_assignments_per_team=1
        )

        shuffler_half_weight = BalancedShuffler(
            use_glicko=False,
            consider_roles=True,
            off_role_multiplier=1.0,
            off_role_flat_penalty=0.0,
            role_matchup_delta_weight=0.5,
        )
        _, _, score_half = shuffler_half_weight._optimize_role_assignments_for_matchup(
            team1_players, team2_players, max_assignments_per_team=1
        )

        # value diff = |6500 - 6800| = 300
        # lane delta = sum(|2000-1900|, |1400-1000|, |1500-1500|, |1000-1000|, |1000-1000|)
        #            = 100 + 400 + 0 + 0 + 0 = 500
        # role parity = |2000-1400| + |1500-1500| + |1000-1900|
        #             + |1000-1000| + |1000-1000| = 1500
        assert score_full == pytest.approx(2300)  # 300 + 500 + 1500
        assert score_half == pytest.approx(1300)  # 300 + ((500 + 1500) * 0.5)

    def test_pool_log_entry_uses_weighted_parity_penalty(self):
        """Logged parity should be the contribution included in total score."""
        team1_players = [
            Player(name="RadiantCarry", mmr=2000, preferred_roles=["1"]),
            Player(name="RadiantMid", mmr=1500, preferred_roles=["2"]),
            Player(name="RadiantOfflane", mmr=1000, preferred_roles=["3"]),
            Player(name="RadiantSoft", mmr=1000, preferred_roles=["4"]),
            Player(name="RadiantHard", mmr=1000, preferred_roles=["5"]),
        ]
        team2_players = [
            Player(name="DireCarry", mmr=1400, preferred_roles=["1"]),
            Player(name="DireMid", mmr=1300, preferred_roles=["2"]),
            Player(name="DireOfflane", mmr=1900, preferred_roles=["3"]),
            Player(name="DireSoft", mmr=1000, preferred_roles=["4"]),
            Player(name="DireHard", mmr=1000, preferred_roles=["5"]),
        ]
        shuffler = BalancedShuffler(
            use_glicko=False,
            off_role_multiplier=1.0,
            off_role_flat_penalty=0.0,
            role_matchup_delta_weight=0.5,
        )

        matchup = shuffler._evaluate_pool_matchup(
            team1_players,
            team2_players,
            combo_penalty=0.0,
            exclusion_penalty=0.0,
            excluded_names=[],
            recent_penalty=0.0,
            rd_priority=0.0,
            max_assignments_per_team=1,
            avoids=None,
            deals=None,
        )

        # Value diff = 100. Lane delta = 700 and role parity = 1700;
        # the nonzero mid gap contributes 200 to each parity factor.
        assert matchup.log_entry[3] == pytest.approx(1200)
        assert matchup.total_score == pytest.approx(1300)

    def test_support_cross_lane_delta_in_shuffler(self):
        """BalancedShuffler should include pos4 vs pos5 cross-lane deltas."""
        team1_players = [
            Player(name="RadiantCarry", mmr=1500, preferred_roles=["1"]),
            Player(name="RadiantMid", mmr=1500, preferred_roles=["2"]),
            Player(name="RadiantOfflane", mmr=1500, preferred_roles=["3"]),
            Player(name="RadiantSoft", mmr=1300, preferred_roles=["4"]),
            Player(name="RadiantHard", mmr=900, preferred_roles=["5"]),
        ]
        team2_players = [
            Player(name="DireCarry", mmr=1500, preferred_roles=["1"]),
            Player(name="DireMid", mmr=1500, preferred_roles=["2"]),
            Player(name="DireOfflane", mmr=1500, preferred_roles=["3"]),
            Player(name="DireSoft", mmr=1100, preferred_roles=["4"]),
            Player(name="DireHard", mmr=1200, preferred_roles=["5"]),
        ]

        team1 = Team(team1_players, role_assignments=["1", "2", "3", "4", "5"])
        team2 = Team(team2_players, role_assignments=["1", "2", "3", "4", "5"])

        shuffler = BalancedShuffler(
            use_glicko=False,
            consider_roles=True,
            off_role_multiplier=1.0,
            off_role_flat_penalty=0.0,
            role_matchup_delta_weight=1.0,
        )
        delta = shuffler._role_matchup_delta_from_metrics(
            shuffler._assigned_role_metrics(team1),
            shuffler._assigned_role_metrics(team2),
        )

        # carry/offlane/mid all equal → 0
        # pos4 cross 1: |1300 - 1200| = 100
        # pos4 cross 2: |1100 - 900| = 200
        # total = 0 + 0 + 0 + 100 + 200 = 300
        assert delta == 300


class TestRoleDeltaFormulaConsistency:
    """Characterization tests pinning the lane-matchup/parity formulas.

    The canonical formulas live in domain/services/team_balancing_service.py.
    The shuffler's metrics helpers reuse them, while the two inlined hot loops
    (_score_unconstrained_role_assignments and the constrained fallback in
    _score_role_assignments_for_matchup) keep manual copies for speed. These
    tests pin every copy to the same numbers so a future scoring tweak that
    misses one copy fails here instead of silently diverging.
    """

    @staticmethod
    def _teams() -> tuple[Team, Team]:
        team1_players = [
            Player(name="Carry1", mmr=2000, preferred_roles=["1"], discord_id=1),
            Player(name="Mid1", mmr=1500, preferred_roles=["2"], discord_id=2),
            Player(name="Offlane1", mmr=1200, preferred_roles=["3"], discord_id=3),
            Player(name="Sup1", mmr=1100, preferred_roles=["4"], discord_id=4),
            Player(name="Sup2", mmr=1000, preferred_roles=["5"], discord_id=5),
        ]
        team2_players = [
            Player(name="Carry2", mmr=1000, preferred_roles=["1"], discord_id=6),
            Player(name="Mid2", mmr=1500, preferred_roles=["2"], discord_id=7),
            Player(name="Offlane2", mmr=1900, preferred_roles=["3"], discord_id=8),
            Player(name="Sup3", mmr=1050, preferred_roles=["4"], discord_id=9),
            Player(name="Sup4", mmr=950, preferred_roles=["5"], discord_id=10),
        ]
        roles = ["1", "2", "3", "4", "5"]
        return (
            Team(team1_players, role_assignments=roles),
            Team(team2_players, role_assignments=roles),
        )

    # Pinned by hand from the fixture above:
    # matchup = |2000-1900| + |1000-1200| + |1500-1500| + |1100-950| + |1050-1000|
    #         = 100 + 200 + 0 + 150 + 50 = 500
    # parity  = |2000-1000| + |1500-1500| + |1200-1900| + |1100-1050| + |1000-950|
    #         = 1000 + 0 + 700 + 50 + 50 = 1800
    EXPECTED_MATCHUP = 500
    EXPECTED_PARITY = 1800

    def test_service_and_shuffler_metrics_helpers_agree(self):
        """Service methods and shuffler metrics helpers pin the same numbers."""
        team1, team2 = self._teams()
        service = TeamBalancingService(use_glicko=False, off_role_multiplier=1.0)
        shuffler = BalancedShuffler(
            use_glicko=False, consider_roles=True, off_role_multiplier=1.0
        )
        metrics1 = shuffler._assigned_role_metrics(team1)
        metrics2 = shuffler._assigned_role_metrics(team2)

        assert service.calculate_role_matchup_delta(team1, team2) == self.EXPECTED_MATCHUP
        assert service.calculate_role_parity_delta(team1, team2) == self.EXPECTED_PARITY
        assert (
            shuffler._role_matchup_delta_from_metrics(metrics1, metrics2)
            == self.EXPECTED_MATCHUP
        )
        assert (
            shuffler._role_parity_delta_from_metrics(metrics1, metrics2)
            == self.EXPECTED_PARITY
        )

    def test_inlined_hot_loops_match_shared_formulas(self):
        """Both inlined scoring loops must reproduce the shared formulas."""
        team1, team2 = self._teams()
        shuffler = BalancedShuffler(
            use_glicko=False,
            consider_roles=True,
            off_role_multiplier=1.0,
            off_role_flat_penalty=0.0,
            role_matchup_delta_weight=0.5,
            rd_priority_weight=0.0,
        )
        # value diff = |6800 - 6400| = 400; weighted deltas = (500 + 1800) * 0.5
        expected_score = 400 + (self.EXPECTED_MATCHUP + self.EXPECTED_PARITY) * 0.5

        # Unconstrained hot loop (_score_unconstrained_role_assignments).
        _, _, unconstrained_score = shuffler._score_role_assignments_for_matchup(
            team1.players, team2.players, max_assignments_per_team=1
        )
        assert unconstrained_score == pytest.approx(expected_score)

        # Constrained fallback loop: a cross-team avoid adds zero penalty but
        # forces the avoid/deal scoring path, whose arithmetic must agree.
        inert_avoid = SimpleNamespace(avoider_discord_id=1, avoided_discord_id=6)
        _, _, constrained_score = shuffler._score_role_assignments_for_matchup(
            team1.players,
            team2.players,
            max_assignments_per_team=1,
            avoids=[inert_avoid],
        )
        assert constrained_score == pytest.approx(expected_score)


def _create_players_with_roles(count: int, base_mmr: int = 1500, spread: int = 50) -> list[Player]:
    """Create test players with realistic role preferences for efficient B&B pruning."""
    roles_cycle = [["1"], ["2"], ["3"], ["4"], ["5"], ["1", "2"], ["3", "4"], ["4", "5"]]
    return [
        Player(
            name=f"Player{i}",
            mmr=base_mmr + i * spread,
            glicko_rating=float(base_mmr + i * spread // 2),
            preferred_roles=roles_cycle[i % len(roles_cycle)],
        )
        for i in range(count)
    ]


def _shuffle_result_signature(
    result: tuple[Team, Team, list[Player]],
    *,
    identity_only: bool = False,
) -> tuple:
    """Canonicalize teams, roles, and exclusions for differential checks."""

    def player_key(player: Player) -> tuple:
        if identity_only:
            return (player.discord_id,)
        return (player.name, player.discord_id)

    def team_signature(team: Team) -> tuple:
        return tuple(
            sorted(
                (
                    player_key(player),
                    role,
                )
                for player, role in zip(
                    team.players,
                    team.ensure_role_assignments(),
                    strict=True,
                )
            )
        )

    team1, team2, excluded = result
    teams = sorted((team_signature(team1), team_signature(team2)))
    return (
        teams[0],
        teams[1],
        tuple(sorted(player_key(player) for player in excluded)),
    )


def _seeded_14_player_pool(
    seed: int, prefix: str = "Seeded", max_roles: int = 3
) -> list[Player]:
    # Role preferences default to 1-3 per player, which still mixes off-role
    # and flex assignments while keeping the exhaustive reference search
    # cheap. Callers pass max_roles=5 to get full-flex players, the regime
    # with the largest role-assignment candidate sets (and so the most room
    # for the split bound to prune wrongly); at least one differential case
    # must exercise it.
    rng = random.Random(seed)
    roles = ["1", "2", "3", "4", "5"]
    return [
        Player(
            name=f"{prefix}{index}",
            discord_id=index + 1,
            glicko_rating=rng.uniform(850.0, 2750.0),
            glicko_rd=None if index % 5 == 0 else rng.uniform(20.0, 350.0),
            preferred_roles=rng.sample(roles, rng.randint(1, max_roles)),
        )
        for index in range(14)
    ]


def _give_shuffler_loose_upper_bound(
    monkeypatch: pytest.MonkeyPatch, shuffler: BalancedShuffler
) -> None:
    """Force search beyond the greedy result while retaining its valid teams."""
    original_greedy = shuffler._greedy_shuffle

    def loose_greedy(*args, **kwargs):
        team1, team2, excluded, _score = original_greedy(*args, **kwargs)
        return team1, team2, excluded, float("inf")

    monkeypatch.setattr(shuffler, "_greedy_shuffle", loose_greedy)


def _disable_split_bound(
    monkeypatch: pytest.MonkeyPatch, shuffler: BalancedShuffler
) -> None:
    """Retain the pre-optimization search path as a differential oracle."""
    original_scorer = shuffler._score_role_assignments_for_matchup

    def wrapped_scorer(*args, **kwargs):
        return original_scorer(*args, **kwargs)

    monkeypatch.setattr(
        shuffler, "_score_role_assignments_for_matchup", wrapped_scorer
    )


def _assert_split_bound_matches_reference(
    monkeypatch: pytest.MonkeyPatch,
    players: list[Player],
    settings: dict,
    *,
    exclusion_counts: dict[str, int] | None = None,
    recent_match_names: set[str] | None = None,
    avoids: list | None = None,
    deals: list | None = None,
) -> None:
    reference = BalancedShuffler(**settings)
    optimized = BalancedShuffler(**settings)
    for shuffler in (reference, optimized):
        _give_shuffler_loose_upper_bound(monkeypatch, shuffler)
    _disable_split_bound(monkeypatch, reference)
    kwargs = {
        "exclusion_counts": exclusion_counts,
        "recent_match_names": recent_match_names,
        "avoids": avoids,
        "deals": deals,
    }
    expected = reference.shuffle_branch_bound(players, **kwargs)
    actual = optimized.shuffle_branch_bound(players, **kwargs)
    assert _shuffle_result_signature(actual) == _shuffle_result_signature(
        expected
    )


class TestRoleAssignmentCommonPath:
    def test_common_path_skips_constraint_penalty_work(self, monkeypatch):
        players = _create_players_with_roles(10)
        shuffler = BalancedShuffler(use_glicko=True)

        def unexpected(*_args, **_kwargs):
            raise AssertionError("constraint penalty ran on the common path")

        monkeypatch.setattr(shuffler, "calculate_soft_avoid_penalty", unexpected)
        monkeypatch.setattr(shuffler, "calculate_package_deal_penalty", unexpected)
        monkeypatch.setattr(shuffler, "_calculate_region_split_penalty", unexpected)

        team1_roles, team2_roles, score = shuffler._score_role_assignments_for_matchup(
            players[:5], players[5:], max_assignments_per_team=5
        )

        assert team1_roles is not None
        assert team2_roles is not None
        assert math.isfinite(score)

    @pytest.mark.parametrize("constraint", ["avoid", "deal", "region"])
    def test_configured_constraints_retain_general_fallback(self, constraint, monkeypatch):
        players = _create_players_with_roles(10)
        for discord_id, player in enumerate(players, start=1):
            player.discord_id = discord_id
        shuffler = BalancedShuffler(
            use_glicko=True,
            region_split=constraint == "region",
        )
        avoids = None
        deals = None
        if constraint == "avoid":
            avoids = [
                SimpleNamespace(
                    avoider_discord_id=players[0].discord_id,
                    avoided_discord_id=players[5].discord_id,
                )
            ]
        elif constraint == "deal":
            deals = [
                SimpleNamespace(
                    buyer_discord_id=players[0].discord_id,
                    partner_discord_id=players[1].discord_id,
                )
            ]

        def unexpected(*_args, **_kwargs):
            raise AssertionError("constrained scoring used the common path")

        monkeypatch.setattr(shuffler, "_score_unconstrained_role_assignments", unexpected)

        roles1, roles2, score = shuffler._score_role_assignments_for_matchup(
            players[:5],
            players[5:],
            max_assignments_per_team=3,
            avoids=avoids,
            deals=deals,
        )

        assert roles1 is not None
        assert roles2 is not None
        assert math.isfinite(score)

    def test_seeded_randomized_common_path_exactly_matches_zero_penalty_fallback(
        self,
    ):
        rng = random.Random(0xCADA)
        role_pool = ["1", "2", "3", "4", "5"]

        for case in range(60):
            players = []
            for index in range(10):
                role_count = rng.randint(1, len(role_pool))
                players.append(
                    Player(
                        name=f"Case{case}Player{index}",
                        discord_id=case * 100 + index + 1,
                        glicko_rating=rng.uniform(900.0, 2600.0),
                        glicko_rd=rng.uniform(30.0, 350.0),
                        preferred_roles=rng.sample(role_pool, role_count),
                    )
                )

            shuffler = BalancedShuffler(
                off_role_multiplier=rng.uniform(0.75, 1.0),
                off_role_flat_penalty=rng.uniform(0.0, 250.0),
                role_matchup_delta_weight=rng.uniform(0.0, 1.5),
                rd_priority_weight=rng.uniform(0.0, 0.25),
            )
            assignment_limit = rng.randint(1, 20)

            common = shuffler._score_role_assignments_for_matchup(
                players[:5],
                players[5:],
                max_assignments_per_team=assignment_limit,
            )
            # This cross-team avoid forces the general path but contributes
            # exactly zero, making it an output-equivalent oracle.
            fallback = shuffler._score_role_assignments_for_matchup(
                players[:5],
                players[5:],
                max_assignments_per_team=assignment_limit,
                avoids=[
                    SimpleNamespace(
                        avoider_discord_id=players[0].discord_id,
                        avoided_discord_id=players[5].discord_id,
                    )
                ],
            )

            assert common == fallback


class TestShuffler14Players:
    """Tests for 14-player pool shuffling (new max lobby size)."""

    def test_split_bound_indexes_each_team_summary_once(
        self, monkeypatch
    ):
        """Recurring five-player teams should bypass repeated summary lookups."""
        players = _seeded_14_player_pool(0x14CA, "Mask")
        shuffler = BalancedShuffler()
        _give_shuffler_loose_upper_bound(monkeypatch, shuffler)
        original_summary = shuffler._team_role_metrics_summary
        summarized_teams: list[tuple[int | None, ...]] = []

        def counted_summary(team_players, *args, **kwargs):
            summarized_teams.append(
                tuple(player.discord_id for player in team_players)
            )
            return original_summary(team_players, *args, **kwargs)

        monkeypatch.setattr(
            shuffler, "_team_role_metrics_summary", counted_summary
        )

        shuffler.shuffle_branch_bound(players)

        assert summarized_teams
        assert len(summarized_teams) == len(set(summarized_teams))
        assert len(summarized_teams) <= math.comb(14, 5)

    @pytest.mark.parametrize(
        ("seed", "recent_count", "max_roles"),
        [
            (0x1401, 0, 5),
            (0x1403, 7, 3),
        ],
    )
    def test_split_bound_exactly_matches_reference_search_for_seeded_pools(
        self, monkeypatch, seed, recent_count, max_roles
    ):
        """Tighter split pruning must not change any selected player or role.

        Two seeded cases suffice: every penalty weight is randomized nonzero in
        both, and the pair covers recent_count == 0 (recent term inert) and
        recent_count > 0 (recent term active); the dropped seeds sampled the
        same code paths again. The first case uses full-flex (1-5 role)
        players so the widest role-assignment candidate sets — where the
        bound has the most to prune — stay covered.
        """
        players = _seeded_14_player_pool(seed, max_roles=max_roles)
        rng = random.Random(seed ^ 0xB0A0D)
        exclusion_counts = {
            player.name: rng.randint(0, 5) for player in players
        }
        recent_names = {
            player.name for player in rng.sample(players, recent_count)
        }
        settings = {
            "off_role_multiplier": rng.uniform(0.55, 1.0),
            "off_role_flat_penalty": rng.uniform(0.0, 220.0),
            "role_matchup_delta_weight": rng.uniform(0.0, 1.25),
            "exclusion_penalty_weight": rng.uniform(0.0, 100.0),
            "rd_priority_weight": rng.uniform(0.0, 0.3),
            "recent_match_penalty_weight": rng.uniform(0.0, 180.0),
            "rating_spread_divisor": rng.uniform(5.0, 40.0),
        }
        _assert_split_bound_matches_reference(
            monkeypatch,
            players,
            settings,
            exclusion_counts=exclusion_counts,
            recent_match_names=recent_names,
        )

    def test_duplicate_display_names_do_not_change_branch_bound_result(
        self, monkeypatch
    ):
        """Selection bounds must cache ratings by identity, not display name."""
        unique_players = _seeded_14_player_pool(0xD00D, "Identity")
        duplicate_players = _seeded_14_player_pool(0xD00D, "Identity")
        duplicate_players[-1].name = duplicate_players[0].name
        unique_shuffler = BalancedShuffler()
        duplicate_shuffler = BalancedShuffler()
        _give_shuffler_loose_upper_bound(monkeypatch, unique_shuffler)
        _give_shuffler_loose_upper_bound(monkeypatch, duplicate_shuffler)

        unique_result = unique_shuffler.shuffle_branch_bound(unique_players)
        duplicate_result = duplicate_shuffler.shuffle_branch_bound(
            duplicate_players
        )

        assert _shuffle_result_signature(
            duplicate_result, identity_only=True
        ) == _shuffle_result_signature(unique_result, identity_only=True)

    def test_split_bound_materially_prunes_zero_width_rating_intervals(
        self, monkeypatch
    ):
        """Fixed team values should skip scorer work that cannot beat the best."""
        players = [
            Player(
                name=f"Fixed{index}",
                discord_id=index + 1,
                glicko_rating=950.0 + index * 113.0 + index**2,
                glicko_rd=0.0,
                preferred_roles=["1", "2", "3", "4", "5"],
            )
            for index in range(14)
        ]
        settings = {
            "off_role_multiplier": 1.0,
            "off_role_flat_penalty": 0.0,
            "role_matchup_delta_weight": 0.0,
            "exclusion_penalty_weight": 0.0,
            "rd_priority_weight": 0.0,
            "recent_match_penalty_weight": 0.0,
            "rating_spread_divisor": 1_000_000.0,
        }
        reference_shuffler = BalancedShuffler(**settings)
        optimized_shuffler = BalancedShuffler(**settings)
        _give_shuffler_loose_upper_bound(monkeypatch, reference_shuffler)
        _give_shuffler_loose_upper_bound(monkeypatch, optimized_shuffler)
        _disable_split_bound(monkeypatch, reference_shuffler)

        scorer_calls = {
            id(reference_shuffler): 0,
            id(optimized_shuffler): 0,
        }
        original_scorer = (
            BalancedShuffler._score_unconstrained_role_assignments
        )

        def counted_scorer(shuffler, *args, **kwargs):
            scorer_calls[id(shuffler)] += 1
            return original_scorer(shuffler, *args, **kwargs)

        # Patch at class scope: the optimized method remains the class's
        # canonical scorer, so its safety guard stays enabled.
        monkeypatch.setattr(
            BalancedShuffler,
            "_score_unconstrained_role_assignments",
            counted_scorer,
        )

        expected = reference_shuffler.shuffle_branch_bound(players)
        actual = optimized_shuffler.shuffle_branch_bound(players)

        assert _shuffle_result_signature(actual) == _shuffle_result_signature(
            expected
        )
        reference_calls = scorer_calls[id(reference_shuffler)]
        optimized_calls = scorer_calls[id(optimized_shuffler)]
        assert optimized_calls * 2 < reference_calls, (
            f"split bound only reduced scorer calls from "
            f"{reference_calls} to {optimized_calls}"
        )

    @pytest.mark.parametrize(
        "negative_term",
        ["recent", "role", "avoid", "deal"],
    )
    def test_negative_penalty_terms_match_reference_search(
        self, monkeypatch, negative_term
    ):
        """Coarse bounds must fall back when an omitted term can be negative."""
        players = _seeded_14_player_pool(0x14CE, "Negative")
        settings = {
            "off_role_multiplier": 0.75,
            "off_role_flat_penalty": 110.0,
            "role_matchup_delta_weight": (
                -0.6 if negative_term == "role" else 0.7
            ),
            "recent_match_penalty_weight": (
                -450.0 if negative_term == "recent" else 125.0
            ),
            "soft_avoid_penalty": (
                -900.0 if negative_term == "avoid" else 500.0
            ),
            "package_deal_penalty": (
                -900.0 if negative_term == "deal" else 100.0
            ),
            "rd_priority_weight": 0.09,
            "rating_spread_divisor": 20.0,
        }
        recent_names = (
            {player.name for player in players[:8]}
            if negative_term == "recent"
            else set()
        )
        avoids = (
            [
                SimpleNamespace(
                    avoider_discord_id=players[0].discord_id,
                    avoided_discord_id=players[1].discord_id,
                )
            ]
            if negative_term == "avoid"
            else None
        )
        deals = (
            [
                SimpleNamespace(
                    buyer_discord_id=players[2].discord_id,
                    partner_discord_id=players[11].discord_id,
                )
            ]
            if negative_term == "deal"
            else None
        )
        _assert_split_bound_matches_reference(
            monkeypatch,
            players,
            settings,
            recent_match_names=recent_names,
            avoids=avoids,
            deals=deals,
        )

    def test_split_bound_preserves_empty_assignment_fallback(
        self, monkeypatch
    ):
        """An empty assignment interval must retain the valid greedy fallback."""
        players = _create_players_with_roles(14)
        reference_shuffler = BalancedShuffler()
        optimized_shuffler = BalancedShuffler()
        monkeypatch.setattr(
            reference_shuffler,
            "_get_cached_role_assignments",
            lambda _players: (),
        )
        monkeypatch.setattr(
            optimized_shuffler,
            "_get_cached_role_assignments",
            lambda _players: (),
        )
        _disable_split_bound(monkeypatch, reference_shuffler)

        expected = reference_shuffler.shuffle_branch_bound(players)
        actual = optimized_shuffler.shuffle_branch_bound(players)

        assert _shuffle_result_signature(actual) == _shuffle_result_signature(
            expected
        )
        for team in actual[:2]:
            assert set(team.ensure_role_assignments()) == {
                "1",
                "2",
                "3",
                "4",
                "5",
            }

    def test_branch_bound_does_not_prune_negative_rd_score(self, monkeypatch):
        players = [
            Player(
                name=f"Player{i}",
                mmr=1500,
                glicko_rd=25,
                preferred_roles=["1", "2", "3", "4", "5"],
            )
            for i in range(14)
        ]
        shuffler = BalancedShuffler(use_glicko=False)
        greedy_result = (
            Team(players[:5], role_assignments=["1", "2", "3", "4", "5"]),
            Team(players[5:10], role_assignments=["1", "2", "3", "4", "5"]),
            players[10:],
            -100.0,
        )
        monkeypatch.setattr(shuffler, "_greedy_shuffle", lambda *args, **kwargs: greedy_result)

        optimized_calls = 0

        def score_roles(team1_players, team2_players, **kwargs):
            nonlocal optimized_calls
            optimized_calls += 1
            return (
                ("1", "2", "3", "4", "5"),
                ("1", "2", "3", "4", "5"),
                -50.0,
            )

        monkeypatch.setattr(shuffler, "_score_role_assignments_for_matchup", score_roles)

        shuffler.shuffle_branch_bound(players)

        assert optimized_calls > 0

    def test_branch_bound_is_deterministic_with_fixed_optimizer_scores(self, monkeypatch):
        """
        Branch-and-bound traversal should be deterministic for fixed optimizer scores.
        """
        players = _create_players_with_roles(14)
        shuffler = BalancedShuffler(use_glicko=True, off_role_flat_penalty=100.0)
        roles = ["1", "2", "3", "4", "5"]

        def score_roles(team1_players, team2_players, **kwargs):
            score = abs(
                sum(player.glicko_rating for player in team1_players)
                - sum(player.glicko_rating for player in team2_players)
            )
            return (
                tuple(roles),
                tuple(roles),
                score,
            )

        monkeypatch.setattr(shuffler, "_score_role_assignments_for_matchup", score_roles)

        exclusion_counts = {pl.name: 0 for pl in players}

        # Two runs are sufficient to prove identical inputs produce identical output.
        results = []
        for _ in range(2):
            team1, team2, excluded = shuffler.shuffle_branch_bound(players, exclusion_counts)
            result = (
                frozenset(p.name for p in team1.players),
                frozenset(p.name for p in team2.players),
                frozenset(p.name for p in excluded),
            )
            results.append(result)

        assert results[0] == results[1]

    def test_role_optimizer_is_deterministic_for_same_matchup(self):
        """The real role optimizer should return the same assignments and score."""
        players = _create_players_with_roles(10)
        shuffler = BalancedShuffler(use_glicko=True, off_role_flat_penalty=100.0)

        first_team1, first_team2, first_score = shuffler._optimize_role_assignments_for_matchup(
            players[:5], players[5:], max_assignments_per_team=3
        )
        second_team1, second_team2, second_score = shuffler._optimize_role_assignments_for_matchup(
            players[:5], players[5:], max_assignments_per_team=3
        )

        assert first_team1.role_assignments == second_team1.role_assignments
        assert first_team2.role_assignments == second_team2.role_assignments
        assert first_score == second_score

    def test_score_only_role_optimizer_matches_materialized_result(self):
        """The hot score-only path must choose the same roles and score."""
        players = _create_players_with_roles(10)
        shuffler = BalancedShuffler(use_glicko=True, off_role_flat_penalty=100.0)

        team1_roles, team2_roles, score = shuffler._score_role_assignments_for_matchup(
            players[:5], players[5:], max_assignments_per_team=3
        )
        team1, team2, materialized_score = shuffler._optimize_role_assignments_for_matchup(
            players[:5], players[5:], max_assignments_per_team=3
        )

        assert team1_roles == tuple(team1.role_assignments)
        assert team2_roles == tuple(team2.role_assignments)
        assert score == materialized_score

    def test_role_optimizer_reads_each_player_value_once(self, monkeypatch):
        """Role-pair exploration must reuse values instead of rescanning players."""
        players = [
            Player(
                name=f"Player{i}",
                glicko_rating=1200.0 + i * 75,
                glicko_rd=50.0 + i,
                preferred_roles=["1", "2", "3", "4", "5"],
            )
            for i in range(10)
        ]
        shuffler = BalancedShuffler()
        original_get_value = Player.get_value
        value_reads = 0

        def counted_get_value(player, *args, **kwargs):
            nonlocal value_reads
            value_reads += 1
            return original_get_value(player, *args, **kwargs)

        monkeypatch.setattr(Player, "get_value", counted_get_value)

        shuffler._optimize_role_assignments_for_matchup(
            players[:5], players[5:], max_assignments_per_team=20
        )

        assert value_reads == len(players)

    def test_pool_shuffle_reuses_five_player_role_metrics(self, monkeypatch):
        players = [
            Player(
                name=f"Player{i}",
                glicko_rating=1200.0 + i * 50,
                preferred_roles=["1", "2", "3", "4", "5"],
            )
            for i in range(11)
        ]
        shuffler = BalancedShuffler()
        original_role_metrics = shuffler._role_assignment_metrics
        metric_builds = 0

        def counted_role_metrics(*args, **kwargs):
            nonlocal metric_builds
            metric_builds += 1
            return original_role_metrics(*args, **kwargs)

        monkeypatch.setattr(
            shuffler, "_role_assignment_metrics", counted_role_metrics
        )

        shuffler.shuffle_from_pool(players)

        # Every unique five-player team is built once, with three assignment
        # candidates, instead of once for every ten-player selection it occurs in.
        assert metric_builds == math.comb(11, 5) * 3

    def test_pool_player_value_cache_is_request_scoped(self, monkeypatch):
        players = _create_players_with_roles(11)
        shuffler = BalancedShuffler()
        original_get_value = Player.get_value
        value_reads = dict.fromkeys((id(player) for player in players), 0)

        def counted_get_value(player, *args, **kwargs):
            value_reads[id(player)] += 1
            return original_get_value(player, *args, **kwargs)

        monkeypatch.setattr(Player, "get_value", counted_get_value)

        shuffler.shuffle_from_pool(players)
        assert set(value_reads.values()) == {1}

        players[0].glicko_rating += 100
        shuffler.shuffle_from_pool(players)
        assert set(value_reads.values()) == {2}

    def test_14_player_pool_with_role_preferences(self):
        """
        Test 14-player pool with varied role preferences.
        """
        roles_by_player = [
            ["1"], ["1"],  # Carry specialists
            ["2"], ["2"],  # Mid specialists
            ["3"], ["3"],  # Offlane specialists
            ["4"], ["4"],  # Soft support specialists
            ["5"], ["5"],  # Hard support specialists
            ["1", "2", "3"],  # Flex core
            ["4", "5"],  # Flex support
            ["1", "2", "3", "4", "5"],  # All roles
            ["1", "2", "3", "4", "5"],  # All roles
        ]

        players = [
            Player(
                name=f"Player{i}",
                mmr=1500 + i * 30,
                glicko_rating=1500.0 + i * 15,
                preferred_roles=roles_by_player[i],
            )
            for i in range(14)
        ]

        shuffler = BalancedShuffler(
            use_glicko=True,
            off_role_flat_penalty=100.0,
            exclusion_penalty_weight=75.0,
        )

        exclusion_counts = {pl.name: 0 for pl in players}
        team1, team2, excluded = shuffler.shuffle_from_pool(players, exclusion_counts)

        assert len(team1.players) == 5
        assert len(team2.players) == 5
        assert len(excluded) == 4

        # Verify role assignments exist
        assert team1.role_assignments is not None
        assert team2.role_assignments is not None
        assert len(team1.role_assignments) == 5
        assert len(team2.role_assignments) == 5

        all_players = team1.players + team2.players + excluded
        assert len({player.name for player in all_players}) == 14


class TestExclusionPenaltyWeightDefault:
    """Tests for the default exclusion penalty weight."""

    def test_default_weight_is_70(self):
        """Default exclusion penalty weight is 70, sourced from SHUFFLER_SETTINGS."""
        from config import SHUFFLER_SETTINGS
        assert SHUFFLER_SETTINGS["exclusion_penalty_weight"] == 70.0
        # BalancedShuffler must default to the config value, not its own literal.
        assert BalancedShuffler().exclusion_penalty_weight == SHUFFLER_SETTINGS["exclusion_penalty_weight"]

    def test_higher_weight_prevents_repeat_exclusions(self):
        """
        Test that a higher exclusion penalty weight prevents repeated exclusions.
        """
        players = _create_players_with_roles(14, base_mmr=1500, spread=0)

        # Scenario: first 4 players have been excluded twice each
        exclusion_counts = {}
        for i, pl in enumerate(players):
            if i < 4:
                exclusion_counts[pl.name] = 2  # Previously excluded twice
            else:
                exclusion_counts[pl.name] = 0

        shuffler = BalancedShuffler(
            use_glicko=True,
            off_role_flat_penalty=100.0,
            exclusion_penalty_weight=75.0,
        )

        *_, excluded = shuffler.shuffle_from_pool(players, exclusion_counts)

        high_exclusion_names = {players[i].name for i in range(4)}
        excluded_names = {p.name for p in excluded}

        # With weight 75 and count 2, penalty is 150 per excluded high-count player
        # Algorithm should avoid excluding them
        excluded_high = high_exclusion_names & excluded_names
        assert len(excluded_high) <= 1, (
            f"With weight 75, at most 1 high-exclusion player should be excluded, "
            f"got {len(excluded_high)}: {excluded_high}"
        )


class TestJopacoinBalancing:
    """Tests for jopacoin balance-based team balancing."""

    def test_player_value_jopacoin(self):
        """Player.get_value(use_jopacoin=True) returns the jopacoin balance —
        including negative (debt) and zero balances — and takes priority over
        both use_glicko and use_openskill."""
        rich = Player(name="Rich", mmr=2000, glicko_rating=1800, jopacoin_balance=500)
        assert rich.get_value(use_jopacoin=True) == 500.0
        broke = Player(name="Broke", mmr=5000, glicko_rating=2500, jopacoin_balance=-200)
        assert broke.get_value(use_jopacoin=True) == -200.0
        zero = Player(name="Zero", mmr=3000, jopacoin_balance=0)
        assert zero.get_value(use_jopacoin=True) == 0.0
        glicko = Player(name="P", glicko_rating=2000, jopacoin_balance=42)
        assert glicko.get_value(use_glicko=True, use_jopacoin=True) == 42.0
        openskill = Player(name="P", os_mu=50.0, os_sigma=3.0, jopacoin_balance=7)
        assert openskill.get_value(use_openskill=True, use_jopacoin=True) == 7.0

    def test_team_value_jopacoin(self):
        """Team value sums jopacoin balances when use_jopacoin=True, and the
        off-role penalty still applies with jopacoin balancing."""
        players = [
            Player(name="P1", mmr=2000, preferred_roles=["1"], jopacoin_balance=100),
            Player(name="P2", mmr=1800, preferred_roles=["2"], jopacoin_balance=200),
            Player(name="P3", mmr=1600, preferred_roles=["3"], jopacoin_balance=50),
            Player(name="P4", mmr=1400, preferred_roles=["4"], jopacoin_balance=75),
            Player(name="P5", mmr=1200, preferred_roles=["5"], jopacoin_balance=25),
        ]
        team = Team(players, role_assignments=["1", "2", "3", "4", "5"])
        assert team.get_team_value(use_jopacoin=True) == 450.0

        # Swap P1 and P2 roles (both off-role)
        off_role_team = Team(players, role_assignments=["2", "1", "3", "4", "5"])
        # P1 off-role: 100*0.9=90, P2 off-role: 200*0.9=180, rest on-role: 50+75+25=150
        assert off_role_team.get_team_value(
            use_jopacoin=True, off_role_multiplier=0.9
        ) == pytest.approx(420.0)

    def test_shuffler_jopacoin_balancing(self):
        """BalancedShuffler produces balanced teams by jopacoin balance."""
        players = [
            Player(name=f"P{i}", mmr=1500, preferred_roles=[str((i % 5) + 1)],
                   jopacoin_balance=(i + 1) * 100)
            for i in range(10)
        ]
        shuffler = BalancedShuffler(use_jopacoin=True)
        team1, team2 = shuffler.shuffle(players)

        team1_value = team1.get_team_value(use_jopacoin=True)
        team2_value = team2.get_team_value(use_jopacoin=True)
        # Total pool is 100+200+...+1000 = 5500, each team should be close to 2750
        assert abs(team1_value - team2_value) <= 500

    def test_get_player_by_role_jopacoin(self):
        """get_player_by_role returns jopacoin-based value when use_jopacoin=True."""
        players = [
            Player(name="P1", preferred_roles=["1"], jopacoin_balance=100),
            Player(name="P2", preferred_roles=["2"], jopacoin_balance=200),
            Player(name="P3", preferred_roles=["3"], jopacoin_balance=50),
            Player(name="P4", preferred_roles=["4"], jopacoin_balance=75),
            Player(name="P5", preferred_roles=["5"], jopacoin_balance=25),
        ]
        team = Team(players, role_assignments=["1", "2", "3", "4", "5"])
        player, value = team.get_player_by_role("2", use_jopacoin=True)
        assert player.name == "P2"
        assert value == 200.0


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
