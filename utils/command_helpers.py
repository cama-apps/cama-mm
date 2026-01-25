"""
Command helper utilities for Discord slash commands.

Provides utilities for handling service Results in command handlers,
reducing boilerplate and ensuring consistent error reporting.
"""

import discord
from typing import TYPE_CHECKING

from utils.interaction_safety import safe_followup

if TYPE_CHECKING:
    from services.result import Result


async def handle_result(
    interaction: discord.Interaction,
    result: "Result",
    success_msg: str | None = None,
    ephemeral: bool = True,
) -> bool:
    """
    Handle a service Result, sending appropriate Discord response.

    Args:
        interaction: The Discord interaction to respond to
        result: The Result from a service call
        success_msg: Optional message to send on success (None = no message)
        ephemeral: Whether the message should be ephemeral

    Returns:
        True if the result was successful, False otherwise

    Usage:
        result = await some_service.do_something()
        if not await handle_result(interaction, result, "Operation completed!"):
            return  # Error was already reported to user
        # Continue with success case...
    """
    if not result.success:
        error_msg = f"Error: {result.error}"
        if result.error_code:
            error_msg = f"[{result.error_code}] {result.error}"
        await safe_followup(interaction, content=error_msg, ephemeral=ephemeral)
        return False

    if success_msg:
        await safe_followup(interaction, content=success_msg, ephemeral=ephemeral)
    return True


async def handle_result_with_embed(
    interaction: discord.Interaction,
    result: "Result",
    success_embed: discord.Embed | None = None,
    ephemeral: bool = False,
) -> bool:
    """
    Handle a service Result, sending an embed on success.

    Args:
        interaction: The Discord interaction to respond to
        result: The Result from a service call
        success_embed: Embed to send on success (None = no response)
        ephemeral: Whether the message should be ephemeral

    Returns:
        True if the result was successful, False otherwise
    """
    if not result.success:
        error_msg = f"Error: {result.error}"
        await safe_followup(interaction, content=error_msg, ephemeral=True)
        return False

    if success_embed:
        await safe_followup(interaction, embed=success_embed, ephemeral=ephemeral)
    return True


def format_result_error(result: "Result") -> str:
    """
    Format a Result error for display.

    Args:
        result: A failed Result

    Returns:
        Formatted error string
    """
    if result.success:
        return ""
    if result.error_code:
        return f"[{result.error_code}] {result.error}"
    return result.error or "Unknown error"
