"""
Shared formatting utilities for draft embeds.
"""

# Display constants
MAX_NAME_LEN = 10  # Max length for player names in embeds


def format_roles(roles: list[str] | None) -> str:
    """
    Format player roles as a sorted bracket string.

    Args:
        roles: List of role strings like ["3", "1", "2"]

    Returns:
        Formatted string like "[1,2,3]" or empty string if no roles
    """
    if not roles:
        return ""
    sorted_roles = sorted(roles, key=lambda r: int(r) if r.isdigit() else 99)
    return "[" + ",".join(sorted_roles) + "]"


def format_player_row(is_captain: bool, name: str, rating: float, roles: str) -> str:
    """
    Format a player row for draft embed display.

    Args:
        is_captain: Whether this player is a captain (shows crown icon)
        name: Player display name
        rating: Player's Glicko rating
        roles: Formatted role string like "[1,2,3]"

    Returns:
        Formatted string like "👑 PlayerName (1500) [1,2]"
    """
    icon = "👑" if is_captain else "⠀⠀"  # Braille blank for spacing
    name_display = name[:MAX_NAME_LEN]
    roles_display = f" {roles}" if roles else ""
    return f"{icon} {name_display} ({rating:.0f}){roles_display}"
