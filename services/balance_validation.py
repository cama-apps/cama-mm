"""
Balance validation utilities for jopacoin economy.

Centralizes balance checking logic that's used across multiple services.
"""

from typing import TYPE_CHECKING

from config import MAX_DEBT
from services.result import Result
from services import error_codes

if TYPE_CHECKING:
    from repositories.interfaces import IPlayerRepository


def validate_can_spend(
    player_repo: "IPlayerRepository",
    discord_id: int,
    amount: int,
    max_debt: int | None = None,
    guild_id: int | None = None,
) -> Result[int]:
    """
    Check if a player can spend the specified amount.

    A player can spend if:
    - They have sufficient balance, OR
    - Spending wouldn't push them below -max_debt

    Args:
        player_repo: Player repository for balance lookup
        discord_id: The player's Discord ID
        amount: Amount to spend
        max_debt: Maximum allowed debt (defaults to config.MAX_DEBT)
        guild_id: Guild ID for multi-guild support

    Returns:
        Result.ok(new_balance) if spending is allowed
        Result.fail(error, code) if spending would exceed debt limit

    Examples:
        >>> validate_can_spend(repo, 123, 10)  # balance=50
        Result(success=True, value=40)

        >>> validate_can_spend(repo, 123, 100)  # balance=50, max_debt=100
        Result(success=True, value=-50)

        >>> validate_can_spend(repo, 123, 200)  # balance=50, max_debt=100
        Result(success=False, error="...", error_code="max_debt_exceeded")
    """
    if max_debt is None:
        max_debt = MAX_DEBT

    balance = player_repo.get_balance(discord_id, guild_id)
    new_balance = balance - amount

    if new_balance < -max_debt:
        return Result.fail(
            f"Spending {amount} would exceed max debt of {max_debt}. "
            f"Current balance: {balance}",
            code=error_codes.MAX_DEBT_EXCEEDED,
        )

    return Result.ok(new_balance)


def validate_positive_balance(
    player_repo: "IPlayerRepository",
    discord_id: int,
    guild_id: int | None = None,
) -> Result[int]:
    """
    Check if a player has a non-negative balance.

    Args:
        player_repo: Player repository for balance lookup
        discord_id: The player's Discord ID
        guild_id: Guild ID for multi-guild support

    Returns:
        Result.ok(balance) if balance >= 0
        Result.fail(error, code) if balance < 0
    """
    balance = player_repo.get_balance(discord_id, guild_id)

    if balance < 0:
        return Result.fail(
            f"You're in debt ({balance}). You cannot perform this action while in debt.",
            code=error_codes.IN_DEBT,
        )

    return Result.ok(balance)


def validate_has_amount(
    player_repo: "IPlayerRepository",
    discord_id: int,
    amount: int,
    guild_id: int | None = None,
) -> Result[int]:
    """
    Check if a player has at least the specified amount.

    Unlike validate_can_spend, this doesn't allow going into debt.

    Args:
        player_repo: Player repository for balance lookup
        discord_id: The player's Discord ID
        amount: Required amount
        guild_id: Guild ID for multi-guild support

    Returns:
        Result.ok(balance) if balance >= amount
        Result.fail(error, code) if balance < amount
    """
    balance = player_repo.get_balance(discord_id, guild_id)

    if balance < amount:
        return Result.fail(
            f"Insufficient balance. You have {balance}, but need {amount}.",
            code=error_codes.INSUFFICIENT_FUNDS,
        )

    return Result.ok(balance)
