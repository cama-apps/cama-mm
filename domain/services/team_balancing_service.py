"""
Team balancing domain service.

Handles team value calculations and balance scoring.
"""

from domain.models.player import Player
from domain.models.team import Team
from domain.services.role_assignment_service import RoleAssignmentService


class TeamBalancingService:
    """
    Pure domain service for team balancing logic.

    Responsibilities:
    - Calculate team values
    - Score team balance
    - Apply off-role penalties
    """

    def __init__(
        self,
        use_glicko: bool = True,
        off_role_multiplier: float = 0.9,
        off_role_flat_penalty: float = 50.0,
        role_matchup_delta_weight: float = 1.0,
    ):
        """
        Initialize team balancing service.

        Args:
            use_glicko: Whether to use Glicko-2 ratings
            off_role_multiplier: Multiplier for rating when playing off-role
            off_role_flat_penalty: Flat penalty per off-role player
            role_matchup_delta_weight: Weight applied to lane matchup delta in scores
        """
        self.use_glicko = use_glicko
        self.off_role_multiplier = off_role_multiplier
        self.off_role_flat_penalty = off_role_flat_penalty
        self.role_matchup_delta_weight = role_matchup_delta_weight
        self.role_service = RoleAssignmentService()

    def calculate_team_value(self, team: Team) -> float:
        """
        Calculate total team value with off-role penalties.

        Args:
            team: Team to evaluate

        Returns:
            Team value adjusted for role assignments
        """
        return team.get_team_value(self.use_glicko, self.off_role_multiplier)

    def calculate_player_effective_value(self, player: Player, assigned_role: str) -> float:
        """
        Calculate a player's effective value for a given role.

        Args:
            player: Player to evaluate
            assigned_role: Role assigned to the player

        Returns:
            Player's effective value with off-role penalty applied if needed
        """
        base_value = player.get_value(self.use_glicko)

        if player.preferred_roles and assigned_role in player.preferred_roles:
            return base_value
        else:
            return base_value * self.off_role_multiplier

    def calculate_role_matchup_delta(self, team1: Team, team2: Team) -> float:
        """
        Calculate the sum of role matchup deltas between two teams.

        Compares:
        - Team1 carry (1) vs Team2 offlane (3)
        - Team2 carry (1) vs Team1 offlane (3)
        - Team1 mid (2) vs Team2 mid (2)

        Args:
            team1: First team
            team2: Second team

        Returns:
            Sum of deltas across the three critical matchups
        """
        # Get players and their effective values for each role
        team1_carry_player, team1_carry_value = team1.get_player_by_role(
            "1", self.use_glicko, self.off_role_multiplier
        )
        team1_offlane_player, team1_offlane_value = team1.get_player_by_role(
            "3", self.use_glicko, self.off_role_multiplier
        )
        team1_mid_player, team1_mid_value = team1.get_player_by_role(
            "2", self.use_glicko, self.off_role_multiplier
        )

        team2_carry_player, team2_carry_value = team2.get_player_by_role(
            "1", self.use_glicko, self.off_role_multiplier
        )
        team2_offlane_player, team2_offlane_value = team2.get_player_by_role(
            "3", self.use_glicko, self.off_role_multiplier
        )
        team2_mid_player, team2_mid_value = team2.get_player_by_role(
            "2", self.use_glicko, self.off_role_multiplier
        )

        # Calculate the three critical matchups
        carry_vs_offlane_1 = abs(team1_carry_value - team2_offlane_value)
        carry_vs_offlane_2 = abs(team2_carry_value - team1_offlane_value)
        mid_vs_mid = abs(team1_mid_value - team2_mid_value)

        # Return the sum of all three deltas
        return carry_vs_offlane_1 + carry_vs_offlane_2 + mid_vs_mid

    def calculate_matchup_score(self, team1: Team, team2: Team) -> float:
        """
        Calculate a score for a matchup (lower is better).

        Combines value difference, off-role penalties, and role matchup deltas.

        Args:
            team1: First team
            team2: Second team

        Returns:
            Matchup score (lower = more balanced)
        """
        team1_value = self.calculate_team_value(team1)
        team2_value = self.calculate_team_value(team2)
        value_diff = abs(team1_value - team2_value)

        off_role_penalty = (
            team1.get_off_role_count() + team2.get_off_role_count()
        ) * self.off_role_flat_penalty

        # Calculate role matchup delta (sum of deltas across critical matchups)
        role_matchup_delta = self.calculate_role_matchup_delta(team1, team2)

        return value_diff + off_role_penalty + (role_matchup_delta * self.role_matchup_delta_weight)

    def get_team_stats(self, team: Team) -> dict:
        """
        Get detailed stats for a team.

        Args:
            team: Team to analyze

        Returns:
            Dictionary with team statistics
        """
        value = self.calculate_team_value(team)
        off_role_count = team.get_off_role_count()
        role_distribution = team.get_role_distribution()
        role_balance_score = team.get_role_balance_score()

        return {
            "value": value,
            "off_role_count": off_role_count,
            "role_distribution": role_distribution,
            "role_balance_score": role_balance_score,
            "is_balanced": team.has_balanced_roles(),
        }

    def compare_matchups(self, matchup1: tuple[Team, Team], matchup2: tuple[Team, Team]) -> int:
        """
        Compare two matchups and return which is better.

        Args:
            matchup1: First matchup (team1, team2)
            matchup2: Second matchup (team1, team2)

        Returns:
            -1 if matchup1 is better, 1 if matchup2 is better, 0 if equal
        """
        score1 = self.calculate_matchup_score(matchup1[0], matchup1[1])
        score2 = self.calculate_matchup_score(matchup2[0], matchup2[1])

        if score1 < score2:
            return -1
        elif score1 > score2:
            return 1
        return 0

    def calculate_average_value(self, players: list[Player]) -> float:
        """
        Calculate average player value.

        Args:
            players: List of players

        Returns:
            Average player value
        """
        if not players:
            return 0

        total = sum(p.get_value(self.use_glicko) for p in players)
        return total / len(players)

    def estimate_team_strength(self, players: list[Player]) -> float:
        """
        Estimate team strength from a list of players.

        This is a simpler estimate without role assignment optimization.

        Args:
            players: List of players

        Returns:
            Estimated team strength
        """
        return sum(p.get_value(self.use_glicko) for p in players)
