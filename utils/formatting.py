"""
Shared formatting helpers and role constants.
"""

from typing import Iterable, Optional


# Role configuration with emojis and friendly names
ROLE_EMOJIS = {
    "1": "⚔️",  # Sword for Carry
    "2": "🏹",  # Bow for Mid
    "3": "🛡️",  # Shield for Offlane
    "4": "🔥",  # Fire for Soft Support
    "5": "⚕️",  # Medical symbol for Hard Support
}

ROLE_NAMES = {
    "1": "Carry",
    "2": "Mid",
    "3": "Offlane",
    "4": "Soft Support",
    "5": "Hard Support",
}

# Custom jopacoin emote used across embeds/messages
JOPACOIN_EMOTE = "<:jopacoin:954159801049440297>"


def format_role_display(role: str) -> str:
    """Return role string with emoji and name (e.g., '⚔️ Carry')."""
    emoji = ROLE_EMOJIS.get(role, "")
    name = ROLE_NAMES.get(role, role)
    return f"{emoji} {name}".strip()


def format_roles_list(roles: Iterable[str]) -> str:
    """Return comma-separated roles with emoji display."""
    return ", ".join(format_role_display(r) for r in roles)


def get_player_display_name(player, discord_id: Optional[int] = None, guild=None) -> str:
    """
    Get a player's display name, preferring Discord nickname when available.

    Fake users (negative IDs) skip guild lookups to avoid API calls.
    """
    # Skip guild lookup for fake users
    if discord_id and discord_id < 0:
        return player.name if hasattr(player, "name") else str(player)

    if guild and discord_id:
        try:
            member = guild.get_member(discord_id)
            if member:
                return member.display_name
        except Exception:
            # If Discord lookup fails, fall back to stored name
            pass

    return player.name if hasattr(player, "name") else str(player)

