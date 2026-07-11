"""
Draft domain service for Immortal Draft mode.

Contains pure domain logic for captain selection and player pool management.
No side effects or external dependencies.
"""

import random
from dataclasses import dataclass


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
    - Captain selection (closest Glicko rating pair)
    - Player pool selection (using exclusion counts)
    - Coinflip logic
    """

    def __init__(self, rating_weight_factor: float = 50.0):
        """
        Initialize draft service.

        Args:
            rating_weight_factor: Retained for compatibility with existing callers.
                                  Captain selection now ignores this value and
                                  chooses the closest Glicko ratings deterministically.
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
        - If one specified, select the eligible captain closest in Glicko rating
        - If neither specified, select the eligible pair closest in Glicko rating

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
        available = list(eligible_ids)

        # Remove specified captains from available pool
        if specified_captain1 is not None and specified_captain1 in available:
            available.remove(specified_captain1)
        if specified_captain2 is not None and specified_captain2 in available:
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

            captain1, captain2 = self._closest_rating_pair(available, player_ratings)

        elif captain1 is None:
            # captain2 is specified, need to select captain1
            if len(available) < 1:
                raise ValueError("Need at least 1 captain-eligible player to be second captain.")
            captain1 = self._closest_rating_captain(
                captain2, available, player_ratings
            )

        else:
            # captain1 is specified, need to select captain2
            if len(available) < 1:
                raise ValueError("Need at least 1 captain-eligible player to be second captain.")
            captain2 = self._closest_rating_captain(
                captain1, available, player_ratings
            )

        return CaptainPair(
            captain1_id=captain1,
            captain1_rating=player_ratings.get(captain1, 0.0),
            captain2_id=captain2,
            captain2_rating=player_ratings.get(captain2, 0.0),
        )

    def _closest_rating_pair(
        self,
        candidates: list[int],
        player_ratings: dict[int, float],
    ) -> tuple[int, int]:
        """
        Select the pair of captains with the smallest absolute rating gap.
        """
        best_pair: tuple[int, int] | None = None
        best_diff = float("inf")

        for index, captain1_id in enumerate(candidates):
            captain1_rating = player_ratings.get(captain1_id, 0.0)
            for captain2_id in candidates[index + 1:]:
                diff = abs(captain1_rating - player_ratings.get(captain2_id, 0.0))
                if diff < best_diff:
                    best_diff = diff
                    best_pair = (captain1_id, captain2_id)

        if best_pair is None:
            raise ValueError("Need at least 2 captain-eligible players.")

        return best_pair

    def _closest_rating_captain(
        self,
        reference_captain_id: int,
        candidates: list[int],
        player_ratings: dict[int, float],
    ) -> int:
        """
        Select the captain closest in rating to the reference captain.

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
        return min(
            candidates,
            key=lambda pid: abs(player_ratings.get(pid, 0.0) - reference_rating),
        )

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
