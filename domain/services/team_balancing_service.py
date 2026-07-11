"""
Team balancing domain service.

Handles team value calculations and balance scoring.
"""

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


    def calculate_role_matchup_delta(self, team1: Team, team2: Team) -> float:
        """
        Calculate the sum of role matchup deltas between two teams.

        Compares:
        - Team1 carry (1) vs Team2 offlane (3)
        - Team2 carry (1) vs Team1 offlane (3)
        - Team1 mid (2) vs Team2 mid (2)
        - Team1 pos4 vs Team2 pos5 (cross-lane support)
        - Team2 pos4 vs Team1 pos5 (cross-lane support)

        Args:
            team1: First team
            team2: Second team

        Returns:
            Sum of deltas across the five critical matchups
        """
        # Get players and their effective values for each role
        _, team1_carry_value = team1.get_player_by_role(
            "1", self.use_glicko, self.off_role_multiplier
        )
        _, team1_offlane_value = team1.get_player_by_role(
            "3", self.use_glicko, self.off_role_multiplier
        )
        _, team1_mid_value = team1.get_player_by_role(
            "2", self.use_glicko, self.off_role_multiplier
        )
        _, team1_pos4_value = team1.get_player_by_role(
            "4", self.use_glicko, self.off_role_multiplier
        )
        _, team1_pos5_value = team1.get_player_by_role(
            "5", self.use_glicko, self.off_role_multiplier
        )

        _, team2_carry_value = team2.get_player_by_role(
            "1", self.use_glicko, self.off_role_multiplier
        )
        _, team2_offlane_value = team2.get_player_by_role(
            "3", self.use_glicko, self.off_role_multiplier
        )
        _, team2_mid_value = team2.get_player_by_role(
            "2", self.use_glicko, self.off_role_multiplier
        )
        _, team2_pos4_value = team2.get_player_by_role(
            "4", self.use_glicko, self.off_role_multiplier
        )
        _, team2_pos5_value = team2.get_player_by_role(
            "5", self.use_glicko, self.off_role_multiplier
        )

        # Calculate the five critical matchups
        carry_vs_offlane_1 = abs(team1_carry_value - team2_offlane_value)
        carry_vs_offlane_2 = abs(team2_carry_value - team1_offlane_value)
        mid_vs_mid = abs(team1_mid_value - team2_mid_value)
        support_cross_1 = abs(team1_pos4_value - team2_pos5_value)
        support_cross_2 = abs(team2_pos4_value - team1_pos5_value)

        # Return the sum of all five deltas
        return carry_vs_offlane_1 + carry_vs_offlane_2 + mid_vs_mid + support_cross_1 + support_cross_2

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




