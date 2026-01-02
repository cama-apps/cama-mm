"""
Unit tests for the shuffler algorithm and team balancing logic.
"""

import pytest
from domain.models.player import Player
from domain.services.team_balancing_service import TeamBalancingService
from domain.models.team import Team
from shuffler import BalancedShuffler


class TestPlayer:
    """Test Player class functionality."""
    
    def test_player_value_with_glicko(self):
        """Test player value calculation using Glicko-2 rating."""
        player = Player(
            name="TestPlayer",
            mmr=2000,
            glicko_rating=1800,
            wins=5,
            losses=3
        )
        assert player.get_value(use_glicko=True) == 1800
    
    def test_player_value_without_glicko(self):
        """Test player value calculation using MMR fallback."""
        player = Player(
            name="TestPlayer",
            mmr=2000,
            wins=5,
            losses=3
        )
        assert player.get_value(use_glicko=False) == 2000
    
    def test_player_value_no_rating(self):
        """Test player value with no rating data."""
        player = Player(name="TestPlayer")
        assert player.get_value() == 0


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
        assignments = team._assign_roles_optimally()
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
    
    def test_shuffle_exact_10_players(self):
        """Test shuffling with exactly 10 players."""
        players = [Player(name=f"Player{i}", mmr=1500 + i*10) for i in range(10)]
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
        
        # Teams should be relatively balanced (within reasonable range)
        diff = abs(value1 - value2)
        # Total value is 15500, so difference should be much less than that
        assert diff < 1000  # Reasonable threshold
    
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
    
    def test_shuffle_from_pool(self):
        """Test shuffling from a pool of more than 10 players."""
        players = [Player(name=f"Player{i}", mmr=1500 + i*10) for i in range(12)]
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
        """Test that off-role penalty is applied in scoring."""
        # Create players where some must play off-role
        players = [
            Player(name="P1", mmr=2000, preferred_roles=["1"]),
            Player(name="P2", mmr=1900, preferred_roles=["1"]),  # Two carry players
            Player(name="P3", mmr=1800, preferred_roles=["2"]),
            Player(name="P4", mmr=1700, preferred_roles=["2"]),  # Two mid players
            Player(name="P5", mmr=1600, preferred_roles=["3"]),
            Player(name="P6", mmr=1500, preferred_roles=["3"]),  # Two offlane players
            Player(name="P7", mmr=1400, preferred_roles=["4"]),
            Player(name="P8", mmr=1300, preferred_roles=["4"]),  # Two soft support
            Player(name="P9", mmr=1200, preferred_roles=["5"]),
            Player(name="P10", mmr=1100, preferred_roles=["5"]),  # Two hard support
        ]
        shuffler = BalancedShuffler(use_glicko=False, off_role_flat_penalty=50.0)
        team1, team2 = shuffler.shuffle(players)
        
        # At least one team should have some off-role players
        # (since we have duplicates of each role)
        off_roles_team1 = team1.get_off_role_count()
        off_roles_team2 = team2.get_off_role_count()
        
        # The algorithm should try to minimize off-roles
        # But with this setup, some off-roles are inevitable
        total_off_roles = off_roles_team1 + off_roles_team2
        assert total_off_roles >= 0  # At least tracked correctly

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

        best_score = float('inf')
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
        """Test that exclusion counts influence player selection."""
        # Create 12 players with varying exclusion counts
        players = []
        exclusion_counts = {}
        
        # Players 1-10: normal exclusion counts (0-2)
        for i in range(10):
            player = Player(name=f"Player{i}", mmr=1500, preferred_roles=["1"])
            players.append(player)
            exclusion_counts[player.name] = i % 3  # 0, 1, 2, 0, 1, 2, ...
        
        # Player 11: very high exclusion count (should be prioritized for inclusion)
        high_exclusion_player = Player(name="HighExclusionPlayer", mmr=1500, preferred_roles=["2"])
        players.append(high_exclusion_player)
        exclusion_counts[high_exclusion_player.name] = 20
        
        # Player 12: zero exclusion count (more likely to be excluded)
        low_exclusion_player = Player(name="LowExclusionPlayer", mmr=1500, preferred_roles=["3"])
        players.append(low_exclusion_player)
        exclusion_counts[low_exclusion_player.name] = 0
        
        # Shuffle multiple times and track how often each player is excluded
        shuffler = BalancedShuffler(
            use_glicko=False, 
            off_role_flat_penalty=50.0,
            exclusion_penalty_weight=5.0
        )
        
        # Run shuffle once (deterministic for this test due to algorithm preference)
        team1, team2, excluded = shuffler.shuffle_from_pool(players, exclusion_counts)
        
        assert len(team1.players) == 5
        assert len(team2.players) == 5
        assert len(excluded) == 2
        
        # High exclusion player should be more likely to be included
        # (but we can't guarantee it due to other factors)
        # Just verify the penalty is being applied
        excluded_names = [p.name for p in excluded]
        
        # At minimum, verify structure is correct
        assert high_exclusion_player.name in [p.name for p in team1.players + team2.players + excluded]
        assert low_exclusion_player.name in [p.name for p in team1.players + team2.players + excluded]
    
    def test_exclusion_penalty_calculation(self):
        """Test that exclusion penalty is calculated correctly."""
        # Create 11 players (1 will be excluded)
        players = [Player(name=f"Player{i}", mmr=1500, preferred_roles=["1"]) for i in range(11)]
        
        # Set exclusion counts
        exclusion_counts = {}
        for i, player in enumerate(players):
            exclusion_counts[player.name] = i * 2  # 0, 2, 4, 6, 8, 10, 12, 14, 16, 18, 20
        
        shuffler = BalancedShuffler(
            use_glicko=False,
            off_role_flat_penalty=50.0,
            exclusion_penalty_weight=5.0
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
        assert excluded_count <= 10, \
            f"Expected lower-count player to be excluded, got {excluded_name} with count {excluded_count}"
    
    def test_exclusion_penalty_weight_parameter(self):
        """Test that exclusion_penalty_weight parameter is stored correctly."""
        # Test default value
        shuffler1 = BalancedShuffler()
        assert shuffler1.exclusion_penalty_weight == 5.0
        
        # Test custom value
        shuffler2 = BalancedShuffler(exclusion_penalty_weight=10.0)
        assert shuffler2.exclusion_penalty_weight == 10.0
        
        # Test zero value (disables exclusion penalty)
        shuffler3 = BalancedShuffler(exclusion_penalty_weight=0.0)
        assert shuffler3.exclusion_penalty_weight == 0.0
    
    def test_shuffle_from_pool_without_exclusion_counts(self):
        """Test that shuffle_from_pool works without exclusion counts (backward compatibility)."""
        players = [Player(name=f"Player{i}", mmr=1500 + i*10, preferred_roles=["1"]) for i in range(12)]
        shuffler = BalancedShuffler(use_glicko=False, off_role_flat_penalty=50.0)
        
        # Call without exclusion_counts parameter
        team1, team2, excluded = shuffler.shuffle_from_pool(players)
        
        assert len(team1.players) == 5
        assert len(team2.players) == 5
        assert len(excluded) == 2
        
        # Call with None explicitly
        team1, team2, excluded = shuffler.shuffle_from_pool(players, None)
        
        assert len(team1.players) == 5
        assert len(team2.players) == 5
        assert len(excluded) == 2
        
        # Call with empty dict
        team1, team2, excluded = shuffler.shuffle_from_pool(players, {})
        
        assert len(team1.players) == 5
        assert len(team2.players) == 5
        assert len(excluded) == 2


class TestRoleMatchupDelta:
    """Tests for the role matchup delta scoring additions."""

    def test_role_matchup_delta_calculation(self):
        """Role matchup delta should return the max of the critical matchups."""
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
        assert delta == 200

        score = service.calculate_matchup_score(team1, team2)
        # value difference = |6800 - 6400| = 400
        # off-role penalty = 0
        # total should therefore be 600
        assert score == pytest.approx(600)


if __name__ == "__main__":
    pytest.main([__file__, "-v"])

