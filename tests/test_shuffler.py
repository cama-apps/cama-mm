"""
Unit tests for the shuffler algorithm and team balancing logic.
"""

import pytest

from domain.models.player import Player
from domain.models.team import Team
from domain.services.team_balancing_service import TeamBalancingService
from shuffler import BalancedShuffler
from utils.region import region_split_mismatches, resolve_region


class TestPlayer:
    """Test Player class functionality."""

    def test_player_value_with_glicko(self):
        """Test player value calculation using Glicko-2 rating."""
        player = Player(name="TestPlayer", mmr=2000, glicko_rating=1800, wins=5, losses=3)
        assert player.get_value(use_glicko=True) == 1800

    def test_player_value_without_glicko(self):
        """Test player value calculation using MMR fallback."""
        player = Player(name="TestPlayer", mmr=2000, wins=5, losses=3)
        assert player.get_value(use_glicko=False) == 2000

    def test_player_value_no_rating(self):
        """Test player value with no rating data."""
        player = Player(name="TestPlayer")
        assert player.get_value() == 0

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

    def test_team_value_all_on_role(self):
        """Test team value when all players are on their preferred roles."""
        players = [
            Player(name="P1", mmr=2000, preferred_roles=["1"]),
            Player(name="P2", mmr=1800, preferred_roles=["2"]),
            Player(name="P3", mmr=1600, preferred_roles=["3"]),
            Player(name="P4", mmr=1400, preferred_roles=["4"]),
            Player(name="P5", mmr=1200, preferred_roles=["5"]),
        ]
        team = Team(players, role_assignments=["1", "2", "3", "4", "5"])
        value = team.get_team_value(use_glicko=False, off_role_multiplier=0.9)
        # All on-role, so full value: 2000 + 1800 + 1600 + 1400 + 1200 = 8000
        assert value == 8000

    def test_team_value_with_off_role(self):
        """Test team value when some players are off-role."""
        players = [
            Player(name="P1", mmr=2000, preferred_roles=["1"]),
            Player(name="P2", mmr=1800, preferred_roles=["2"]),
            Player(name="P3", mmr=1600, preferred_roles=["3"]),
            Player(name="P4", mmr=1400, preferred_roles=["4"]),
            Player(name="P5", mmr=1200, preferred_roles=["5"]),
        ]
        # P1 playing role 2 (off-role), P2 playing role 1 (off-role)
        team = Team(players, role_assignments=["2", "1", "3", "4", "5"])
        value = team.get_team_value(use_glicko=False, off_role_multiplier=0.9)
        # P1 (2000) and P2 (1800) are off-role: 2000*0.9 + 1800*0.9 = 3420
        # P3, P4, P5 on-role: 1600 + 1400 + 1200 = 4200
        # Total: 3420 + 4200 = 7620
        assert value == pytest.approx(7620)

    def test_off_role_count(self):
        """Test counting off-role players."""
        players = [
            Player(name="P1", mmr=2000, preferred_roles=["1"]),
            Player(name="P2", mmr=1800, preferred_roles=["2"]),
            Player(name="P3", mmr=1600, preferred_roles=["3"]),
            Player(name="P4", mmr=1400, preferred_roles=["4"]),
            Player(name="P5", mmr=1200, preferred_roles=["5"]),
        ]
        # P1 and P2 playing off-role
        team = Team(players, role_assignments=["2", "1", "3", "4", "5"])
        assert team.get_off_role_count() == 2

    def test_off_role_count_all_on_role(self):
        """Test off-role count when all players are on-role."""
        players = [
            Player(name="P1", mmr=2000, preferred_roles=["1"]),
            Player(name="P2", mmr=1800, preferred_roles=["2"]),
            Player(name="P3", mmr=1600, preferred_roles=["3"]),
            Player(name="P4", mmr=1400, preferred_roles=["4"]),
            Player(name="P5", mmr=1200, preferred_roles=["5"]),
        ]
        team = Team(players, role_assignments=["1", "2", "3", "4", "5"])
        assert team.get_off_role_count() == 0

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

    def test_lobby_rating_bonus_uses_average_team_total(self):
        shuffler = BalancedShuffler(use_glicko=False)

        bonus = shuffler._calculate_lobby_rating_bonus([1500] * 10)

        assert bonus == pytest.approx(75)

    def test_shuffle_exact_10_players(self):
        """Test shuffling with exactly 10 players."""
        players = [Player(name=f"Player{i}", mmr=1500 + i * 10) for i in range(10)]
        shuffler = BalancedShuffler(use_glicko=False, off_role_flat_penalty=50.0)
        team1, team2 = shuffler.shuffle(players)

        assert len(team1.players) == 5
        assert len(team2.players) == 5
        # All players should be assigned
        all_players = team1.players + team2.players
        assert len(all_players) == 10
        # Compare by name since Player objects aren't hashable
        all_player_names = {p.name for p in all_players}
        player_names = {p.name for p in players}
        assert all_player_names == player_names

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
        """Test that shuffled teams have similar values."""
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

    def test_shuffle_from_pool(self):
        """Test shuffling from a pool of more than 10 players."""
        players = [Player(name=f"Player{i}", mmr=1500 + i * 10) for i in range(12)]
        shuffler = BalancedShuffler(use_glicko=False, off_role_flat_penalty=50.0)
        team1, team2, excluded = shuffler.shuffle_from_pool(players)

        assert len(team1.players) == 5
        assert len(team2.players) == 5
        assert len(excluded) == 2
        # All players should be accounted for
        all_players = team1.players + team2.players + excluded
        assert len(all_players) == 12
        # Compare by name since Player objects aren't hashable
        all_player_names = {p.name for p in all_players}
        player_names = {p.name for p in players}
        assert all_player_names == player_names

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

    def test_exclusion_penalty_calculation(self):
        """Test that exclusion penalty is calculated correctly."""
        # Create 11 players (1 will be excluded)
        players = [Player(name=f"Player{i}", mmr=1500, preferred_roles=["1"]) for i in range(11)]

        # Set exclusion counts
        exclusion_counts = {}
        for i, player in enumerate(players):
            exclusion_counts[player.name] = i * 2  # 0, 2, 4, 6, 8, 10, 12, 14, 16, 18, 20

        shuffler = BalancedShuffler(
            use_glicko=False, off_role_flat_penalty=50.0, exclusion_penalty_weight=5.0
        )

        team1, team2, excluded = shuffler.shuffle_from_pool(players, exclusion_counts)

        # Verify basic structure
        assert len(team1.players) == 5
        assert len(team2.players) == 5
        assert len(excluded) == 1

        # The excluded player should preferably have lower exclusion count
        # (because excluding high-count players adds more penalty)
        # Player0 (count=0) should be more likely excluded than Player10 (count=20)
        excluded_name = excluded[0].name
        excluded_count = exclusion_counts[excluded_name]

        # The penalty for excluding Player10 (count=20) is 20*5 = 100
        # The penalty for excluding Player0 (count=0) is 0*5 = 0
        # Algorithm should prefer excluding Player0
        # Note: This is not guaranteed due to role balancing, but should trend this way
        assert excluded_count <= 10, (
            f"Expected lower-count player to be excluded, got {excluded_name} with count {excluded_count}"
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

    def test_exclusion_penalty_weight_parameter(self):
        """Test that exclusion_penalty_weight parameter is stored correctly."""
        # Test default value
        shuffler1 = BalancedShuffler()
        assert shuffler1.exclusion_penalty_weight == 70.0  # Config default

        # Test custom value
        shuffler2 = BalancedShuffler(exclusion_penalty_weight=10.0)
        assert shuffler2.exclusion_penalty_weight == 10.0

        # Test zero value (disables exclusion penalty)
        shuffler3 = BalancedShuffler(exclusion_penalty_weight=0.0)
        assert shuffler3.exclusion_penalty_weight == 0.0

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

    def test_shuffle_from_pool_without_exclusion_counts(self):
        """Test that shuffle_from_pool works without exclusion counts (backward compatibility)."""
        players = [
            Player(name=f"Player{i}", mmr=1500 + i * 10, preferred_roles=["1"]) for i in range(12)
        ]
        shuffler = BalancedShuffler(use_glicko=False, off_role_flat_penalty=50.0)

        # Call without exclusion_counts parameter
        team1, team2, excluded = shuffler.shuffle_from_pool(players)

        assert len(team1.players) == 5
        assert len(team2.players) == 5
        assert len(excluded) == 2


class TestRoleMatchupDelta:
    """Tests for the role matchup delta scoring additions."""

    def test_role_matchup_delta_calculation(self):
        """Role matchup delta should return the sum of the critical matchups."""
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

        score = service.calculate_matchup_score(team1, team2)
        # value difference = |6800 - 6400| = 400
        # off-role penalty = 0
        # total should therefore be 400 + 500 = 900
        assert score == pytest.approx(900)

    def test_role_matchup_delta_weight_applied_in_service(self):
        """Weight should scale the matchup delta when computing scores."""
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

        service = TeamBalancingService(
            use_glicko=False, off_role_multiplier=1.0, role_matchup_delta_weight=0.5
        )

        delta = service.calculate_role_matchup_delta(team1, team2)
        assert delta == 500  # sum of the five matchups (100 + 200 + 0 + 150 + 50)

        score = service.calculate_matchup_score(team1, team2)
        # value difference = |6800 - 6400| = 400
        # weighted role delta = 500 * 0.5 = 250
        # off-role penalty = 0
        assert score == pytest.approx(650)

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
        # role delta = sum(|2000-1900|, |1400-1000|, |1500-1500|, |1000-1000|, |1000-1000|)
        #            = 100 + 400 + 0 + 0 + 0 = 500
        assert score_full == pytest.approx(800)  # 300 + 500
        assert score_half == pytest.approx(550)  # 300 + (500 * 0.5)

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
        delta = shuffler._calculate_role_matchup_delta(team1, team2)

        # carry/offlane/mid all equal → 0
        # pos4 cross 1: |1300 - 1200| = 100
        # pos4 cross 2: |1100 - 900| = 200
        # total = 0 + 0 + 0 + 100 + 200 = 300
        assert delta == 300


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


class TestShuffler14Players:
    """Tests for 14-player pool shuffling (new max lobby size)."""

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

        def optimize(team1_players, team2_players, **kwargs):
            nonlocal optimized_calls
            optimized_calls += 1
            return (
                Team(team1_players, role_assignments=["1", "2", "3", "4", "5"]),
                Team(team2_players, role_assignments=["1", "2", "3", "4", "5"]),
                -50.0,
            )

        monkeypatch.setattr(shuffler, "_optimize_role_assignments_for_matchup", optimize)

        shuffler.shuffle_branch_bound(players)

        assert optimized_calls > 0

    def test_branch_bound_is_deterministic_with_fixed_optimizer_scores(self, monkeypatch):
        """
        Branch-and-bound traversal should be deterministic for fixed optimizer scores.
        """
        players = _create_players_with_roles(14)
        shuffler = BalancedShuffler(use_glicko=True, off_role_flat_penalty=100.0)
        roles = ["1", "2", "3", "4", "5"]

        def optimize(team1_players, team2_players, **kwargs):
            score = abs(
                sum(player.glicko_rating for player in team1_players)
                - sum(player.glicko_rating for player in team2_players)
            )
            return (
                Team(team1_players, role_assignments=roles),
                Team(team2_players, role_assignments=roles),
                score,
            )

        monkeypatch.setattr(shuffler, "_optimize_role_assignments_for_matchup", optimize)

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

    def test_14_player_pool_exclusion_penalty(self):
        """
        Test that exclusion penalty affects player selection in 14-player pool.
        Players with high exclusion counts should be included over those with 0.
        """
        players = _create_players_with_roles(14, base_mmr=1500, spread=0)

        # Give first 4 players high exclusion counts
        exclusion_counts = {}
        for i, pl in enumerate(players):
            if i < 4:
                exclusion_counts[pl.name] = 10  # High exclusion count
            else:
                exclusion_counts[pl.name] = 0

        # Use high exclusion penalty weight (new default from PRD)
        shuffler = BalancedShuffler(
            use_glicko=True,
            off_role_flat_penalty=100.0,
            exclusion_penalty_weight=75.0,
        )

        team1, team2, _ = shuffler.shuffle_from_pool(players, exclusion_counts)

        # Get names of included players
        included_names = {p.name for p in team1.players + team2.players}

        # High-exclusion players should be included (not excluded)
        high_exclusion_names = {players[i].name for i in range(4)}

        # With penalty weight 75 and count 10, excluding costs 750 points each
        # Algorithm should strongly prefer including them
        included_high_exclusion = high_exclusion_names & included_names

        # At least 3 of the 4 high-exclusion players should be included
        assert len(included_high_exclusion) >= 3, (
            f"Expected at least 3 high-exclusion players included, "
            f"got {len(included_high_exclusion)}: {included_high_exclusion}"
        )

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

    def test_13_player_pool(self):
        """Test 13-player pool (edge case between 12 and 14)."""
        players = _create_players_with_roles(13)
        shuffler = BalancedShuffler(use_glicko=True, off_role_flat_penalty=100.0)

        exclusion_counts = {pl.name: 0 for pl in players}
        team1, team2, excluded = shuffler.shuffle_from_pool(players, exclusion_counts)

        assert len(team1.players) == 5
        assert len(team2.players) == 5
        assert len(excluded) == 3


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
        """Player.get_value(use_jopacoin=True) returns jopacoin balance."""
        player = Player(name="Rich", mmr=2000, glicko_rating=1800, jopacoin_balance=500)
        assert player.get_value(use_jopacoin=True) == 500.0

    def test_player_value_jopacoin_negative(self):
        """Jopacoin value can be negative (players in debt)."""
        player = Player(name="Broke", mmr=5000, glicko_rating=2500, jopacoin_balance=-200)
        assert player.get_value(use_jopacoin=True) == -200.0

    def test_player_value_jopacoin_zero(self):
        """Jopacoin value is zero when balance is zero."""
        player = Player(name="Zero", mmr=3000, jopacoin_balance=0)
        assert player.get_value(use_jopacoin=True) == 0.0

    def test_player_value_jopacoin_overrides_glicko(self):
        """use_jopacoin takes priority over use_glicko."""
        player = Player(name="P", glicko_rating=2000, jopacoin_balance=42)
        assert player.get_value(use_glicko=True, use_jopacoin=True) == 42.0

    def test_player_value_jopacoin_overrides_openskill(self):
        """use_jopacoin takes priority over use_openskill."""
        player = Player(name="P", os_mu=50.0, os_sigma=3.0, jopacoin_balance=7)
        assert player.get_value(use_openskill=True, use_jopacoin=True) == 7.0

    def test_team_value_jopacoin(self):
        """Team value sums jopacoin balances when use_jopacoin=True."""
        players = [
            Player(name="P1", mmr=2000, preferred_roles=["1"], jopacoin_balance=100),
            Player(name="P2", mmr=1800, preferred_roles=["2"], jopacoin_balance=200),
            Player(name="P3", mmr=1600, preferred_roles=["3"], jopacoin_balance=50),
            Player(name="P4", mmr=1400, preferred_roles=["4"], jopacoin_balance=75),
            Player(name="P5", mmr=1200, preferred_roles=["5"], jopacoin_balance=25),
        ]
        team = Team(players, role_assignments=["1", "2", "3", "4", "5"])
        value = team.get_team_value(use_jopacoin=True)
        assert value == 450.0

    def test_team_value_jopacoin_off_role_penalty(self):
        """Off-role penalty still applies with jopacoin balancing."""
        players = [
            Player(name="P1", mmr=2000, preferred_roles=["1"], jopacoin_balance=100),
            Player(name="P2", mmr=1800, preferred_roles=["2"], jopacoin_balance=200),
            Player(name="P3", mmr=1600, preferred_roles=["3"], jopacoin_balance=50),
            Player(name="P4", mmr=1400, preferred_roles=["4"], jopacoin_balance=75),
            Player(name="P5", mmr=1200, preferred_roles=["5"], jopacoin_balance=25),
        ]
        # Swap P1 and P2 roles (both off-role)
        team = Team(players, role_assignments=["2", "1", "3", "4", "5"])
        value = team.get_team_value(use_jopacoin=True, off_role_multiplier=0.9)
        # P1 off-role: 100*0.9=90, P2 off-role: 200*0.9=180, rest on-role: 50+75+25=150
        assert value == pytest.approx(420.0)

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
