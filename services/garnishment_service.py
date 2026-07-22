"""
Service for applying garnishment to income for players with debt.
"""

from config import GARNISHMENT_PERCENTAGE
from repositories.player_repository import PlayerRepository


class GarnishmentService:
    """Applies garnishment to income for players with negative balances."""

    def __init__(
        self,
        player_repo: PlayerRepository,
        garnishment_rate: float | None = None,
    ):
        self.player_repo = player_repo
        self.garnishment_rate = (
            garnishment_rate if garnishment_rate is not None else GARNISHMENT_PERCENTAGE
        )

    def add_income(
        self,
        discord_id: int,
        amount: int,
        guild_id: int | None = None,
        bankruptcy_penalty_rate: float = 0.0,
        *,
        source: str | None = None,
        actor_id: int | None = None,
        related_type: str | None = None,
        related_id: str | int | None = None,
        reason: str | None = None,
        metadata: dict | str | None = None,
    ) -> dict[str, int]:
        """
        Add income to a player, applying garnishment if they have debt.

        When a player has a negative balance, a portion of their income is
        "garnished" to pay down the debt. The full amount is credited to
        their balance, but the return value shows the garnishment breakdown.

        Args:
            discord_id: Player's Discord ID
            amount: Income amount (bet winnings, participation reward, etc.)
            guild_id: Guild ID for multi-guild support
            bankruptcy_penalty_rate: When > 0, the repo fuses a bankruptcy
                penalty debit into the same atomic txn, computing the penalty
                from the live post-garnishment net. Pass 0.0 (default) to
                skip. Callers must decide whether the player is *eligible*
                for a penalty (e.g. ``penalty_games > 0``) and pass the rate
                only when they are — this service is policy-agnostic about
                eligibility and just forwards the coefficient.

        Returns:
            Dict with:
            - gross: Original income amount
            - garnished: Amount conceptually going toward debt repayment
            - net: Amount the player "feels" (gross - garnished - penalty)
            - bankruptcy_penalty: Amount debited for the penalty (0 if none)
        """
        if amount <= 0:
            return {"gross": amount, "garnished": 0, "net": amount, "bankruptcy_penalty": 0}

        return self.player_repo.add_balance_with_garnishment(
            discord_id,
            guild_id,
            amount,
            self.garnishment_rate,
            bankruptcy_penalty_rate=bankruptcy_penalty_rate,
            source=source,
            actor_id=actor_id,
            related_type=related_type,
            related_id=related_id,
            reason=reason,
            metadata=metadata,
        )

    def add_income_many(
        self,
        discord_ids: list[int],
        amount: int,
        guild_id: int | None = None,
        bankruptcy_penalty_rates: dict[int, float] | None = None,
    ) -> list[dict[str, int]]:
        """Add the same income to multiple players in one transaction."""
        penalty_rates = bankruptcy_penalty_rates or {}
        return self.player_repo.add_balances_with_garnishment(
            [
                (discord_id, amount, penalty_rates.get(discord_id, 0.0))
                for discord_id in discord_ids
            ],
            guild_id,
            self.garnishment_rate,
        )
