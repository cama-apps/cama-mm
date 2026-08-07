"""
Team balancing domain service.

Handles team value calculations and balance scoring.
"""

from domain.models.team import Team


def role_matchup_delta_from_values(
    team1_values: tuple[float, ...],
    team2_values: tuple[float, ...],
) -> float:
    """
    Sum of the five critical lane matchups from effective role values.

    Values are ordered by position ("1".."5"). Compares:
    - Team1 carry (1) vs Team2 offlane (3), and vice versa
    - Team1 mid (2) vs Team2 mid (2)
    - Team1 pos4 vs Team2 pos5 (cross-lane support), and vice versa

    This is the single canonical formula; the shuffler's inlined hot loops
    keep manual copies for speed that are pinned to it by tests
    (tests/test_shuffler.py::TestRoleDeltaFormulaConsistency).
    """
    return (
        abs(team1_values[0] - team2_values[2])
        + abs(team2_values[0] - team1_values[2])
        + abs(team1_values[1] - team2_values[1])
        + abs(team1_values[3] - team2_values[4])
        + abs(team2_values[3] - team1_values[4])
    )


def role_parity_delta_from_values(
    team1_values: tuple[float, ...],
    team2_values: tuple[float, ...],
) -> float:
    """Sum of same-role value differences from effective role values."""
    return sum(abs(v1 - v2) for v1, v2 in zip(team1_values, team2_values))


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
            role_matchup_delta_weight: Weight applied to lane and role parity deltas in scores
        """
        self.use_glicko = use_glicko
        self.off_role_multiplier = off_role_multiplier
        self.off_role_flat_penalty = off_role_flat_penalty
        self.role_matchup_delta_weight = role_matchup_delta_weight

    def calculate_team_value(
        self,
        team: Team,
        *,
        use_openskill: bool = False,
        use_jopacoin: bool = False,
    ) -> float:
        """
        Calculate total team value with off-role penalties.

        Args:
            team: Team to evaluate
            use_openskill: Score with OpenSkill ratings instead of Glicko
            use_jopacoin: Score with jopacoin balances instead of ratings

        Returns:
            Team value adjusted for role assignments
        """
        return team.get_team_value(
            self.use_glicko,
            self.off_role_multiplier,
            use_openskill=use_openskill,
            use_jopacoin=use_jopacoin,
        )


    def calculate_role_matchup_delta(
        self,
        team1: Team,
        team2: Team,
        *,
        use_openskill: bool = False,
        use_jopacoin: bool = False,
    ) -> float:
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
            use_openskill: Score with OpenSkill ratings instead of Glicko
            use_jopacoin: Score with jopacoin balances instead of ratings

        The rating-system flags must match the ones the teams were built with.
        Omitting them scored the lanes in Glicko while the team values used
        OpenSkill/jopacoin, mixing two scales in one balance breakdown.

        Returns:
            Sum of deltas across the five critical matchups
        """
        return role_matchup_delta_from_values(
            self._role_values(team1, use_openskill=use_openskill, use_jopacoin=use_jopacoin),
            self._role_values(team2, use_openskill=use_openskill, use_jopacoin=use_jopacoin),
        )

    def calculate_role_parity_delta(
        self,
        team1: Team,
        team2: Team,
        *,
        use_openskill: bool = False,
        use_jopacoin: bool = False,
    ) -> float:
        """Return the sum of same-role value differences between two teams."""
        return role_parity_delta_from_values(
            self._role_values(team1, use_openskill=use_openskill, use_jopacoin=use_jopacoin),
            self._role_values(team2, use_openskill=use_openskill, use_jopacoin=use_jopacoin),
        )

    def _role_values(
        self,
        team: Team,
        *,
        use_openskill: bool = False,
        use_jopacoin: bool = False,
    ) -> tuple[float, ...]:
        """Return the team's effective values ordered by position ("1".."5")."""

        def role_value(role: str) -> float:
            _, value = team.get_player_by_role(
                role,
                self.use_glicko,
                self.off_role_multiplier,
                use_openskill=use_openskill,
                use_jopacoin=use_jopacoin,
            )
            return value

        return tuple(role_value(role) for role in ("1", "2", "3", "4", "5"))

    def calculate_matchup_score(
        self,
        team1: Team,
        team2: Team,
        *,
        use_openskill: bool = False,
        use_jopacoin: bool = False,
    ) -> float:
        """
        Calculate a score for a matchup (lower is better).

        Combines value difference, off-role penalties, and role matchup deltas.

        Args:
            team1: First team
            team2: Second team
            use_openskill: Score with OpenSkill ratings instead of Glicko
            use_jopacoin: Score with jopacoin balances instead of ratings

        Returns:
            Matchup score (lower = more balanced)
        """
        team1_value = self.calculate_team_value(
            team1,
            use_openskill=use_openskill,
            use_jopacoin=use_jopacoin,
        )
        team2_value = self.calculate_team_value(
            team2,
            use_openskill=use_openskill,
            use_jopacoin=use_jopacoin,
        )
        value_diff = abs(team1_value - team2_value)

        off_role_penalty = (
            team1.get_off_role_count() + team2.get_off_role_count()
        ) * self.off_role_flat_penalty

        # Calculate lane matchup and same-role parity deltas.
        role_matchup_delta = self.calculate_role_matchup_delta(
            team1,
            team2,
            use_openskill=use_openskill,
            use_jopacoin=use_jopacoin,
        )
        role_parity_delta = self.calculate_role_parity_delta(
            team1,
            team2,
            use_openskill=use_openskill,
            use_jopacoin=use_jopacoin,
        )

        return (
            value_diff
            + off_role_penalty
            + ((role_matchup_delta + role_parity_delta) * self.role_matchup_delta_weight)
        )




