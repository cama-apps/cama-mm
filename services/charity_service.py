"""
Charity service for tracking /paydebt contributions and reduced blind rates.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass
from typing import TYPE_CHECKING

from config import (
    AUTO_BLIND_PERCENTAGE,
    CHARITY_CONTRIBUTION_CAP,
    CHARITY_GAMES_DURATION,
    CHARITY_MIN_TARGET_DEBT,
    CHARITY_MIN_TARGET_GAMES,
    CHARITY_REDUCED_RATE,
)

if TYPE_CHECKING:
    from repositories.charity_repository import CharityRepository

logger = logging.getLogger("cama_bot.charity")


@dataclass
class CharityState:
    """Current charity state for a player."""

    discord_id: int
    reduced_rate_games_remaining: int
    last_charity_at: int | None
    total_charity_given: int

    @property
    def has_reduced_rate(self) -> bool:
        """Check if player has reduced blind rate."""
        return self.reduced_rate_games_remaining > 0


class CharityService:
    """
    Service for tracking charitable contributions and granting reduced blind rates.

    When a player helps another pay off their debt (via /paydebt), they may
    earn a reduced blind bet rate for a number of games.
    """

    def __init__(
        self,
        charity_repo: CharityRepository,
        reduced_rate: float = CHARITY_REDUCED_RATE,
        games_duration: int = CHARITY_GAMES_DURATION,
        min_target_debt: int = CHARITY_MIN_TARGET_DEBT,
        min_target_games: int = CHARITY_MIN_TARGET_GAMES,
        contribution_cap: int = CHARITY_CONTRIBUTION_CAP,
    ):
        self.charity_repo = charity_repo
        self.reduced_rate = reduced_rate
        self.games_duration = games_duration
        self.min_target_debt = min_target_debt
        self.min_target_games = min_target_games
        self.contribution_cap = contribution_cap

    def get_state(self, discord_id: int) -> CharityState:
        """Get charity state for a player."""
        state = self.charity_repo.get_state(discord_id)
        if not state:
            return CharityState(
                discord_id=discord_id,
                reduced_rate_games_remaining=0,
                last_charity_at=None,
                total_charity_given=0,
            )
        return CharityState(
            discord_id=discord_id,
            reduced_rate_games_remaining=state["reduced_rate_games_remaining"],
            last_charity_at=state["last_charity_at"],
            total_charity_given=state["total_charity_given"],
        )

    def check_paydebt_qualifies(
        self,
        from_id: int,
        to_id: int,
        amount_paid: int,
        target_debt_before: int,
        target_games_played: int,
    ) -> dict:
        """
        Check if a /paydebt transaction qualifies for charity reward.

        Args:
            from_id: Player who paid (not used for validation, but for logging)
            to_id: Player whose debt was paid
            amount_paid: Amount actually paid
            target_debt_before: Target's debt before payment (positive number)
            target_games_played: Number of games the target has played

        Returns:
            {
                "qualifies": bool,
                "reason": str | None,  # Reason for not qualifying
            }
        """
        # Check minimum target debt
        if target_debt_before < self.min_target_debt:
            return {
                "qualifies": False,
                "reason": f"Target debt ({target_debt_before}) below minimum ({self.min_target_debt})",
            }

        # Check minimum target games
        if target_games_played < self.min_target_games:
            return {
                "qualifies": False,
                "reason": f"Target has only {target_games_played} games (minimum: {self.min_target_games})",
            }

        # Calculate threshold: min(target_debt, contribution_cap)
        threshold = min(target_debt_before, self.contribution_cap)

        # Check if amount paid meets threshold
        if amount_paid < threshold:
            return {
                "qualifies": False,
                "reason": f"Paid {amount_paid} but need {threshold} to qualify",
            }

        return {"qualifies": True, "reason": None}

    def grant_charity_reward(self, discord_id: int, amount: int) -> None:
        """
        Grant charity reward (reduced blind rate) to a player.

        Args:
            discord_id: Player to reward
            amount: Amount they donated (for tracking)
        """
        self.charity_repo.grant_reduced_rate(
            discord_id=discord_id,
            games=self.games_duration,
            amount=amount,
            max_games=self.games_duration,  # Cap at games_duration (no stacking)
        )
        logger.info(
            f"Granted {self.games_duration} games of reduced blind rate to {discord_id} "
            f"for {amount} charity"
        )

    def get_blind_rate_for_player(self, discord_id: int) -> float:
        """
        Get the blind bet rate for a player.

        Returns reduced rate if player has remaining charity games,
        otherwise returns normal AUTO_BLIND_PERCENTAGE.
        """
        games_remaining = self.charity_repo.get_reduced_rate_games(discord_id)
        if games_remaining > 0:
            return self.reduced_rate
        return AUTO_BLIND_PERCENTAGE

    def has_reduced_rate(self, discord_id: int) -> bool:
        """Check if player currently has reduced blind rate."""
        return self.charity_repo.get_reduced_rate_games(discord_id) > 0

    def on_blind_bet_created(self, discord_id: int) -> int:
        """
        Called after a blind bet is created for a player.

        Decrements their reduced rate games remaining.

        Returns:
            New games remaining count.
        """
        return self.charity_repo.decrement_games_remaining(discord_id)
