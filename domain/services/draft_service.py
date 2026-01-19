"""
Draft domain service for Immortal Draft mode.

Contains pure domain logic for captain selection and player pool management.
No side effects or external dependencies.
"""

import random
from dataclasses import dataclass

from domain.models.player import Player


@dataclass
class CaptainPair:
    """Result of captain selection."""

    captain1_id: int
    captain1_rating: float
    captain2_id: int
    captain2_rating: float


@dataclass
class PoolSelectionResult:
    """Result of player pool selection."""

    selected_ids: list[int]
    excluded_ids: list[int]


class DraftService:
    """
    Pure domain logic for Immortal Draft.

    Handles:
    - Captain selection (random + weighted random)
    - Player pool selection (using exclusion counts)
    - Coinflip logic
    """

    def __init__(self, rating_weight_factor: float = 100.0):
        """
        Initialize draft service.

        Args:
            rating_weight_factor: Higher = more weight to similar ratings.
                                  At 100, a 100-point difference halves the weight.
        """
        self.rating_weight_factor = rating_weight_factor

    def select_captains(
        self,
        eligible_ids: list[int],
        player_ratings: dict[int, float],
        specified_captain1: int | None = None,
        specified_captain2: int | None = None,
    ) -> CaptainPair:
        """
        Select two captains from eligible players.

        Algorithm:
        - If both captains specified, use them
        - If one specified, select the other using weighted random
        - If neither specified:
          1. Random select first captain
          2. Weighted random select second (closer rating = higher chance)

        Args:
            eligible_ids: List of captain-eligible player IDs
            player_ratings: Dict mapping player ID to rating
            specified_captain1: Optional pre-specified captain
            specified_captain2: Optional pre-specified captain

        Returns:
            CaptainPair with both captain IDs and ratings

        Raises:
            ValueError: If not enough eligible captains
        """
        # Handle specified captains
        if specified_captain1 is not None and specified_captain2 is not None:
            return CaptainPair(
                captain1_id=specified_captain1,
                captain1_rating=player_ratings.get(specified_captain1, 0.0),
                captain2_id=specified_captain2,
                captain2_rating=player_ratings.get(specified_captain2, 0.0),
            )

        # Build pool of available captains
        available = [pid for pid in eligible_ids]

        # Remove specified captains from available pool
        if specified_captain1 is not None:
            if specified_captain1 in available:
                available.remove(specified_captain1)
        if specified_captain2 is not None:
            if specified_captain2 in available:
                available.remove(specified_captain2)

        # Determine how many captains we need to select
        captain1 = specified_captain1
        captain2 = specified_captain2

        if captain1 is None and captain2 is None:
            # Need to select both
            if len(available) < 2:
                raise ValueError(
                    f"Need at least 2 captain-eligible players, but only {len(available)} available."
                )

            # Random select first captain
            captain1 = random.choice(available)
            available.remove(captain1)

            # Weighted random select second captain
            captain2 = self._weighted_random_captain(
                captain1, available, player_ratings
            )

        elif captain1 is None:
            # captain2 is specified, need to select captain1
            if len(available) < 1:
                raise ValueError("Need at least 1 captain-eligible player to be second captain.")
            captain1 = self._weighted_random_captain(
                captain2, available, player_ratings
            )

        else:
            # captain1 is specified, need to select captain2
            if len(available) < 1:
                raise ValueError("Need at least 1 captain-eligible player to be second captain.")
            captain2 = self._weighted_random_captain(
                captain1, available, player_ratings
            )

        return CaptainPair(
            captain1_id=captain1,
            captain1_rating=player_ratings.get(captain1, 0.0),
            captain2_id=captain2,
            captain2_rating=player_ratings.get(captain2, 0.0),
        )

    def _weighted_random_captain(
        self,
        reference_captain_id: int,
        candidates: list[int],
        player_ratings: dict[int, float],
    ) -> int:
        """
        Select a captain using weighted random based on rating proximity.

        Closer rating to reference captain = higher weight.

        Args:
            reference_captain_id: The already-selected captain
            candidates: List of candidate captain IDs
            player_ratings: Dict mapping player ID to rating

        Returns:
            Selected captain ID
        """
        if len(candidates) == 1:
            return candidates[0]

        reference_rating = player_ratings.get(reference_captain_id, 0.0)

        # Calculate weights based on rating difference
        # Weight = 1 / (1 + |rating_diff| / factor)
        # This gives higher weight to closer ratings
        weights = []
        for pid in candidates:
            rating = player_ratings.get(pid, 0.0)
            diff = abs(rating - reference_rating)
            weight = 1.0 / (1.0 + diff / self.rating_weight_factor)
            weights.append(weight)

        # Normalize weights
        total = sum(weights)
        if total == 0:
            # Fallback to uniform random
            return random.choice(candidates)

        # Weighted random selection
        r = random.random() * total
        cumulative = 0.0
        for pid, weight in zip(candidates, weights):
            cumulative += weight
            if r <= cumulative:
                return pid

        # Fallback (shouldn't happen)
        return candidates[-1]

    def select_player_pool(
        self,
        lobby_player_ids: list[int],
        exclusion_counts: dict[int, int],
        forced_include_ids: list[int] | None = None,
        pool_size: int = 10,
    ) -> PoolSelectionResult:
        """
        Select players for the draft pool from lobby.

        Uses exclusion counts to prioritize players who have been excluded more.

        Args:
            lobby_player_ids: All player IDs in the lobby
            exclusion_counts: Dict mapping player ID to exclusion count
            forced_include_ids: IDs that must be included (e.g., specified captains)
            pool_size: Target pool size (default 10)

        Returns:
            PoolSelectionResult with selected and excluded IDs

        Raises:
            ValueError: If lobby has fewer than pool_size players
        """
        if len(lobby_player_ids) < pool_size:
            raise ValueError(
                f"Need at least {pool_size} players in lobby, but only {len(lobby_player_ids)} present."
            )

        if len(lobby_player_ids) == pool_size:
            # Exact match, no exclusions needed
            return PoolSelectionResult(
                selected_ids=list(lobby_player_ids),
                excluded_ids=[],
            )

        forced = set(forced_include_ids or [])

        # Separate forced and non-forced players
        non_forced = [pid for pid in lobby_player_ids if pid not in forced]

        # Sort non-forced by exclusion count descending (higher = more priority)
        # Then by ID for deterministic ordering
        non_forced_sorted = sorted(
            non_forced,
            key=lambda pid: (-exclusion_counts.get(pid, 0), pid),
        )

        # Calculate how many non-forced we need
        forced_count = len(forced)
        needed_from_pool = pool_size - forced_count

        if needed_from_pool < 0:
            # More forced than pool size - shouldn't happen
            raise ValueError("Too many forced-include players for pool size.")

        # Select top N from sorted non-forced
        selected_non_forced = non_forced_sorted[:needed_from_pool]
        excluded = non_forced_sorted[needed_from_pool:]

        # Combine forced + selected
        selected = list(forced) + selected_non_forced

        return PoolSelectionResult(
            selected_ids=selected,
            excluded_ids=excluded,
        )

    def coinflip(self, captain1_id: int, captain2_id: int) -> int:
        """
        Perform a coinflip between two captains.

        Args:
            captain1_id: First captain's Discord ID
            captain2_id: Second captain's Discord ID

        Returns:
            Discord ID of the winning captain
        """
        return random.choice([captain1_id, captain2_id])

    def determine_lower_rated_captain(
        self,
        captain1_id: int,
        captain1_rating: float,
        captain2_id: int,
        captain2_rating: float,
    ) -> int:
        """
        Determine which captain has the lower rating.

        Args:
            captain1_id: First captain's Discord ID
            captain1_rating: First captain's rating
            captain2_id: Second captain's Discord ID
            captain2_rating: Second captain's rating

        Returns:
            Discord ID of the lower-rated captain
        """
        if captain1_rating <= captain2_rating:
            return captain1_id
        return captain2_id
