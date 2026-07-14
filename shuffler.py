"""
Balanced team shuffling algorithm.
"""

import heapq
import itertools
import logging
import math
import random
from collections.abc import Iterable
from dataclasses import dataclass

from config import (
    PACKAGE_DEAL_PENALTY,
    PACKAGE_DEAL_SPLIT_PENALTY,
    RATING_SPREAD_DIVISOR,
    RD_PRIORITY_WEIGHT,
    SHUFFLER_SETTINGS,
    SOFT_AVOID_PENALTY,
)
from domain.models.player import Player
from domain.models.team import Team
from utils.region import region_split_mismatches
from utils.role_assignment_cache import get_cached_role_assignments

logger = logging.getLogger("cama_bot.shuffler")


@dataclass
class DraftPoolResult:
    """Result of balanced draft pool selection."""

    selected_players: list[Player]  # 8 non-captain players
    excluded_players: list[Player]  # Players not selected
    pool_score: float  # Best achievable balance score + penalties


@dataclass
class _PoolMatchup:
    """A single evaluated team split within a pool shuffle."""

    team1: Team
    team2: Team
    total_score: float
    value_diff: float
    total_off_roles: int
    log_entry: tuple  # 12-tuple consumed by BalancedShuffler._log_top_matchups


class BalancedShuffler:
    """
    Implements balanced team shuffling algorithm.

    Minimizes team value difference while optionally considering role distribution.
    """

    def __init__(
        self,
        use_glicko: bool = True,
        consider_roles: bool = True,
        off_role_multiplier: float | None = None,
        off_role_flat_penalty: float | None = None,
        role_matchup_delta_weight: float | None = None,
        exclusion_penalty_weight: float | None = None,
        rd_priority_weight: float | None = None,
        use_openskill: bool = False,
        use_jopacoin: bool = False,
        recent_match_penalty_weight: float | None = None,
        soft_avoid_penalty: float | None = None,
        package_deal_penalty: float | None = None,
        package_deal_split_penalty: float | None = None,
        rating_spread_divisor: float | None = None,
        region_split: bool = False,
        region_split_penalty: float | None = None,
    ):
        """
        Initialize the shuffler.

        Args:
            use_glicko: Whether to use Glicko-2 ratings (default True)
            consider_roles: Whether to consider role distribution
            off_role_multiplier: Multiplier for MMR when playing off-role (default 0.95 = 95% effectiveness)
            off_role_flat_penalty: Flat penalty per off-role player added to team value difference (see SHUFFLER_SETTINGS)
            role_matchup_delta_weight: Weight applied to lane matchup delta when scoring teams
            exclusion_penalty_weight: Penalty per exclusion count for excluded players (default 50.0)
            use_openskill: Whether to use OpenSkill ratings instead of Glicko-2 (default False)
            use_jopacoin: Whether to use jopacoin balance instead of ratings (default False)
            recent_match_penalty_weight: Penalty per recent match participant selected (see SHUFFLER_SETTINGS)
            soft_avoid_penalty: Penalty added when avoider/avoided pair are on same team (default 500.0)
            package_deal_penalty: Penalty added when buyer/partner pair are on DIFFERENT teams (default 100.0)
            package_deal_split_penalty: Penalty added when one of the pair is excluded from the match (default 100.0)
            rating_spread_divisor: Divisor for (max_rating - min_rating) pool spread penalty (default 10.0)
            region_split: Whether to prefer US West vs US East teams.
            region_split_penalty: Penalty per region mismatch in region split mode.
        """
        self.use_glicko = use_glicko
        self.consider_roles = consider_roles
        self.use_openskill = use_openskill
        self.use_jopacoin = use_jopacoin
        settings = SHUFFLER_SETTINGS
        self.off_role_multiplier = (
            off_role_multiplier
            if off_role_multiplier is not None
            else settings["off_role_multiplier"]
        )
        self.off_role_flat_penalty = (
            off_role_flat_penalty
            if off_role_flat_penalty is not None
            else settings["off_role_flat_penalty"]
        )
        self.role_matchup_delta_weight = (
            role_matchup_delta_weight
            if role_matchup_delta_weight is not None
            else settings["role_matchup_delta_weight"]
        )
        self.exclusion_penalty_weight = (
            exclusion_penalty_weight
            if exclusion_penalty_weight is not None
            else settings["exclusion_penalty_weight"]
        )
        self.rd_priority_weight = (
            rd_priority_weight
            if rd_priority_weight is not None
            else RD_PRIORITY_WEIGHT
        )
        self.recent_match_penalty_weight = (
            recent_match_penalty_weight
            if recent_match_penalty_weight is not None
            else settings["recent_match_penalty_weight"]
        )
        self.soft_avoid_penalty = (
            soft_avoid_penalty
            if soft_avoid_penalty is not None
            else SOFT_AVOID_PENALTY
        )
        self.package_deal_penalty = (
            package_deal_penalty
            if package_deal_penalty is not None
            else PACKAGE_DEAL_PENALTY
        )
        self.package_deal_split_penalty = (
            package_deal_split_penalty
            if package_deal_split_penalty is not None
            else PACKAGE_DEAL_SPLIT_PENALTY
        )
        self.rating_spread_divisor = (
            rating_spread_divisor
            if rating_spread_divisor is not None
            else RATING_SPREAD_DIVISOR
        )
        self.region_split = region_split
        self.region_split_penalty = (
            region_split_penalty
            if region_split_penalty is not None
            else settings["region_split_penalty"]
        )

    def _calculate_rating_spread_penalty(self, player_values: list[float]) -> float:
        """
        Calculate penalty for rating spread among selected players.

        Penalizes wide skill gaps in pool shuffles to incentivize closer matches.

        Args:
            player_values: List of player rating values

        Returns:
            (max_value - min_value) / rating_spread_divisor
        """
        if len(player_values) < 2:
            return 0.0
        return (max(player_values) - min(player_values)) / self.rating_spread_divisor

    @staticmethod
    def _calculate_lobby_rating_bonus(player_values: list[float]) -> float:
        """Return the average team rating total divided by 100."""
        return sum(player_values) / 2 / 100

    def _calculate_role_matchup_delta(self, team1: Team, team2: Team) -> float:
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
        # Effective value for each role, per team
        def role_value(team: Team, role: str) -> float:
            _player, value = team.get_player_by_role(
                role,
                self.use_glicko,
                self.off_role_multiplier,
                use_openskill=self.use_openskill,
                use_jopacoin=self.use_jopacoin,
            )
            return value

        t1 = {role: role_value(team1, role) for role in ("1", "2", "3", "4", "5")}
        t2 = {role: role_value(team2, role) for role in ("1", "2", "3", "4", "5")}

        # Calculate the five critical matchups
        carry_vs_offlane_1 = abs(t1["1"] - t2["3"])
        carry_vs_offlane_2 = abs(t2["1"] - t1["3"])
        mid_vs_mid = abs(t1["2"] - t2["2"])
        support_cross_1 = abs(t1["4"] - t2["5"])
        support_cross_2 = abs(t2["4"] - t1["5"])

        # Return the sum of all five deltas
        return carry_vs_offlane_1 + carry_vs_offlane_2 + mid_vs_mid + support_cross_1 + support_cross_2

    def _calculate_region_split_penalty(
        self,
        team1_players: list[Player],
        team2_players: list[Player],
    ) -> float:
        """Penalty for teams that do not cleanly split resolved USE/USW players."""
        if not self.region_split or self.region_split_penalty <= 0:
            return 0.0
        return region_split_mismatches(team1_players, team2_players) * self.region_split_penalty

    def _calculate_rd_priority(self, players: Iterable[Player]) -> float:
        """Compute the RD-based bonus for a group of players."""
        if self.rd_priority_weight <= 0:
            return 0.0
        rd_total = sum(p.glicko_rd or 0.0 for p in players)
        return rd_total * self.rd_priority_weight

    def _calculate_soft_avoid_penalty(
        self,
        team1_ids: set[int],
        team2_ids: set[int],
        avoids: list | None,
    ) -> float:
        """
        Calculate penalty for soft avoids where avoider/avoided are on the same team.

        Avoids are ADDITIVE: if A avoids B AND B avoids A, the penalty is doubled.

        Args:
            team1_ids: Set of discord IDs on team 1
            team2_ids: Set of discord IDs on team 2
            avoids: List of SoftAvoid objects (or any object with avoider_discord_id and avoided_discord_id)

        Returns:
            Total penalty for same-team avoid pairs
        """
        if not avoids:
            return 0.0

        penalty = 0.0
        for avoid in avoids:
            avoider = avoid.avoider_discord_id
            avoided = avoid.avoided_discord_id

            # Check if both are on the same team (penalty applies)
            both_on_team1 = avoider in team1_ids and avoided in team1_ids
            both_on_team2 = avoider in team2_ids and avoided in team2_ids

            if both_on_team1 or both_on_team2:
                penalty += self.soft_avoid_penalty

        return penalty

    def _calculate_package_deal_penalty(
        self,
        team1_ids: set[int],
        team2_ids: set[int],
        deals: list | None,
    ) -> float:
        """
        Calculate penalty for package deals where buyer/partner are on DIFFERENT teams.

        This is the inverse of soft avoid - we want the pair on the SAME team.
        Deals are ADDITIVE: if A deals with B AND B deals with A, the penalty is doubled.

        Args:
            team1_ids: Set of discord IDs on team 1
            team2_ids: Set of discord IDs on team 2
            deals: List of PackageDeal objects (or any object with buyer_discord_id and partner_discord_id)

        Returns:
            Total penalty for different-team deal pairs
        """
        if not deals:
            return 0.0

        penalty = 0.0
        for deal in deals:
            buyer = deal.buyer_discord_id
            partner = deal.partner_discord_id

            # Check if on OPPOSITE teams (penalty applies)
            on_opposite = (
                (buyer in team1_ids and partner in team2_ids) or
                (buyer in team2_ids and partner in team1_ids)
            )

            if on_opposite:
                penalty += self.package_deal_penalty

        return penalty

    def _calculate_package_deal_split_penalty(
        self,
        selected_ids: set[int],
        excluded_ids: set[int],
        deals: list | None,
    ) -> float:
        """
        Calculate penalty for package deals where one player is selected and one is excluded.

        Args:
            selected_ids: Set of discord IDs for players selected to play
            excluded_ids: Set of discord IDs for players excluded from the match
            deals: List of PackageDeal objects (or any object with buyer_discord_id and partner_discord_id)

        Returns:
            Total penalty for split deal pairs
        """
        if not deals:
            return 0.0

        penalty = 0.0
        for deal in deals:
            buyer = deal.buyer_discord_id
            partner = deal.partner_discord_id

            # Split: one selected, one excluded
            is_split = (
                (buyer in selected_ids and partner in excluded_ids) or
                (buyer in excluded_ids and partner in selected_ids)
            )
            if is_split:
                penalty += self.package_deal_split_penalty

        return penalty

    def _greedy_shuffle(
        self,
        players: list[Player],
        exclusion_counts: dict[str, int] | None = None,
        recent_match_names: set[str] | None = None,
        avoids: list | None = None,
        deals: list | None = None,

    ) -> tuple[Team, Team, list[Player], float]:
        """
        Greedy snake-draft shuffle for initial upper bound in branch and bound.

        Alternates picking highest-rated players to balance team values.

        Args:
            players: List of players (10-14)
            exclusion_counts: Optional dict mapping player names to exclusion counts
            recent_match_names: Optional set of player names who participated in the most recent match
            avoids: Optional list of SoftAvoid objects to apply same-team penalties
            deals: Optional list of PackageDeal objects to apply different-team penalties


        Returns:
            Tuple of (team1, team2, excluded_players, score)
        """
        exclusion_counts = exclusion_counts or {}
        recent_match_names = recent_match_names or set()

        # Sort by rating descending
        sorted_players = sorted(
            players,
            key=lambda p: p.get_value(self.use_glicko, use_openskill=self.use_openskill, use_jopacoin=self.use_jopacoin),
            reverse=True,
        )

        # For >10 players, exclude those with lowest exclusion counts first (greedily)
        # Recent match participants get lower effective count (more likely to be excluded)
        if len(players) > 10:
            # Effective exclusion count over ALL players: high count means the
            # player sat out often and should be protected (included);
            # recent match participants have their count reduced to prefer excluding them
            def effective_exclusion_count(p: Player) -> float:
                base_count = exclusion_counts.get(p.name, 0)
                # Reduce count for recent participants (makes them more likely to be
                # excluded). Guard the divisor: an exclusion penalty of 0 means "no
                # exclusion weighting", so recent participants get no reduction.
                if p.name in recent_match_names and self.exclusion_penalty_weight:
                    return base_count - (self.recent_match_penalty_weight / self.exclusion_penalty_weight)
                return base_count

            # Exclude the players with the lowest effective counts; break ties
            # by rating ascending so lower-rated players sit out first.
            exclusion_order = sorted(
                sorted_players,
                key=lambda p: (
                    effective_exclusion_count(p),
                    p.get_value(self.use_glicko, use_openskill=self.use_openskill, use_jopacoin=self.use_jopacoin),
                ),
            )
            excluded = exclusion_order[: len(players) - 10]
            excluded_ids = {id(p) for p in excluded}
            # sorted_players is already rating-descending, so filtering keeps
            # the selected players sorted by rating for the snake draft.
            selected = [p for p in sorted_players if id(p) not in excluded_ids]
        else:
            selected = sorted_players
            excluded = []

        # Snake draft: alternate picks to balance teams
        team1_players: list[Player] = []
        team2_players: list[Player] = []
        for i, player in enumerate(selected):
            # Snake pattern: 0->T1, 1->T2, 2->T2, 3->T1, 4->T1, 5->T2, ...
            round_num = i // 2
            if round_num % 2 == 0:
                # Even rounds: T1, T2
                if i % 2 == 0:
                    team1_players.append(player)
                else:
                    team2_players.append(player)
            else:
                # Odd rounds: T2, T1
                if i % 2 == 0:
                    team2_players.append(player)
                else:
                    team1_players.append(player)

        # Optimize role assignments for the greedy teams
        team1, team2, base_score = self._optimize_role_assignments_for_matchup(
            team1_players, team2_players, max_assignments_per_team=3, avoids=avoids, deals=deals        )

        # Add exclusion penalty
        exclusion_penalty = (
            sum(exclusion_counts.get(p.name, 0) for p in excluded)
            * self.exclusion_penalty_weight
        )

        # Add recent match penalty for selected players
        recent_penalty = 0.0
        if recent_match_names:
            selected_names = {p.name for p in team1.players + team2.players}
            recent_in_match = len(selected_names & recent_match_names)
            recent_penalty = recent_in_match * self.recent_match_penalty_weight

        # Add split penalty for package deals (one selected, one excluded)
        selected_discord_ids = {p.discord_id for p in team1.players + team2.players if p.discord_id}
        excluded_discord_ids = {p.discord_id for p in excluded if p.discord_id}
        deal_split_penalty = self._calculate_package_deal_split_penalty(
            selected_discord_ids, excluded_discord_ids, deals
        )

        # Rating spread penalty: incentivize selecting players of closer skill
        selected_values = [
            p.get_value(self.use_glicko, use_openskill=self.use_openskill, use_jopacoin=self.use_jopacoin)
            for p in team1.players + team2.players
        ]
        rating_spread_penalty = self._calculate_rating_spread_penalty(selected_values)
        lobby_rating_bonus = self._calculate_lobby_rating_bonus(selected_values)

        total_score = (
            base_score + exclusion_penalty + recent_penalty + deal_split_penalty
            + rating_spread_penalty - lobby_rating_bonus
        )

        return team1, team2, excluded, total_score


    def _get_cached_role_assignments(self, players: list[Player]) -> list[list[str]]:
        """
        Get optimal role assignments using service-layer caching.

        This provides performance optimization for repeated shuffles with
        the same player combinations, while keeping the domain layer pure.

        Args:
            players: List of 5 players

        Returns:
            List of optimal role assignment permutations
        """
        player_roles_key = tuple(
            tuple(p.preferred_roles) if p.preferred_roles else () for p in players
        )
        cached_result = get_cached_role_assignments(player_roles_key)
        return [list(assignment) for assignment in cached_result]

    def _optimize_role_assignments_for_matchup(
        self,
        team1_players: list[Player],
        team2_players: list[Player],
        max_assignments_per_team: int = 20,
        avoids: list | None = None,
        deals: list | None = None,

    ) -> tuple[Team, Team, float]:
        """
        Find optimal role assignments for two teams that minimize total score.

        Tries all combinations of valid role assignments (with minimum off-role count),
        but limits the search space to avoid combinatorial explosion.

        Args:
            team1_players: Players for team 1
            team2_players: Players for team 2
            max_assignments_per_team: Maximum number of role assignments to try per team
            avoids: Optional list of SoftAvoid objects to apply same-team penalties
            deals: Optional list of PackageDeal objects to apply different-team penalties


        Returns:
            Tuple of (best_team1, best_team2, best_score)
        """
        # Get all optimal role assignments for each team (with caching, limited)
        team1_assignments = self._get_cached_role_assignments(team1_players)[:max_assignments_per_team]
        team2_assignments = self._get_cached_role_assignments(team2_players)[:max_assignments_per_team]

        best_team1 = None
        best_team2 = None
        best_score = float("inf")

        # Pre-compute team IDs for penalty calculations (only once per call)
        team1_ids = {p.discord_id for p in team1_players if p.discord_id is not None}
        team2_ids = {p.discord_id for p in team2_players if p.discord_id is not None}
        avoid_penalty = self._calculate_soft_avoid_penalty(team1_ids, team2_ids, avoids)
        deal_penalty = self._calculate_package_deal_penalty(team1_ids, team2_ids, deals)
        region_penalty = self._calculate_region_split_penalty(team1_players, team2_players)

        # Try all combinations of valid role assignments
        for t1_roles in team1_assignments:
            for t2_roles in team2_assignments:
                team1 = Team(team1_players, role_assignments=t1_roles)
                team2 = Team(team2_players, role_assignments=t2_roles)

                team1_value = team1.get_team_value(
                    self.use_glicko, self.off_role_multiplier, use_openskill=self.use_openskill, use_jopacoin=self.use_jopacoin
                )
                team2_value = team2.get_team_value(
                    self.use_glicko, self.off_role_multiplier, use_openskill=self.use_openskill, use_jopacoin=self.use_jopacoin
                )
                value_diff = abs(team1_value - team2_value)

                team1_off_roles = team1.get_off_role_count()
                team2_off_roles = team2.get_off_role_count()
                off_role_penalty = (team1_off_roles + team2_off_roles) * self.off_role_flat_penalty

                role_matchup_delta = self._calculate_role_matchup_delta(team1, team2)

                weighted_role_delta = role_matchup_delta * self.role_matchup_delta_weight
                rd_priority = self._calculate_rd_priority(team1_players + team2_players)
                total_score = (
                    value_diff + off_role_penalty + weighted_role_delta - rd_priority
                    + avoid_penalty + deal_penalty + region_penalty
                )

                if total_score < best_score:
                    best_score = total_score
                    best_team1 = team1
                    best_team2 = team2

        # Fallback to default if no assignments found
        if best_team1 is None:
            best_team1 = Team(team1_players)
            best_team2 = Team(team2_players)
            best_team1.ensure_role_assignments()
            best_team2.ensure_role_assignments()
            best_score = float("inf")

        return best_team1, best_team2, best_score

    def _log_top_matchups(
        self,
        entries: list[tuple],
        header: str,
        show_exclusions: bool,
    ) -> None:
        """
        Log the top-K candidate matchups (shared by ``shuffle`` and ``shuffle_from_pool``).

        Args:
            entries: List of matchup tuples, each
                (score, value_diff, off_penalty, role_delta, excl_penalty,
                 t1_val, t2_val, t1_off, t2_off, excluded, t1, t2),
                sorted ascending by score.
            header: Heading line printed above the matchups.
            show_exclusions: Whether to include exclusion penalty / excluded players.
        """
        if not entries:
            return

        logger.info("=" * 60)
        logger.info(header)
        for i, (
            score,
            value_diff,
            off_penalty,
            role_delta,
            excl_penalty,
            t1_val,
            t2_val,
            t1_off,
            t2_off,
            excluded,
            t1,
            t2,
        ) in enumerate(entries, 1):
            t1_roles = t1.role_assignments if t1.role_assignments else t1._assign_roles_optimally()
            t2_roles = t2.role_assignments if t2.role_assignments else t2._assign_roles_optimally()

            if show_exclusions:
                logger.info(
                    f"\n#{i} - Total Score: {score:.1f} (Value Diff: {value_diff:.1f}, Off-Role Penalty: {off_penalty:.1f}, "
                    f"Role Matchup Delta: {role_delta:.1f}, Exclusion Penalty: {excl_penalty:.1f})"
                )
            else:
                logger.info(
                    f"\n#{i} - Total Score: {score:.1f} (Value Diff: {value_diff:.1f}, Off-Role Penalty: {off_penalty:.1f}, Role Matchup Delta: {role_delta:.1f})"
                )
            logger.info(
                f"  Team 1 Value: {t1_val:.1f} | Team 2 Value: {t2_val:.1f} | Diff: {abs(t1_val - t2_val):.1f}"
            )
            logger.info(f"  Off-Roles: Team1={t1_off}, Team2={t2_off} (Total: {t1_off + t2_off})")
            if show_exclusions:
                logger.info(f"  Excluded: {', '.join(excluded) if excluded else 'None'}")
            logger.info(
                f"  Team 1: {', '.join([f'{p.name}({role})' for p, role in zip(t1.players, t1_roles)])}"
            )
            logger.info(
                f"  Team 2: {', '.join([f'{p.name}({role})' for p, role in zip(t2.players, t2_roles)])}"
            )
        logger.info("=" * 60)

    def shuffle(self, players: list[Player], avoids: list | None = None, deals: list | None = None) -> tuple[Team, Team]:
        """
        Shuffle players into two balanced teams.

        Args:
            players: List of exactly 10 players
            avoids: Optional list of SoftAvoid objects to apply same-team penalties
            deals: Optional list of PackageDeal objects to apply different-team penalties


        Returns:
            Tuple of (Team1, Team2)
        """
        if len(players) != 10:
            raise ValueError(f"Need exactly 10 players, got {len(players)}")

        # Generate all possible team combinations
        # We only need to generate combinations for one team (the other is the complement)
        best_teams = None
        best_score = float("inf")

        # Track all matchups with the best score for random tie-breaking
        best_matchups = []  # List of (team1, team2, value_diff, off_roles)

        # Track top matchups for logging (deduplicate by team composition, not order)
        top_matchups = []  # List of (score, value_diff, off_role_penalty, team1, team2)
        seen_matchups = set()  # Track unique matchups (frozenset of player names)

        for team1_indices in itertools.combinations(range(10), 5):
            team1_players = [players[i] for i in team1_indices]
            team2_players = [players[i] for i in range(10) if i not in team1_indices]

            # Create canonical matchup key (order doesn't matter)
            team1_names = frozenset(p.name for p in team1_players)
            team2_names = frozenset(p.name for p in team2_players)
            matchup_key = frozenset([team1_names, team2_names])

            # Skip if we've seen this matchup before (swapped teams)
            if matchup_key in seen_matchups:
                continue
            seen_matchups.add(matchup_key)

            # Optimize role assignments for this matchup
            team1, team2, total_score = self._optimize_role_assignments_for_matchup(
                team1_players, team2_players, avoids=avoids, deals=deals            )

            team1_value = team1.get_team_value(
                self.use_glicko, self.off_role_multiplier, use_openskill=self.use_openskill, use_jopacoin=self.use_jopacoin
            )
            team2_value = team2.get_team_value(
                self.use_glicko, self.off_role_multiplier, use_openskill=self.use_openskill, use_jopacoin=self.use_jopacoin
            )
            value_diff = abs(team1_value - team2_value)
            team1_off_roles = team1.get_off_role_count()
            team2_off_roles = team2.get_off_role_count()
            off_role_penalty = (team1_off_roles + team2_off_roles) * self.off_role_flat_penalty
            role_matchup_delta = self._calculate_role_matchup_delta(team1, team2)
            total_off_roles = team1_off_roles + team2_off_roles

            # Track this matchup (normalized 12-tuple shape shared with shuffle_from_pool;
            # exclusion penalty / excluded players are unused for a fixed 10-player shuffle)
            top_matchups.append(
                (
                    total_score,
                    value_diff,
                    off_role_penalty,
                    role_matchup_delta,
                    0.0,
                    team1_value,
                    team2_value,
                    team1_off_roles,
                    team2_off_roles,
                    [],
                    team1,
                    team2,
                )
            )

            # Non-deterministic tie-breaking: collect all matchups with the best score
            if total_score < best_score:
                best_score = total_score
                best_matchups = [(team1, team2, value_diff, total_off_roles)]

                # Early termination: if perfect match found (score = 0), stop searching
                if total_score == 0:
                    logger.info("Early termination: Perfect match found (score=0)")
                    break
            elif total_score == best_score:
                best_matchups.append((team1, team2, value_diff, total_off_roles))

        # Randomly select from all matchups with the best score
        if best_matchups:
            best_teams = random.choice(best_matchups)[:2]  # Just get (team1, team2)
        else:
            # Should be unreachable for 10 valid players; raise rather than
            # silently return None into a tuple-unpack at the call site.
            raise RuntimeError("Shuffler produced no candidate matchup")

        # Log top 5 matchups
        top_matchups.sort(key=lambda x: x[0])
        self._log_top_matchups(
            top_matchups[:5],
            "TOP 5 MATCHUPS (10 players):",
            show_exclusions=False,
        )
        logger.info(f"SELECTED: Matchup #1 with score {top_matchups[0][0]:.1f}")

        return best_teams

    @staticmethod
    def _sample_player_combinations(
        rng: random.Random, n: int, k: int, max_samples: int
    ) -> Iterable[tuple[int, ...]]:
        """
        Yield up to max_samples unique k-combinations from range(n).

        Uses the supplied RNG to sample; caps total attempts to avoid pathological
        loops when nCk is not much larger than max_samples.
        """
        if max_samples <= 0:
            return
        seen = set()
        attempts_left = max_samples * 25
        while len(seen) < max_samples and attempts_left > 0:
            attempts_left -= 1
            combo = tuple(sorted(rng.sample(range(n), k)))
            if combo in seen:
                continue
            seen.add(combo)
            yield combo

    def _evaluate_pool_matchup(
        self,
        team1_players: list[Player],
        team2_players: list[Player],
        combo_penalty: float,
        exclusion_penalty: float,
        excluded_names: list[str],
        recent_match_names: set[str],
        max_assignments_per_team: int,
        avoids: list | None,
        deals: list | None,
    ) -> _PoolMatchup:
        """
        Optimize role assignments for one team split and score it.

        ``combo_penalty`` is the combination-wide exclusions, deal split,
        rating spread, and lobby rating adjustment. The recent-match penalty
        depends on the split and is added here.
        """
        team1, team2, base_score = self._optimize_role_assignments_for_matchup(
            team1_players,
            team2_players,
            max_assignments_per_team=max_assignments_per_team,
            avoids=avoids,
            deals=deals,
        )

        # base_score includes: value_diff + off_role_penalty + role_matchup_delta - rd_priority + avoid_penalty + deal_penalty
        # We need to add exclusion_penalty, recent_match_penalty, deal_split_penalty, and rating_spread_penalty
        selected_player_names = {p.name for p in team1_players + team2_players}
        recent_in_match = len(selected_player_names & recent_match_names)
        recent_penalty = recent_in_match * self.recent_match_penalty_weight

        total_score = base_score + combo_penalty + recent_penalty

        # Extract components for logging
        team1_value = team1.get_team_value(
            self.use_glicko, self.off_role_multiplier, use_openskill=self.use_openskill, use_jopacoin=self.use_jopacoin
        )
        team2_value = team2.get_team_value(
            self.use_glicko, self.off_role_multiplier, use_openskill=self.use_openskill, use_jopacoin=self.use_jopacoin
        )
        value_diff = abs(team1_value - team2_value)
        team1_off_roles = team1.get_off_role_count()
        team2_off_roles = team2.get_off_role_count()
        off_role_penalty = (team1_off_roles + team2_off_roles) * self.off_role_flat_penalty
        role_matchup_delta = self._calculate_role_matchup_delta(team1, team2)
        total_off_roles = team1_off_roles + team2_off_roles

        log_entry = (
            total_score,
            value_diff,
            off_role_penalty,
            role_matchup_delta,
            exclusion_penalty,
            team1_value,
            team2_value,
            team1_off_roles,
            team2_off_roles,
            excluded_names,
            team1,
            team2,
        )

        return _PoolMatchup(
            team1=team1,
            team2=team2,
            total_score=total_score,
            value_diff=value_diff,
            total_off_roles=total_off_roles,
            log_entry=log_entry,
        )

    @staticmethod
    def _pool_matchup_is_better(
        candidate: tuple[float, float, int, tuple],
        best: tuple[float, float, float, tuple | None],
    ) -> bool:
        """
        Deterministic comparison for pool-shuffle candidate selection.

        Minimizes (score, value_diff, total_off_roles), breaking remaining ties
        lexicographically by the candidate's name signature so results are stable.

        Args:
            candidate: (score, value_diff, total_off_roles, signature)
            best: (best_score, best_value_diff, best_total_off_roles, best_signature)
        """
        score, value_diff, total_off_roles, signature = candidate
        best_score, best_value_diff, best_total_off_roles, best_signature = best
        return (
            score < best_score
            or (score == best_score and value_diff < best_value_diff)
            or (
                score == best_score
                and value_diff == best_value_diff
                and total_off_roles < best_total_off_roles
            )
            or (
                score == best_score
                and value_diff == best_value_diff
                and total_off_roles == best_total_off_roles
                and (best_signature is None or signature < best_signature)
            )
        )

    @staticmethod
    def _pool_matchup_signature(
        team1: Team, team2: Team, excluded_names: list[str]
    ) -> tuple[tuple[str, ...], tuple[str, ...], tuple[str, ...]]:
        """Canonical (order-independent) name signature for a pool matchup."""
        team1_sig = tuple(sorted(p.name for p in team1.players))
        team2_sig = tuple(sorted(p.name for p in team2.players))
        excluded_sig = tuple(sorted(excluded_names))
        if team2_sig < team1_sig:
            team1_sig, team2_sig = team2_sig, team1_sig
        return (team1_sig, team2_sig, excluded_sig)

    def shuffle_from_pool(
        self,
        players: list[Player],
        exclusion_counts: dict[str, int] | None = None,
        recent_match_names: set[str] | None = None,
        avoids: list | None = None,
        deals: list | None = None,
        rng: random.Random | None = None,
    ) -> tuple[Team, Team, list[Player]]:
        """
        Shuffle players into two balanced teams when there are more than 10 players.

        Tries all combinations of 10 players from the pool and finds the best balanced teams.
        Considers exclusion counts to prioritize including players who have been excluded frequently.

        Args:
            players: List of players (can be 10 or more)
            exclusion_counts: Optional dict mapping player names to their exclusion counts.
                             Players with higher counts are prioritized for inclusion.
            recent_match_names: Optional set of player names who participated in the most recent match.
                               These players receive a penalty when selected, making them more likely
                               to sit out.
            avoids: Optional list of SoftAvoid objects to apply same-team penalties
            deals: Optional list of PackageDeal objects to apply different-team penalties


        Returns:
            Tuple of (Team1, Team2, excluded_players)
            excluded_players: List of players not included in the shuffle
        """
        # NOTE: This method can get expensive quickly. Keep this implementation
        # mindful of both CPU and memory (avoid storing every matchup for logging).
        if len(players) < 10:
            raise ValueError(f"Need at least 10 players, got {len(players)}")

        # Default to empty dict/set if not provided
        if exclusion_counts is None:
            exclusion_counts = {}
        if recent_match_names is None:
            recent_match_names = set()

        if len(players) == 10:
            # Just use the regular shuffle
            team1, team2 = self.shuffle(players, avoids=avoids, deals=deals)
            return team1, team2, []

        if len(players) == 14:
            # Use branch and bound for 14 players (optimized pruning)
            return self.shuffle_branch_bound(players, exclusion_counts, recent_match_names, avoids=avoids, deals=deals)

        # ---- Performance knobs (kept internal to preserve current public API) ----
        # Pool shuffles are far more expensive than 10-player shuffles. We therefore
        # intentionally reduce role-assignment exploration here.
        pool_max_assignments_per_team = 3  # 3x3=9 role combos per matchup (vs 20x20=400)
        log_top_k = 5

        # RNG for sampling/tie-breaking in pool shuffles. Callers that need
        # reproducibility (tests) can pass an explicit rng; otherwise we use a
        # fresh system-seeded RNG so consecutive shuffles of the same pool can
        # explore different candidate sets.
        pool_rng = rng if rng is not None else random.Random()

        # Try combinations of 10 players from the pool (possibly sampled for very large pools).
        best_teams: tuple[Team, Team] | None = None
        best_excluded: list[Player] | None = None
        best_score = float("inf")
        best_value_diff = float("inf")
        best_total_off_roles = float("inf")
        best_signature: tuple[tuple[str, ...], tuple[str, ...], tuple[str, ...]] | None = None

        # Track top-K matchups for logging (store only a small heap to avoid O(N) memory).
        # Keep a numeric tiebreaker so heapq never tries to compare Team objects.
        top_matchups_heap: list[tuple[float, int, tuple]] = []
        heap_tiebreaker = 0
        seen_matchups = set()  # Track unique matchups per player combination

        total_player_combinations = math.comb(len(players), 10)
        logger.info(
            f"Evaluating {total_player_combinations} player combinations from pool of {len(players)}"
        )

        # For very large pools, sampling keeps runtime reasonable.
        # Keep the threshold conservative to maintain quality for small/medium pools.
        max_player_combinations = 2500
        if total_player_combinations > max_player_combinations:
            selected_indices_iter = self._sample_player_combinations(
                pool_rng, len(players), 10, max_player_combinations
            )
            logger.info(
                f"Sampling {max_player_combinations} of {total_player_combinations} player combinations"
            )
        else:
            selected_indices_iter = itertools.combinations(range(len(players)), 10)

        # Generate all (or sampled) ways to choose 10 players from the pool
        for selected_indices in selected_indices_iter:
            selected_players = [players[i] for i in selected_indices]
            excluded_players = [
                players[i] for i in range(len(players)) if i not in selected_indices
            ]
            excluded_names = [p.name for p in excluded_players]

            # Create a key for this player combination to track seen matchups
            selected_names = frozenset(p.name for p in selected_players)

            exclusion_penalty = (
                sum(exclusion_counts.get(name, 0) for name in excluded_names)
                * self.exclusion_penalty_weight
            )

            # Calculate split penalty for package deals (one selected, one excluded)
            selected_discord_ids = {p.discord_id for p in selected_players if p.discord_id}
            excluded_discord_ids = {p.discord_id for p in excluded_players if p.discord_id}
            deal_split_penalty = self._calculate_package_deal_split_penalty(
                selected_discord_ids, excluded_discord_ids, deals
            )

            # Rating spread penalty: incentivize selecting players of closer skill
            selected_values = [
                p.get_value(self.use_glicko, use_openskill=self.use_openskill, use_jopacoin=self.use_jopacoin)
                for p in selected_players
            ]
            rating_spread_penalty = self._calculate_rating_spread_penalty(selected_values)
            lobby_rating_bonus = self._calculate_lobby_rating_bonus(selected_values)

            # Penalties that are constant across every team split of this combination.
            combo_penalty = (
                exclusion_penalty + deal_split_penalty + rating_spread_penalty
                - lobby_rating_bonus
            )

            # For this combination of 10, try all ways to split into teams
            for team1_indices in itertools.combinations(range(10), 5):
                team1_players = [selected_players[i] for i in team1_indices]
                team2_players = [selected_players[i] for i in range(10) if i not in team1_indices]

                # Create canonical matchup key (order doesn't matter)
                team1_names = frozenset(p.name for p in team1_players)
                team2_names = frozenset(p.name for p in team2_players)
                matchup_key = (selected_names, frozenset([team1_names, team2_names]))

                # Skip if we've seen this matchup before (swapped teams)
                if matchup_key in seen_matchups:
                    continue
                seen_matchups.add(matchup_key)

                matchup = self._evaluate_pool_matchup(
                    team1_players,
                    team2_players,
                    combo_penalty,
                    exclusion_penalty,
                    excluded_names,
                    recent_match_names,
                    pool_max_assignments_per_team,
                    avoids,
                    deals,
                )

                # Track top-K only (avoid storing all matchups).
                if log_top_k > 0:
                    if len(top_matchups_heap) < log_top_k:
                        heap_tiebreaker += 1
                        heapq.heappush(
                            top_matchups_heap,
                            (-matchup.total_score, heap_tiebreaker, matchup.log_entry),
                        )
                    else:
                        worst_score = -top_matchups_heap[0][0]
                        if matchup.total_score < worst_score:
                            heap_tiebreaker += 1
                            heapq.heapreplace(
                                top_matchups_heap,
                                (-matchup.total_score, heap_tiebreaker, matchup.log_entry),
                            )

                # Deterministic best selection to avoid flaky tests.
                signature = self._pool_matchup_signature(
                    matchup.team1, matchup.team2, excluded_names
                )
                is_better = self._pool_matchup_is_better(
                    (matchup.total_score, matchup.value_diff, matchup.total_off_roles, signature),
                    (best_score, best_value_diff, best_total_off_roles, best_signature),
                )
                if is_better:
                    best_score = matchup.total_score
                    best_value_diff = matchup.value_diff
                    best_total_off_roles = matchup.total_off_roles
                    best_signature = signature
                    best_teams = (matchup.team1, matchup.team2)
                    best_excluded = excluded_players

        # Log top 5 matchups
        if logger.isEnabledFor(logging.INFO) and top_matchups_heap:
            top_entries = [entry for _neg, _tb, entry in top_matchups_heap]
            top_entries.sort(key=lambda x: x[0])
            self._log_top_matchups(
                top_entries[:log_top_k],
                f"TOP {min(log_top_k, len(top_entries))} MATCHUPS (from pool of {len(players)} players):",
                show_exclusions=True,
            )

        if best_teams is None or best_excluded is None:
            raise RuntimeError("Failed to compute teams from pool shuffle (no matchups evaluated)")

        return best_teams[0], best_teams[1], best_excluded

    def shuffle_branch_bound(
        self,
        players: list[Player],
        exclusion_counts: dict[str, int] | None = None,
        recent_match_names: set[str] | None = None,
        avoids: list | None = None,
        deals: list | None = None,

    ) -> tuple[Team, Team, list[Player]]:
        """
        Branch and bound shuffle optimized for 14 players.

        Uses pruning to avoid evaluating team combinations that cannot beat
        the current best score. Provides significant speedup for large player pools.

        Args:
            players: List of exactly 14 players
            exclusion_counts: Optional dict mapping player names to exclusion counts
            recent_match_names: Optional set of player names who participated in the most recent match
            avoids: Optional list of SoftAvoid objects to apply same-team penalties
            deals: Optional list of PackageDeal objects to apply different-team penalties


        Returns:
            Tuple of (Team1, Team2, excluded_players)
        """
        if len(players) != 14:
            raise ValueError(f"Branch and bound shuffle requires exactly 14 players, got {len(players)}")

        exclusion_counts = exclusion_counts or {}
        recent_match_names = recent_match_names or set()

        # Step 1: Get greedy initial upper bound
        greedy_t1, greedy_t2, greedy_excluded, best_score = self._greedy_shuffle(
            players, exclusion_counts, recent_match_names, avoids=avoids, deals=deals        )
        best_result: tuple[Team, Team, list[Player]] = (greedy_t1, greedy_t2, greedy_excluded)

        logger.info(f"Branch & bound: greedy upper bound = {best_score:.1f}")

        # Precompute player values for fast lower bound calculations
        player_values = {
            p.name: p.get_value(self.use_glicko, use_openskill=self.use_openskill, use_jopacoin=self.use_jopacoin)
            for p in players
        }

        # Track pruning statistics
        pruned_player_selections = 0
        pruned_team_splits = 0
        evaluated_matchups = 0

        # Step 2: Iterate through all ways to select 10 players from 14
        # C(14,10) = C(14,4) = 1001 combinations
        for selected_indices in itertools.combinations(range(14), 10):
            selected_players = [players[i] for i in selected_indices]
            excluded_players = [players[i] for i in range(14) if i not in selected_indices]
            excluded_names = [p.name for p in excluded_players]

            # Compute exclusion penalty for this selection
            exclusion_penalty = (
                sum(exclusion_counts.get(name, 0) for name in excluded_names)
                * self.exclusion_penalty_weight
            )

            # Calculate split penalty for package deals (one selected, one excluded)
            selected_discord_ids = {p.discord_id for p in selected_players if p.discord_id}
            excluded_discord_ids = {p.discord_id for p in excluded_players if p.discord_id}
            deal_split_penalty = self._calculate_package_deal_split_penalty(
                selected_discord_ids, excluded_discord_ids, deals
            )

            # Rating spread penalty: incentivize selecting players of closer skill
            selected_value_list = [player_values[p.name] for p in selected_players]
            rating_spread_penalty = self._calculate_rating_spread_penalty(selected_value_list)
            lobby_rating_bonus = self._calculate_lobby_rating_bonus(selected_value_list)
            combo_penalty = (
                exclusion_penalty + deal_split_penalty + rating_spread_penalty
                - lobby_rating_bonus
            )

            # Quick pruning: if the combination-wide score exceeds best score, skip
            if combo_penalty >= best_score:
                pruned_player_selections += 1
                continue

            # Step 3: Iterate through team splits with pruning
            # We use combinations to avoid duplicates (T1={A,B,C,D,E} vs T2={F,G,H,I,J}
            # is same as T1={F,G,H,I,J} vs T2={A,B,C,D,E})
            seen_splits = set()

            for team1_indices in itertools.combinations(range(10), 5):
                team1_players = [selected_players[i] for i in team1_indices]
                team2_indices = [i for i in range(10) if i not in team1_indices]
                team2_players = [selected_players[i] for i in team2_indices]

                # Canonical key to avoid duplicate splits
                t1_key = frozenset(p.name for p in team1_players)
                t2_key = frozenset(p.name for p in team2_players)
                split_key = frozenset([t1_key, t2_key])
                if split_key in seen_splits:
                    continue
                seen_splits.add(split_key)

                # Compute value difference lower bound (before role optimization)
                t1_sum = sum(player_values[p.name] for p in team1_players)
                t2_sum = sum(player_values[p.name] for p in team2_players)
                value_diff_lb = abs(t1_sum - t2_sum)

                # Compute recent match penalty for selected players
                selected_player_names = {p.name for p in team1_players + team2_players}
                recent_in_match = len(selected_player_names & recent_match_names)
                recent_penalty = recent_in_match * self.recent_match_penalty_weight

                # Quick lower bound using the combination-wide adjustment.
                lower_bound = value_diff_lb + recent_penalty + combo_penalty

                # Prune if lower bound >= best score
                if lower_bound >= best_score:
                    pruned_team_splits += 1
                    continue

                # Step 4: Full role optimization (only for promising splits)
                evaluated_matchups += 1
                team1, team2, base_score = self._optimize_role_assignments_for_matchup(
                    team1_players, team2_players, max_assignments_per_team=3, avoids=avoids, deals=deals                )

                total_score = base_score + recent_penalty + combo_penalty

                if total_score < best_score:
                    best_score = total_score
                    best_result = (team1, team2, excluded_players)

                    # Early termination on a true perfect match. We compare
                    # against ``== 0`` rather than ``<= 0`` because the
                    # rd_priority bonus subtracted inside _optimize can drive
                    # the score below zero on a non-perfect matchup, which
                    # would otherwise trip the search to terminate early.
                    if best_score == 0:
                        logger.info("Branch & bound: perfect match found, early termination")
                        break

            # Check for early termination after inner loop (same reasoning).
            if best_score == 0:
                break

        logger.info(
            f"Branch & bound stats: pruned {pruned_player_selections} player selections, "
            f"{pruned_team_splits} team splits, evaluated {evaluated_matchups} matchups"
        )
        logger.info(f"Branch & bound: final score = {best_score:.1f}")

        return best_result

    def _score_draft_pool(
        self,
        captain_a: Player,
        captain_b: Player,
        pool: list[Player],
        max_assignments_per_team: int = 3,
    ) -> float:
        """
        Score a candidate draft pool by evaluating all possible team splits.

        Tries all C(8,4) = 70 ways to divide the 8 non-captain players across
        the two captains, and returns the best (lowest) score — the most balanced
        game this pool can produce.

        Args:
            captain_a: First captain
            captain_b: Second captain
            pool: 8 non-captain players
            max_assignments_per_team: Max role assignments to try per team

        Returns:
            Best (lowest) score across all splits.
        """
        best_score = float("inf")

        for team_a_indices in itertools.combinations(range(len(pool)), 4):
            team_a_players = [captain_a] + [pool[i] for i in team_a_indices]
            team_b_players = [captain_b] + [pool[i] for i in range(len(pool)) if i not in team_a_indices]

            _, _, score = self._optimize_role_assignments_for_matchup(
                team_a_players, team_b_players, max_assignments_per_team=max_assignments_per_team
            )

            if score < best_score:
                best_score = score

        return best_score

    def _score_full_pool(
        self,
        captain_a: Player,
        captain_b: Player,
        pool: list[Player],
        excluded: list[Player],
        exclusion_counts: dict[str, int],
        recent_match_names: set[str],
    ) -> float:
        """
        Compute full pool score including split score and penalties.

        Args:
            captain_a: First captain
            captain_b: Second captain
            pool: 8 non-captain players in the pool
            excluded: Players excluded from the pool
            exclusion_counts: Dict mapping player names to exclusion counts
            recent_match_names: Set of player names from most recent match

        Returns:
            Full pool score (lower is better)
        """
        best_split_score = self._score_draft_pool(captain_a, captain_b, pool)

        # Exclusion penalty: penalize excluding frequently-excluded players
        exclusion_penalty = (
            sum(exclusion_counts.get(p.name, 0) for p in excluded)
            * self.exclusion_penalty_weight
        )

        # Recent match penalty for selected players
        selected_names = {p.name for p in pool}
        recent_penalty = len(selected_names & recent_match_names) * self.recent_match_penalty_weight

        return best_split_score + exclusion_penalty + recent_penalty

    def select_draft_pool_beam(
        self,
        captain_a: Player,
        captain_b: Player,
        candidates: list[Player],
        exclusion_counts: dict[str, int] | None = None,
        recent_match_names: set[str] | None = None,
    ) -> DraftPoolResult:
        """
        Select 8-player pool using beam search for faster performance.

        Uses local search with single-player swaps to find a good pool without
        exhaustive enumeration. Significantly faster for large candidate pools
        while typically finding solutions within 5% of optimal.

        Parameters (hardcoded for consistency):
            ITERATIONS: 35 - max search iterations
            BEAM_WIDTH: 8 - number of candidate pools to track
            EARLY_EXIT_THRESHOLD: 150 - exit early if score below this
            STAGNATION_LIMIT: 5 - stop early once the best score plateaus

        Args:
            captain_a: First captain
            captain_b: Second captain
            candidates: Non-captain lobby players (>8)
            exclusion_counts: Dict mapping player names to exclusion counts
            recent_match_names: Set of player names from most recent match

        Returns:
            DraftPoolResult with selected/excluded players and score
        """
        ITERATIONS = 35
        BEAM_WIDTH = 8
        EARLY_EXIT_THRESHOLD = 150.0

        if len(candidates) < 8:
            raise ValueError(f"Need at least 8 candidates, got {len(candidates)}")

        exclusion_counts = exclusion_counts or {}
        recent_match_names = recent_match_names or set()

        # Greedy initial pool: sort by rating (descending) and take top 8
        sorted_candidates = sorted(
            candidates,
            key=lambda p: p.get_value(self.use_glicko, use_openskill=self.use_openskill, use_jopacoin=self.use_jopacoin),
            reverse=True,
        )
        initial_pool = sorted_candidates[:8]
        initial_excluded = sorted_candidates[8:]

        initial_score = self._score_full_pool(
            captain_a, captain_b, initial_pool, initial_excluded,
            exclusion_counts, recent_match_names
        )

        # Early exit if initial pool is already excellent
        if initial_score < EARLY_EXIT_THRESHOLD:
            logger.info(f"Beam search: early exit with initial pool, score={initial_score:.1f}")
            return DraftPoolResult(
                selected_players=list(initial_pool),
                excluded_players=list(initial_excluded),
                pool_score=initial_score,
            )

        # Beam search: track best BEAM_WIDTH pools at each iteration
        # Each beam entry is (pool_set_frozenset, pool_list, score)
        initial_pool_set = frozenset(p.name for p in initial_pool)
        current_beams = [(initial_pool_set, initial_pool, initial_score)]
        best_pool = initial_pool
        best_excluded = initial_excluded
        best_score = initial_score

        # Memoize pool scores across iterations: without this the same
        # candidate pool is re-scored every time it reappears as a neighbour,
        # which is the dominant cost (_score_full_pool is ~3ms). Also stop once
        # the best score stops improving instead of always burning ITERATIONS.
        score_cache: dict[frozenset[str], float] = {initial_pool_set: initial_score}
        STAGNATION_LIMIT = 5
        stagnant_iterations = 0

        for iteration in range(ITERATIONS):
            neighbors = []
            seen_pools = set()

            for pool_set, pool, _ in current_beams:
                # Get players outside this pool
                outside = [c for c in candidates if c.name not in pool_set]

                # Try all single-player swaps: remove one from pool, add one from outside
                for i, _ in enumerate(pool):
                    for in_player in outside:
                        # Create new pool with swap
                        new_pool = pool[:i] + [in_player] + pool[i + 1:]
                        new_pool_set = frozenset(p.name for p in new_pool)

                        # Skip if we've already seen this pool this iteration
                        if new_pool_set in seen_pools:
                            continue
                        seen_pools.add(new_pool_set)

                        new_excluded = [c for c in candidates if c.name not in new_pool_set]

                        # Reuse the score if this pool was evaluated in an
                        # earlier iteration; pay _score_full_pool only once.
                        score = score_cache.get(new_pool_set)
                        if score is None:
                            score = self._score_full_pool(
                                captain_a, captain_b, new_pool, new_excluded,
                                exclusion_counts, recent_match_names
                            )
                            score_cache[new_pool_set] = score

                        neighbors.append((new_pool_set, new_pool, score))

                        # Early exit if we find an excellent pool
                        if score < EARLY_EXIT_THRESHOLD:
                            logger.info(
                                f"Beam search: early exit at iteration {iteration + 1}, "
                                f"score={score:.1f}"
                            )
                            return DraftPoolResult(
                                selected_players=list(new_pool),
                                excluded_players=list(new_excluded),
                                pool_score=score,
                            )

            if not neighbors:
                # No new neighbors to explore (shouldn't happen normally)
                break

            # Sort by score and keep top BEAM_WIDTH
            neighbors.sort(key=lambda x: x[2])
            current_beams = neighbors[:BEAM_WIDTH]

            # Update best if improved; otherwise count toward convergence.
            if current_beams[0][2] < best_score:
                best_pool = current_beams[0][1]
                best_score = current_beams[0][2]
                best_excluded = [c for c in candidates if c.name not in current_beams[0][0]]
                stagnant_iterations = 0
            else:
                stagnant_iterations += 1
                if stagnant_iterations >= STAGNATION_LIMIT:
                    break

        logger.info(
            f"Beam search: finished after {iteration + 1} iterations, "
            f"best score={best_score:.1f}"
        )

        return DraftPoolResult(
            selected_players=list(best_pool),
            excluded_players=list(best_excluded),
            pool_score=best_score,
        )

    def select_draft_pool(
        self,
        captain_a: Player,
        captain_b: Player,
        candidates: list[Player],
        exclusion_counts: dict[str, int] | None = None,
        recent_match_names: set[str] | None = None,
    ) -> DraftPoolResult:
        """
        Select 8 non-captain players for draft such that snake-draft produces
        balanced teams regardless of who picks first.

        Uses exhaustive search for <=12 candidates, beam search for larger pools.

        Args:
            captain_a: First captain
            captain_b: Second captain
            candidates: Non-captain lobby players (8+, uses beam search for >12)
            exclusion_counts: Dict mapping player names to exclusion counts
            recent_match_names: Set of player names from most recent match

        Returns:
            DraftPoolResult with selected/excluded players and scores

        Raises:
            ValueError: If fewer than 8 candidates
        """
        if len(candidates) < 8:
            raise ValueError(
                f"Need at least 8 non-captain candidates, got {len(candidates)}"
            )

        exclusion_counts = exclusion_counts or {}
        recent_match_names = recent_match_names or set()

        if len(candidates) == 8:
            # Only one possible pool
            best_split_score = self._score_draft_pool(captain_a, captain_b, candidates)

            # Add recent match penalty
            selected_names = {p.name for p in candidates}
            recent_penalty = len(selected_names & recent_match_names) * self.recent_match_penalty_weight

            pool_score = best_split_score + recent_penalty

            return DraftPoolResult(
                selected_players=list(candidates),
                excluded_players=[],
                pool_score=pool_score,
            )

        # For larger pools (>12), use beam search instead of exhaustive enumeration
        # C(12,8) = 495 pools is manageable; C(13,8) = 1287; C(14,8) = 3003
        # Beam search is ~5% worse but much faster for large pools
        BEAM_SEARCH_THRESHOLD = 12
        if len(candidates) > BEAM_SEARCH_THRESHOLD:
            logger.info(
                f"Using beam search for {len(candidates)} candidates "
                f"(threshold: {BEAM_SEARCH_THRESHOLD})"
            )
            return self.select_draft_pool_beam(
                captain_a, captain_b, candidates,
                exclusion_counts, recent_match_names
            )

        # Enumerate all C(N, 8) pools and pick the best (for <=12 candidates)
        best_result: DraftPoolResult | None = None
        best_pool_score = float("inf")

        for pool_indices in itertools.combinations(range(len(candidates)), 8):
            pool = [candidates[i] for i in pool_indices]
            excluded = [candidates[i] for i in range(len(candidates)) if i not in pool_indices]

            best_split_score = self._score_draft_pool(captain_a, captain_b, pool)

            # Exclusion penalty: penalize excluding frequently-excluded players
            exclusion_penalty = (
                sum(exclusion_counts.get(p.name, 0) for p in excluded)
                * self.exclusion_penalty_weight
            )

            # Recent match penalty for selected players
            selected_names = {p.name for p in pool}
            recent_penalty = len(selected_names & recent_match_names) * self.recent_match_penalty_weight

            pool_score = best_split_score + exclusion_penalty + recent_penalty

            if pool_score < best_pool_score:
                best_pool_score = pool_score
                best_result = DraftPoolResult(
                    selected_players=list(pool),
                    excluded_players=list(excluded),
                    pool_score=pool_score,
                )

        logger.info(
            f"Draft pool selection (exhaustive): evaluated {math.comb(len(candidates), 8)} pools, "
            f"best score={best_pool_score:.1f}"
        )

        return best_result
