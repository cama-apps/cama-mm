"""
Guild-related utilities.

This module centralizes guild ID handling to ensure consistent behavior
across the codebase.
"""


def normalize_guild_id(guild_id: int | None) -> int:
    """
    Normalize guild ID for database storage and lookups.

    Guild IDs are normalized to 0 for None values (DMs, tests).
    This ensures consistent behavior across all services and repositories.

    Args:
        guild_id: The guild ID, or None for DMs/tests

    Returns:
        The guild ID as an int (0 for None)

    Examples:
        >>> normalize_guild_id(123456789)
        123456789
        >>> normalize_guild_id(None)
        0
        >>> normalize_guild_id(0)
        0
    """
    return guild_id if guild_id is not None else 0


def is_dm_context(guild_id: int | None) -> bool:
    """
    Check if a context is a DM (no guild).

    Args:
        guild_id: The guild ID, or None for DMs

    Returns:
        True if this is a DM context, False otherwise
    """
    return guild_id is None or guild_id == 0
