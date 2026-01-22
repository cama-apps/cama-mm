"""
Service-layer caching for role assignment calculations.

This moves the performance optimization (lru_cache) out of the domain layer
to maintain clean architecture. Domain models should be pure business logic
without performance concerns.
"""

from functools import lru_cache


@lru_cache(maxsize=1024)
def get_cached_role_assignments(
    player_roles_key: tuple[tuple[str, ...], ...],
) -> tuple[tuple[str, ...], ...]:
    """
    Compute and cache optimal role assignments for players based on their preferred roles.

    This is cached because the same 5 players will be evaluated many times
    during shuffle operations. The cache key is a tuple of tuples representing
    each player's preferred roles.

    Args:
        player_roles_key: Tuple of (player_preferred_roles_tuple, ...)

    Returns:
        Tuple of optimal role assignments (each assignment is a tuple of 5 role strings)
    """
    # Import here to avoid circular import at module level
    from domain.models.team import compute_optimal_role_assignments

    return compute_optimal_role_assignments(player_roles_key)


def clear_role_assignment_cache() -> None:
    """Clear the role assignment cache. Useful for testing."""
    get_cached_role_assignments.cache_clear()


def get_cache_info():
    """Get cache statistics. Useful for monitoring/debugging."""
    return get_cached_role_assignments.cache_info()
