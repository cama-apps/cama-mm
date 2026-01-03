"""
Service for applying garnishment to income for players with debt.
"""

from typing import Dict, Optional

from config import GARNISHMENT_PERCENTAGE
from repositories.player_repository import PlayerRepository


class GarnishmentService:
    """Applies garnishment to income for players with negative balances."""

    def __init__(
        self,
        player_repo: PlayerRepository,
        garnishment_rate: Optional[float] = None,
    ):
        self.player_repo = player_repo
        self.garnishment_rate = (
            garnishment_rate if garnishment_rate is not None else GARNISHMENT_PERCENTAGE
        )

    def add_income(self, discord_id: int, amount: int) -> Dict[str, int]:
        """
        Add income to a player, applying garnishment if they have debt.

        When a player has a negative balance, a portion of their income is
        "garnished" to pay down the debt. The full amount is credited to
        their balance, but the return value shows the garnishment breakdown.

        Args:
            discord_id: Player's Discord ID
            amount: Income amount (bet winnings, participation reward, etc.)

        Returns:
            Dict with:
            - gross: Original income amount
            - garnished: Amount conceptually going toward debt repayment
            - net: Amount the player "feels" they received
        """
        if amount <= 0:
            return {"gross": amount, "garnished": 0, "net": amount}

        return self.player_repo.add_balance_with_garnishment(
            discord_id, amount, self.garnishment_rate
        )
