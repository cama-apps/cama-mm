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
    # Need to get member from guild for accurate permissions
    if interaction.guild:
        member = interaction.guild.get_member(interaction.user.id)
        if member and member.guild_permissions:
            return (
                member.guild_permissions.administrator
                or member.guild_permissions.manage_guild
            )

    return False
