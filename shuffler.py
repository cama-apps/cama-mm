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

from config import RD_PRIORITY_WEIGHT, SHUFFLER_SETTINGS
from domain.models.player import Player
from domain.models.team import Team
from utils.role_assignment_cache import get_cached_role_assignments

logger = logging.getLogger("cama_bot.shuffler")


@dataclass
class DraftPoolResult:
    """Result of balanced draft pool selection."""

    selected_players: list[Player]  # 8 non-captain players
    excluded_players: list[Player]  # Players not selected
    pool_score: float  # Best achievable balance score + penalties


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
        recent_match_penalty_weight: float | None = None,
    ):
        """
        Initialize the shuffler.

        Args:
            use_glicko: Whether to use Glicko-2 ratings (default True)
            consider_roles: Whether to consider role distribution
            off_role_multiplier: Multiplier for MMR when playing off-role (default 0.95 = 95% effectiveness)
            off_role_flat_penalty: Flat penalty per off-role player added to team value difference (default 350)
            role_matchup_delta_weight: Weight applied to lane matchup delta when scoring teams
            exclusion_penalty_weight: Penalty per exclusion count for excluded players (default 50.0)
            use_openskill: Whether to use OpenSkill ratings instead of Glicko-2 (default False)
            recent_match_penalty_weight: Penalty per recent match participant selected (default 25.0)
        """
        self.use_glicko = use_glicko
        self.consider_roles = consider_roles
        self.use_openskill = use_openskill
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

    def _calculate_role_matchup_delta(self, team1: Team, team2: Team) -> float:
        """
        Calculate the maximum role matchup delta between two teams.

        Compares:
        - Team1 carry (1) vs Team2 offlane (3)
        - Team2 carry (1) vs Team1 offlane (3)
        - Team1 mid (2) vs Team2 mid (2)

        Args:
            team1: First team
            team2: Second team

        Returns:
            Maximum delta among the three critical matchups
        """
        # Get players and their effective values for each role
        team1_carry_player, team1_carry_value = team1.get_player_by_role(
            "1", self.use_glicko, self.off_role_multiplier, use_openskill=self.use_openskill
        )
        team1_offlane_player, team1_offlane_value = team1.get_player_by_role(
            "3", self.use_glicko, self.off_role_multiplier, use_openskill=self.use_openskill
        )
        team1_mid_player, team1_mid_value = team1.get_player_by_role(
            "2", self.use_glicko, self.off_role_multiplier, use_openskill=self.use_openskill
        )

        team2_carry_player, team2_carry_value = team2.get_player_by_role(
            "1", self.use_glicko, self.off_role_multiplier, use_openskill=self.use_openskill
        )
        team2_offlane_player, team2_offlane_value = team2.get_player_by_role(
            "3", self.use_glicko, self.off_role_multiplier, use_openskill=self.use_openskill
        )
        team2_mid_player, team2_mid_value = team2.get_player_by_role(
            "2", self.use_glicko, self.off_role_multiplier, use_openskill=self.use_openskill
        )

        # Calculate the three critical matchups
        carry_vs_offlane_1 = abs(team1_carry_value - team2_offlane_value)
        carry_vs_offlane_2 = abs(team2_carry_value - team1_offlane_value)
        mid_vs_mid = abs(team1_mid_value - team2_mid_value)

        # Return the maximum delta
        return max(carry_vs_offlane_1, carry_vs_offlane_2, mid_vs_mid)

    def _calculate_rd_priority(self, players: Iterable[Player]) -> float:
        """Compute the RD-based bonus for a group of players."""
        if self.rd_priority_weight <= 0:
            return 0.0
        rd_total = sum(p.glicko_rd or 0.0 for p in players)
        return rd_total * self.rd_priority_weight

    def _greedy_shuffle(
        self,
        players: list[Player],
        exclusion_counts: dict[str, int] | None = None,
        recent_match_names: set[str] | None = None,
    ) -> tuple[Team, Team, list[Player], float]:
        """
        Greedy snake-draft shuffle for initial upper bound in branch and bound.

        Alternates picking highest-rated players to balance team values.

        Args:
            players: List of players (10-14)
            exclusion_counts: Optional dict mapping player names to exclusion counts
            recent_match_names: Optional set of player names who participated in the most recent match

        Returns:
            Tuple of (team1, team2, excluded_players, score)
        """
        exclusion_counts = exclusion_counts or {}
        recent_match_names = recent_match_names or set()

        # Sort by rating descending
        sorted_players = sorted(
            players,
            key=lambda p: p.get_value(self.use_glicko, use_openskill=self.use_openskill),
            reverse=True,
        )

        # For >10 players, exclude those with lowest exclusion counts first (greedily)
        # Recent match participants get lower effective count (more likely to be excluded)
        if len(players) > 10:
            # Sort excluded candidates by effective exclusion count
            # Recent match participants have their count reduced to prefer excluding them
            def effective_exclusion_count(p: Player) -> float:
                base_count = exclusion_counts.get(p.name, 0)
                # Reduce count for recent participants (makes them more likely to be excluded)
                if p.name in recent_match_names:
                    return base_count - (self.recent_match_penalty_weight / self.exclusion_penalty_weight)
                return base_count

            excluded_candidates = sorted(
                sorted_players[10:],
                key=effective_exclusion_count,
            )
            selected = sorted_players[:10]
            excluded = list(excluded_candidates[: len(players) - 10])
            # Re-sort selected by rating
            selected = sorted(
                selected,
                key=lambda p: p.get_value(self.use_glicko, use_openskill=self.use_openskill),
                reverse=True,
            )
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
            team1_players, team2_players, max_assignments_per_team=3
        )

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

        total_score = base_score + exclusion_penalty + recent_penalty

        return team1, team2, excluded, total_score

    def _compute_value_diff_lower_bound(
        self,
        team1_sum: float,
        team2_sum: float,
        remaining_values: list[float],
        need_t1: int,
        need_t2: int,
    ) -> float:
        """
        Compute lower bound on achievable value difference.

        Given partial team sums and remaining player values to assign,
        compute the minimum possible final value difference.

        Args:
            team1_sum: Current sum of team1 values
            team2_sum: Current sum of team2 values
            remaining_values: Sorted (descending) values of unassigned players
            need_t1: Number of players still needed for team1
            need_t2: Number of players still needed for team2

        Returns:
            Lower bound on value difference
        """
        if not remaining_values:
            return abs(team1_sum - team2_sum)

        # Greedy optimal assignment: always give to the team that's behind
        t1, t2 = team1_sum, team2_sum
        n1, n2 = need_t1, need_t2

        for val in remaining_values:
            if n1 == 0:
                t2 += val
                n2 -= 1
            elif n2 == 0:
                t1 += val
                n1 -= 1
            elif t1 <= t2:
                t1 += val
                n1 -= 1
            else:
                t2 += val
                n2 -= 1

        return abs(t1 - t2)

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
    ) -> tuple[Team, Team, float]:
        """
        Find optimal role assignments for two teams that minimize total score.

        Tries all combinations of valid role assignments (with minimum off-role count),
        but limits the search space to avoid combinatorial explosion.

        Args:
            team1_players: Players for team 1
            team2_players: Players for team 2
            max_assignments_per_team: Maximum number of role assignments to try per team

        Returns:
            Tuple of (best_team1, best_team2, best_score)
        """
        # Get all optimal role assignments for each team (with caching, limited)
        team1_assignments = self._get_cached_role_assignments(team1_players)[:max_assignments_per_team]
        team2_assignments = self._get_cached_role_assignments(team2_players)[:max_assignments_per_team]

        best_team1 = None
        best_team2 = None
        best_score = float("inf")

        # Try all combinations of valid role assignments
        for t1_roles in team1_assignments:
            for t2_roles in team2_assignments:
                team1 = Team(team1_players, role_assignments=t1_roles)
                team2 = Team(team2_players, role_assignments=t2_roles)

                team1_value = team1.get_team_value(
                    self.use_glicko, self.off_role_multiplier, use_openskill=self.use_openskill
                )
                team2_value = team2.get_team_value(
                    self.use_glicko, self.off_role_multiplier, use_openskill=self.use_openskill
                )
                value_diff = abs(team1_value - team2_value)

                team1_off_roles = team1.get_off_role_count()
                team2_off_roles = team2.get_off_role_count()
                off_role_penalty = (team1_off_roles + team2_off_roles) * self.off_role_flat_penalty

                role_matchup_delta = self._calculate_role_matchup_delta(team1, team2)

                weighted_role_delta = role_matchup_delta * self.role_matchup_delta_weight
                rd_priority = self._calculate_rd_priority(team1_players + team2_players)
                total_score = value_diff + off_role_penalty + weighted_role_delta - rd_priority

                if total_score < best_score:
                    best_score = total_score
                    best_team1 = team1
                    best_team2 = team2

        # Fallback to default if no assignments found
        if best_team1 is None:
            best_team1 = Team(team1_players)
            best_team2 = Team(team2_players)
            best_score = float("inf")

        return best_team1, best_team2, best_score

    def shuffle(self, players: list[Player]) -> tuple[Team, Team]:
        """
        Shuffle players into two balanced teams.

        Args:
            players: List of exactly 10 players

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
                team1_players, team2_players
            )

            team1_value = team1.get_team_value(
                self.use_glicko, self.off_role_multiplier, use_openskill=self.use_openskill
            )
            team2_value = team2.get_team_value(
                self.use_glicko, self.off_role_multiplier, use_openskill=self.use_openskill
            )
            value_diff = abs(team1_value - team2_value)
            team1_off_roles = team1.get_off_role_count()
            team2_off_roles = team2.get_off_role_count()
            off_role_penalty = (team1_off_roles + team2_off_roles) * self.off_role_flat_penalty
            role_matchup_delta = self._calculate_role_matchup_delta(team1, team2)
            total_off_roles = team1_off_roles + team2_off_roles

            # Track this matchup
            top_matchups.append(
                (
                    total_score,
                    value_diff,
                    off_role_penalty,
                    role_matchup_delta,
                    team1_value,
                    team2_value,
                    team1_off_roles,
                    team2_off_roles,
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

        # Log top 5 matchups
        top_matchups.sort(key=lambda x: x[0])
        logger.info("=" * 60)
        logger.info("TOP 5 MATCHUPS (10 players):")
        for i, (
            score,
            value_diff,
            off_penalty,
            role_delta,
            t1_val,
            t2_val,
            t1_off,
            t2_off,
            t1,
            t2,
        ) in enumerate(top_matchups[:5], 1):
            # Get role assignments
            t1_roles = t1.role_assignments if t1.role_assignments else t1._assign_roles_optimally()
            t2_roles = t2.role_assignments if t2.role_assignments else t2._assign_roles_optimally()

            logger.info(
                f"\n#{i} - Total Score: {score:.1f} (Value Diff: {value_diff:.1f}, Off-Role Penalty: {off_penalty:.1f}, Role Matchup Delta: {role_delta:.1f})"
            )
            logger.info(
                f"  Team 1 Value: {t1_val:.1f} | Team 2 Value: {t2_val:.1f} | Diff: {abs(t1_val - t2_val):.1f}"
            )
            logger.info(f"  Off-Roles: Team1={t1_off}, Team2={t2_off} (Total: {t1_off + t2_off})")
            logger.info(
                f"  Team 1: {', '.join([f'{p.name}({role})' for p, role in zip(t1.players, t1_roles)])}"
            )
            logger.info(
                f"  Team 2: {', '.join([f'{p.name}({role})' for p, role in zip(t2.players, t2_roles)])}"
            )
        logger.info("=" * 60)
        logger.info(f"SELECTED: Matchup #1 with score {top_matchups[0][0]:.1f}")

        return best_teams

    def shuffle_from_pool(
        self,
        players: list[Player],
        exclusion_counts: dict[str, int] | None = None,
        recent_match_names: set[str] | None = None,
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
            team1, team2 = self.shuffle(players)
            return team1, team2, []

        if len(players) == 14:
            # Use branch and bound for 14 players (optimized pruning)
            return self.shuffle_branch_bound(players, exclusion_counts, recent_match_names)

        # ---- Performance knobs (kept internal to preserve current public API) ----
        # Pool shuffles are far more expensive than 10-player shuffles. We therefore
        # intentionally reduce role-assignment exploration here.
        pool_max_assignments_per_team = 3  # 3x3=9 role combos per matchup (vs 20x20=400)
        log_top_k = 5

        # Deterministic RNG for any sampling/tie-breaking in pool shuffles.
        # (Avoid flaky tests and hard-to-reproduce behavior.)
        pool_rng = random.Random(0)

        def _sample_player_combinations(
            n: int, k: int, max_samples: int
        ) -> Iterable[tuple[int, ...]]:
            """Yield up to max_samples unique k-combinations from range(n) deterministically."""
            if max_samples <= 0:
                return []
            seen = set()
            # Cap attempts to avoid pathological loops when nCk isn't much bigger than max_samples.
            attempts_left = max_samples * 25
            while len(seen) < max_samples and attempts_left > 0:
                attempts_left -= 1
                combo = tuple(sorted(pool_rng.sample(range(n), k)))
                if combo in seen:
                    continue
                seen.add(combo)
                yield combo

        # Try combinations of 10 players from the pool (possibly sampled for very large pools).
        best_teams: tuple[Team, Team] | None = None
        best_excluded: list[Player] | None = None
        best_score = float("inf")
        best_value_diff = float("inf")
        best_total_off_roles = float("inf")
        best_signature: tuple[tuple[str, ...], tuple[str, ...], tuple[str, ...]] | None = None

        # Track top-K matchups for logging (store only a small heap to avoid O(N) memory).
        # Keep a numeric tiebreaker so heapq never tries to compare Team objects.
        top_matchups_heap: list[
            tuple[
                float,
                int,
                tuple[
                    float, float, float, float, float, float, float, int, int, list[str], Team, Team
                ],
            ]
        ] = []
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
            selected_indices_iter = _sample_player_combinations(
                len(players), 10, max_player_combinations
            )
            logger.info(
                f"Sampling {max_player_combinations} of {total_player_combinations} player combinations"
            )
        else:
            selected_indices_iter = itertools.combinations(range(len(players)), 10)

        early_termination_threshold = 0.0
        perfect_match = False

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

                # Optimize role assignments for this matchup
                team1, team2, base_score = self._optimize_role_assignments_for_matchup(
                    team1_players,
                    team2_players,
                    max_assignments_per_team=pool_max_assignments_per_team,
                )

                # base_score includes: value_diff + off_role_penalty + role_matchup_delta - rd_priority
                # We need to add exclusion_penalty and recent_match_penalty, then extract components for logging

                # Add recent match penalty for selected players
                selected_player_names = {p.name for p in team1_players + team2_players}
                recent_in_match = len(selected_player_names & recent_match_names)
                recent_penalty = recent_in_match * self.recent_match_penalty_weight

                total_score = base_score + exclusion_penalty + recent_penalty

                # Extract components for logging
                team1_value = team1.get_team_value(
                    self.use_glicko, self.off_role_multiplier, use_openskill=self.use_openskill
                )
                team2_value = team2.get_team_value(
                    self.use_glicko, self.off_role_multiplier, use_openskill=self.use_openskill
                )
                value_diff = abs(team1_value - team2_value)
                team1_off_roles = team1.get_off_role_count()
                team2_off_roles = team2.get_off_role_count()
                off_role_penalty = (team1_off_roles + team2_off_roles) * self.off_role_flat_penalty
                role_matchup_delta = self._calculate_role_matchup_delta(team1, team2)
                total_off_roles = team1_off_roles + team2_off_roles

                # Track top-K only (avoid storing all matchups).
                if log_top_k > 0:
                    entry = (
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
                    if len(top_matchups_heap) < log_top_k:
                        heap_tiebreaker += 1
                        heapq.heappush(top_matchups_heap, (-total_score, heap_tiebreaker, entry))
                    else:
                        worst_score = -top_matchups_heap[0][0]
                        if total_score < worst_score:
                            heap_tiebreaker += 1
                            heapq.heapreplace(
                                top_matchups_heap, (-total_score, heap_tiebreaker, entry)
                            )

                # Deterministic best selection to avoid flaky tests:
                # minimize (score, value_diff, total_off_roles), then break ties lexicographically by names.
                team1_sig = tuple(sorted(p.name for p in team1.players))
                team2_sig = tuple(sorted(p.name for p in team2.players))
                excluded_sig = tuple(sorted(excluded_names))
                # Canonicalize team order
                if team2_sig < team1_sig:
                    team1_sig, team2_sig = team2_sig, team1_sig
                signature = (team1_sig, team2_sig, excluded_sig)

                is_better = (
                    total_score < best_score
                    or (total_score == best_score and value_diff < best_value_diff)
                    or (
                        total_score == best_score
                        and value_diff == best_value_diff
                        and total_off_roles < best_total_off_roles
                    )
                    or (
                        total_score == best_score
                        and value_diff == best_value_diff
                        and total_off_roles == best_total_off_roles
                        and (best_signature is None or signature < best_signature)
                    )
                )
                if is_better:
                    best_score = total_score
                    best_value_diff = value_diff
                    best_total_off_roles = total_off_roles
                    best_signature = signature
                    best_teams = (team1, team2)
                    best_excluded = excluded_players

                    if best_score <= early_termination_threshold:
                        logger.info(f"Early termination: score <= {early_termination_threshold}")
                        perfect_match = True
                        break

            if perfect_match:
                break

        # Log top 5 matchups
        if logger.isEnabledFor(logging.INFO) and top_matchups_heap:
            top_entries = [entry for _neg, _tb, entry in top_matchups_heap]
            top_entries.sort(key=lambda x: x[0])
            logger.info("=" * 60)
            logger.info(
                f"TOP {min(log_top_k, len(top_entries))} MATCHUPS (from pool of {len(players)} players):"
            )
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
            ) in enumerate(top_entries[:log_top_k], 1):
                t1_roles = (
                    t1.role_assignments if t1.role_assignments else t1._assign_roles_optimally()
                )
                t2_roles = (
                    t2.role_assignments if t2.role_assignments else t2._assign_roles_optimally()
                )
                logger.info(
                    f"\n#{i} - Total Score: {score:.1f} (Value Diff: {value_diff:.1f}, Off-Role Penalty: {off_penalty:.1f}, "
                    f"Role Matchup Delta: {role_delta:.1f}, Exclusion Penalty: {excl_penalty:.1f})"
                )
                logger.info(
                    f"  Team 1 Value: {t1_val:.1f} | Team 2 Value: {t2_val:.1f} | Diff: {abs(t1_val - t2_val):.1f}"
                )
                logger.info(
                    f"  Off-Roles: Team1={t1_off}, Team2={t2_off} (Total: {t1_off + t2_off})"
                )
                logger.info(f"  Excluded: {', '.join(excluded) if excluded else 'None'}")
                logger.info(
                    f"  Team 1: {', '.join([f'{p.name}({role})' for p, role in zip(t1.players, t1_roles)])}"
                )
                logger.info(
                    f"  Team 2: {', '.join([f'{p.name}({role})' for p, role in zip(t2.players, t2_roles)])}"
                )
            logger.info("=" * 60)

        if best_teams is None or best_excluded is None:
            raise RuntimeError("Failed to compute teams from pool shuffle (no matchups evaluated)")

        return best_teams[0], best_teams[1], best_excluded

    def shuffle_branch_bound(
        self,
        players: list[Player],
        exclusion_counts: dict[str, int] | None = None,
        recent_match_names: set[str] | None = None,
    ) -> tuple[Team, Team, list[Player]]:
        """
        Branch and bound shuffle optimized for 14 players.

        Uses pruning to avoid evaluating team combinations that cannot beat
        the current best score. Provides significant speedup for large player pools.

        Args:
            players: List of exactly 14 players
            exclusion_counts: Optional dict mapping player names to exclusion counts
            recent_match_names: Optional set of player names who participated in the most recent match

        Returns:
            Tuple of (Team1, Team2, excluded_players)
        """
        if len(players) != 14:
            raise ValueError(f"Branch and bound shuffle requires exactly 14 players, got {len(players)}")

        exclusion_counts = exclusion_counts or {}
        recent_match_names = recent_match_names or set()

        # Step 1: Get greedy initial upper bound
        greedy_t1, greedy_t2, greedy_excluded, best_score = self._greedy_shuffle(
            players, exclusion_counts, recent_match_names
        )
        best_result: tuple[Team, Team, list[Player]] = (greedy_t1, greedy_t2, greedy_excluded)

        logger.info(f"Branch & bound: greedy upper bound = {best_score:.1f}")

        # Precompute player values for fast lower bound calculations
        player_values = {
            p.name: p.get_value(self.use_glicko, use_openskill=self.use_openskill)
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

            # Quick pruning: if exclusion penalty alone exceeds best score, skip
            if exclusion_penalty >= best_score:
                pruned_player_selections += 1
                continue

            # Precompute selected player values (sorted descending for lower bound calc)
            selected_values = sorted(
                [player_values[p.name] for p in selected_players], reverse=True
            )

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

                # Quick lower bound: value_diff + exclusion_penalty + recent_penalty (ignore role penalties)
                lower_bound = value_diff_lb + exclusion_penalty + recent_penalty

                # Prune if lower bound >= best score
                if lower_bound >= best_score:
                    pruned_team_splits += 1
                    continue

                # Step 4: Full role optimization (only for promising splits)
                evaluated_matchups += 1
                team1, team2, base_score = self._optimize_role_assignments_for_matchup(
                    team1_players, team2_players, max_assignments_per_team=3
                )

                total_score = base_score + exclusion_penalty + recent_penalty

                if total_score < best_score:
                    best_score = total_score
                    best_result = (team1, team2, excluded_players)

                    # Early termination on perfect match
                    if best_score <= 0:
                        logger.info("Branch & bound: perfect match found, early termination")
                        break

            # Check for early termination after inner loop
            if best_score <= 0:
                break

        logger.info(
            f"Branch & bound stats: pruned {pruned_player_selections} player selections, "
            f"{pruned_team_splits} team splits, evaluated {evaluated_matchups} matchups"
        )
        logger.info(f"Branch & bound: final score = {best_score:.1f}")

        return best_result

    def shuffle_monte_carlo(self, players: list[Player], top_n: int = 3) -> tuple[Team, Team]:
        """
        Monte Carlo approach: find top N teams by value, then select best by role distribution.

        This is the approach suggested by Dane to avoid unfairly punishing certain roles.

        Args:
            players: List of exactly 10 players
            top_n: Number of top teams to consider for role optimization

        Returns:
            Tuple of (Team1, Team2)
        """
        if len(players) != 10:
            raise ValueError(f"Need exactly 10 players, got {len(players)}")

        # Find top N team combinations by value difference
        team_combinations = []

        for team1_indices in itertools.combinations(range(10), 5):
            team1_players = [players[i] for i in team1_indices]
            team2_players = [players[i] for i in range(10) if i not in team1_indices]

            team1 = Team(team1_players)
            team2 = Team(team2_players)

            team1_value = team1.get_team_value(
                self.use_glicko, self.off_role_multiplier, use_openskill=self.use_openskill
            )
            team2_value = team2.get_team_value(
                self.use_glicko, self.off_role_multiplier, use_openskill=self.use_openskill
            )
            value_diff = abs(team1_value - team2_value)

            team_combinations.append((value_diff, team1, team2))

        # Sort by value difference and take top N
        team_combinations.sort(key=lambda x: x[0])
        top_teams = team_combinations[:top_n]

        # From top N, select the one with best role distribution
        best_teams = None
        best_role_score = float("inf")

        for value_diff, team1, team2 in top_teams:
            role_score = team1.get_role_balance_score() + team2.get_role_balance_score()
            if role_score < best_role_score:
                best_role_score = role_score
                best_teams = (team1, team2)

        return best_teams

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

        Uses worst-case scoring: pool_score = max(score_a, score_b) + penalties.

        Args:
            captain_a: First captain
            captain_b: Second captain
            candidates: Non-captain lobby players (8-12)
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

        # Enumerate all C(N, 8) pools and pick the best
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
            f"Draft pool selection: evaluated {math.comb(len(candidates), 8)} pools, "
            f"best score={best_pool_score:.1f}"
        )

        return best_result
