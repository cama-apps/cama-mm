"""
Service-layer caching for role assignment calculations.

This moves the performance optimization (lru_cache) out of the domain layer
to maintain clean architecture. Domain models should be pure business logic
without performance concerns.

Thread-safety: Uses a lock to prevent cache corruption during parallel test
execution with pytest-xdist.
"""

import threading
from functools import lru_cache

# Lock to protect cache operations during parallel execution
_cache_lock = threading.Lock()


@lru_cache(maxsize=1024)
def _compute_cached_role_assignments(
    player_roles_key: tuple[tuple[str, ...], ...],
) -> tuple[tuple[str, ...], ...]:
    """
    Internal cached computation. Protected by _cache_lock in public wrapper.
    """
    # Import here to avoid circular import at module level
    from domain.models.team import compute_optimal_role_assignments

    return compute_optimal_role_assignments(player_roles_key)


def get_cached_role_assignments(
    player_roles_key: tuple[tuple[str, ...], ...],
) -> tuple[tuple[str, ...], ...]:
    """
    Compute and cache optimal role assignments for players based on their preferred roles.

    This is cached because the same 5 players will be evaluated many times
    during shuffle operations. The cache key is a tuple of tuples representing
    each player's preferred roles.

    Thread-safe: Uses a lock to prevent cache corruption during parallel execution.

    Args:
        player_roles_key: Tuple of (player_preferred_roles_tuple, ...)

    Returns:
        Tuple of optimal role assignments (each assignment is a tuple of 5 role strings)
    """
    with _cache_lock:
        return _compute_cached_role_assignments(player_roles_key)


def clear_role_assignment_cache() -> None:
    """Clear the role assignment cache. Useful for testing."""
    with _cache_lock:
        _compute_cached_role_assignments.cache_clear()


def get_cache_info():
    """Get cache statistics. Useful for monitoring/debugging."""
    with _cache_lock:
        return _compute_cached_role_assignments.cache_info()
