"""
Permission checking utilities for the bot.
"""

import discord

from config import ADMIN_USER_IDS


def has_allowlisted_admin(interaction: discord.Interaction) -> bool:
    """
    Check if the user is explicitly allowlisted via ADMIN_USER_IDS.
    If ADMIN_USER_IDS is empty/unset, nobody is considered admin by this check.
    """
    return interaction.user.id in ADMIN_USER_IDS


def has_admin_permission(interaction: discord.Interaction) -> bool:
    """
    Check if user has admin permissions.

    First checks ADMIN_USER_IDS list, then falls back to Discord permissions.

    Args:
        interaction: Discord interaction object

    Returns:
        True if user has admin permissions, False otherwise
    """
    # Check hardcoded admin list first
    if ADMIN_USER_IDS and interaction.user.id in ADMIN_USER_IDS:
        return True

    # Check Discord permissions (Administrator or Manage Server)
    # Prefer guild member lookup, but fall back gracefully for mocks / partial objects.
    if interaction.guild:
        get_member = getattr(interaction.guild, "get_member", None)
        if callable(get_member):
            member = get_member(interaction.user.id)
            if member and getattr(member, "guild_permissions", None):
                return (
                    member.guild_permissions.administrator
                    or member.guild_permissions.manage_guild
                )

    # Fallback: interaction.user may already be a Member-like object with guild_permissions
    perms = getattr(interaction.user, "guild_permissions", None)
    if perms:
        return bool(getattr(perms, "administrator", False) or getattr(perms, "manage_guild", False))

    return False
