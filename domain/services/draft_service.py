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
        player_pool_ids: list[int],
        player_ratings: dict[int, float],
        specified_captain1: int | None = None,
        specified_captain2: int | None = None,
    ) -> CaptainPair:
        """
        Select two captains from the final player pool.

        Algorithm:
        - If both captains specified, use them
        - If one specified, select the pool member closest in Glicko rating
        - If neither specified, select the pool pair closest in Glicko rating

        Args:
            player_pool_ids: Final Immortal Draft player pool IDs
            player_ratings: Dict mapping player ID to rating
            specified_captain1: Optional pre-specified captain
            specified_captain2: Optional pre-specified captain

        Returns:
            CaptainPair with both captain IDs and ratings

        Raises:
            ValueError: If the pool does not contain enough players
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
        available = list(player_pool_ids)

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
                    f"Need at least 2 players in the draft pool, but only {len(available)} available."
                )

            captain1, captain2 = self._closest_rating_pair(available, player_ratings)

        elif captain1 is None:
            # captain2 is specified, need to select captain1
            if len(available) < 1:
                raise ValueError("Need at least 1 other player in the draft pool.")
            captain1 = self._closest_rating_captain(
                captain2, available, player_ratings
            )

        else:
            # captain1 is specified, need to select captain2
            if len(available) < 1:
                raise ValueError("Need at least 1 other player in the draft pool.")
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
            raise ValueError("Need at least 2 players in the draft pool.")

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
        regular_player_ids: list[int],
        conditional_player_ids: list[int],
        exclusion_counts: dict[int, int],
        player_ratings: dict[int, float],
        forced_include_ids: list[int] | None = None,
        pool_size: int = 10,
    ) -> PoolSelectionResult:
        """
        Select players for the draft pool by exclusion factor, then Glicko.

        Regular players fill the pool before conditional players. Manually
        specified captains are always included, even when conditional.

        Args:
            regular_player_ids: Regular lobby player IDs in lobby order
            conditional_player_ids: Conditional player IDs in lobby order
            exclusion_counts: Dict mapping player ID to exclusion count
            player_ratings: Dict mapping player ID to Glicko rating
            forced_include_ids: Captain override IDs that must be included
            pool_size: Target pool size (default 10)

        Returns:
            PoolSelectionResult with selected and excluded IDs

        Raises:
            ValueError: If lobby has fewer than pool_size players
        """
        all_player_ids = list(dict.fromkeys(regular_player_ids + conditional_player_ids))
        if len(all_player_ids) < pool_size:
            raise ValueError(
                f"Need at least {pool_size} players in lobby, but only {len(all_player_ids)} present."
            )

        forced = list(dict.fromkeys(forced_include_ids or []))
        if len(forced) > pool_size:
            raise ValueError("Too many forced-include players for pool size.")
        if any(pid not in all_player_ids for pid in forced):
            raise ValueError("Specified captains must be present in the lobby.")

        forced_set = set(forced)

        def rank(player_ids: list[int]) -> list[int]:
            return sorted(
                (pid for pid in player_ids if pid not in forced_set),
                key=lambda pid: (
                    -exclusion_counts.get(pid, 0),
                    -player_ratings.get(pid, 1500.0),
                ),
            )

        selected = list(forced)
        for player_id in rank(regular_player_ids):
            if len(selected) >= pool_size:
                break
            selected.append(player_id)

        if len(selected) < pool_size:
            for player_id in rank(conditional_player_ids):
                if len(selected) >= pool_size:
                    break
                selected.append(player_id)

        selected_set = set(selected)
        excluded = [pid for pid in all_player_ids if pid not in selected_set]

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
