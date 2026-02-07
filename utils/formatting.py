"""
Shared formatting helpers and role constants.
"""

from collections.abc import Iterable

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
JOPACOIN_EMOJI_ID = 954159801049440297

# Custom frogling emote for conditional lobby players
FROGLING_EMOTE = "<:frogling:1463270458848842003>"
FROGLING_EMOJI_ID = 1463270458848842003

# Tombstone emoji for bankrupted players
TOMBSTONE_EMOJI = "🪦"


def format_role_display(role: str) -> str:
    """Return role string with emoji and name (e.g., '⚔️ Carry')."""
    emoji = ROLE_EMOJIS.get(role, "")
    name = ROLE_NAMES.get(role, role)
    return f"{emoji} {name}".strip()


def format_roles_list(roles: Iterable[str]) -> str:
    """Return comma-separated roles with emoji display."""
    return ", ".join(format_role_display(r) for r in roles)


def calculate_pool_odds(radiant_total: int, dire_total: int) -> tuple[float | None, float | None]:
    """
    Calculate pool betting multipliers for each team.

    Returns (radiant_multiplier, dire_multiplier).
    Multiplier shows what you'd get back per 1 unit bet if your team wins.
    Returns None for a team if they have 0 bets (undefined odds).
    """
    total_pool = radiant_total + dire_total
    if total_pool == 0:
        return None, None

    radiant_mult = total_pool / radiant_total if radiant_total > 0 else None
    dire_mult = total_pool / dire_total if dire_total > 0 else None
    return radiant_mult, dire_mult


def format_betting_display(
    radiant_total: int,
    dire_total: int,
    betting_mode: str,
    lock_until: int | None = None,
) -> tuple[str, str]:
    """
    Format the betting display for embeds.

    Args:
        radiant_total: Total jopacoin bet on Radiant
        dire_total: Total jopacoin bet on Dire
        betting_mode: "house" or "pool"
        lock_until: Unix timestamp when betting closes

    Returns:
        (field_name, field_value) tuple for the embed field.
    """
    lock_text = f"Closes <t:{int(lock_until)}:R>" if lock_until else ""

    if betting_mode == "pool":
        radiant_mult, dire_mult = calculate_pool_odds(radiant_total, dire_total)
        radiant_odds = f"({radiant_mult:.2f}x)" if radiant_mult else "(—)"
        dire_odds = f"({dire_mult:.2f}x)" if dire_mult else "(—)"

        totals_text = (
            f"Radiant: {radiant_total} {JOPACOIN_EMOTE} {radiant_odds} | "
            f"Dire: {dire_total} {JOPACOIN_EMOTE} {dire_odds}"
        )
        if lock_text:
            totals_text += f"\n{lock_text}"

        return "💰 Pool Betting", totals_text
    else:
        # House mode - original format
        totals_text = (
            f"Radiant: {radiant_total} {JOPACOIN_EMOTE} | Dire: {dire_total} {JOPACOIN_EMOTE}"
        )
        if lock_text:
            totals_text += f"\n{lock_text}"

        return "💰 House Betting (1:1)", totals_text


def get_player_display_name(
    player, discord_id: int | None = None, guild=None, bankruptcy_repo=None
) -> str:
    """
    Get a player's display name, preferring Discord nickname when available.

    Fake users (negative IDs) skip guild lookups to avoid API calls.
    Adds tombstone emoji for players with active bankruptcy penalties.

    Args:
        player: Player object or name
        discord_id: Discord user ID (optional)
        guild: Discord guild object (optional)
        bankruptcy_repo: BankruptcyRepository instance for checking bankruptcy status (optional)
    """
    # Skip guild lookup for fake users
    if discord_id and discord_id < 0:
        return player.name if hasattr(player, "name") else str(player)

    base_name = None
    if guild and discord_id:
        try:
            member = guild.get_member(discord_id)
            if member:
                base_name = member.display_name
        except Exception:
            # If Discord lookup fails, fall back to stored name
            pass

    if base_name is None:
        base_name = player.name if hasattr(player, "name") else str(player)

    # Add tombstone emoji if player has active bankruptcy penalty
    if bankruptcy_repo and discord_id and discord_id > 0:
        try:
            penalty_games = bankruptcy_repo.get_penalty_games(discord_id)
            if penalty_games > 0:
                return f"{TOMBSTONE_EMOJI} {base_name}"
        except Exception:
            # If bankruptcy check fails, just return the base name
            pass

    return base_name


def format_duration_short(seconds: float) -> str:
    """Format a duration as a concise string like '1m', '2h', '1d'."""
    s = int(seconds)
    if s < 60:
        return f"{max(s, 1)}s"
    if s < 3600:
        return f"{s // 60}m"
    if s < 86400:
        return f"{s // 3600}h"
    return f"{s // 86400}d"
