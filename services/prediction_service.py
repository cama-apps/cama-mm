"""
Handles prediction market business logic.
"""

import time
from typing import Any

from repositories.interfaces import IPredictionRepository
from repositories.player_repository import PlayerRepository


class PredictionService:
    """
    Encapsulates prediction market operations:
    - Creating predictions
    - Placing bets
    - Resolution voting
    - Settlement
    """

    MIN_RESOLUTION_VOTES = 3  # Same threshold as match recording

    def __init__(
        self,
        prediction_repo: IPredictionRepository,
        player_repo: PlayerRepository,
        admin_user_ids: list[int] | None = None,
    ):
        self.prediction_repo = prediction_repo
        self.player_repo = player_repo
        self.admin_user_ids = set(admin_user_ids or [])

    def is_admin(self, user_id: int) -> bool:
        """Check if user is an admin."""
        return user_id in self.admin_user_ids

    def create_prediction(
        self,
        guild_id: int,
        creator_id: int,
        question: str,
        closes_at: int,
        channel_id: int | None = None,
    ) -> dict[str, Any]:
        """
        Create a new prediction market.

        Args:
            guild_id: Discord guild ID
            creator_id: Discord ID of the creator
            question: The prediction question
            closes_at: Unix timestamp when betting closes
            channel_id: Discord channel where created

        Returns:
            Dict with prediction_id and details
        """
        if not question or len(question.strip()) < 5:
            raise ValueError("Question must be at least 5 characters.")

        now = int(time.time())
        if closes_at <= now:
            raise ValueError("Close time must be in the future.")

        # Minimum 1 minute betting window
        if closes_at - now < 60:
            raise ValueError("Betting window must be at least 1 minute.")

        prediction_id = self.prediction_repo.create_prediction(
            guild_id=guild_id,
            creator_id=creator_id,
            question=question.strip(),
            closes_at=closes_at,
            channel_id=channel_id,
        )

        return {
            "prediction_id": prediction_id,
            "question": question.strip(),
            "closes_at": closes_at,
            "creator_id": creator_id,
        }

    def update_discord_ids(
        self,
        prediction_id: int,
        thread_id: int | None = None,
        embed_message_id: int | None = None,
        channel_message_id: int | None = None,
        close_message_id: int | None = None,
    ) -> None:
        """Update Discord message/thread IDs for a prediction."""
        self.prediction_repo.update_prediction_discord_ids(
            prediction_id=prediction_id,
            thread_id=thread_id,
            embed_message_id=embed_message_id,
            channel_message_id=channel_message_id,
            close_message_id=close_message_id,
        )

    def place_bet(
        self,
        prediction_id: int,
        discord_id: int,
        position: str,
        amount: int,
    ) -> dict[str, Any]:
        """
        Place a bet on a prediction.

        Args:
            prediction_id: ID of the prediction
            discord_id: Discord ID of the bettor
            position: "yes" or "no"
            amount: Amount to bet

        Returns:
            Dict with bet details and updated odds
        """
        result = self.prediction_repo.place_bet_atomic(
            prediction_id=prediction_id,
            discord_id=discord_id,
            position=position,
            amount=amount,
        )

        # Calculate odds
        odds = self.calculate_odds(result["yes_total"], result["no_total"])
        result["odds"] = odds

        return result

    def calculate_odds(self, yes_total: int, no_total: int) -> dict[str, float]:
        """
        Calculate pool odds for YES and NO.

        Returns multipliers for each position.
        """
        total = yes_total + no_total
        if total == 0:
            return {"yes": 0.0, "no": 0.0}

        yes_odds = total / yes_total if yes_total > 0 else 0.0
        no_odds = total / no_total if no_total > 0 else 0.0

        return {"yes": round(yes_odds, 2), "no": round(no_odds, 2)}

    def get_odds(self, prediction_id: int) -> dict[str, Any]:
        """
        Get current odds and totals for a prediction.

        Returns dict with yes_total, no_total, yes_odds, no_odds, etc.
        """
        totals = self.prediction_repo.get_prediction_totals(prediction_id)
        odds = self.calculate_odds(totals["yes_total"], totals["no_total"])

        return {
            **totals,
            "odds": odds,
            "total_pool": totals["yes_total"] + totals["no_total"],
        }

    def get_prediction(self, prediction_id: int) -> dict | None:
        """Get a prediction by ID with current odds."""
        pred = self.prediction_repo.get_prediction(prediction_id)
        if not pred:
            return None

        totals = self.prediction_repo.get_prediction_totals(prediction_id)
        odds = self.calculate_odds(totals["yes_total"], totals["no_total"])

        return {
            **pred,
            **totals,
            "odds": odds,
            "total_pool": totals["yes_total"] + totals["no_total"],
        }

    def get_active_predictions(self, guild_id: int) -> list[dict]:
        """Get all active predictions for a guild with odds."""
        # First, auto-lock any expired predictions
        self.check_and_lock_expired(guild_id)

        predictions = self.prediction_repo.get_active_predictions(guild_id)
        result = []
        now = int(time.time())
        for pred in predictions:
            # Skip cancelled predictions (shouldn't happen but be safe)
            if pred["status"] == "cancelled":
                continue
            totals = self.prediction_repo.get_prediction_totals(pred["prediction_id"])
            odds = self.calculate_odds(totals["yes_total"], totals["no_total"])
            result.append({
                **pred,
                **totals,
                "odds": odds,
                "total_pool": totals["yes_total"] + totals["no_total"],
            })
        return result

    def get_user_position(self, prediction_id: int, discord_id: int) -> dict | None:
        """Get user's position on a prediction."""
        return self.prediction_repo.get_user_bet_on_prediction(prediction_id, discord_id)

    def get_user_active_positions(self, discord_id: int, guild_id: int | None = None) -> list[dict]:
        """Get all active positions for a user."""
        return self.prediction_repo.get_user_active_positions(discord_id, guild_id)

    def get_user_resolved_positions(self, discord_id: int, guild_id: int | None = None, limit: int = 20) -> list[dict]:
        """Get user's resolved positions with payout info."""
        return self.prediction_repo.get_user_resolved_positions(discord_id, guild_id, limit)

    def add_resolution_vote(
        self,
        prediction_id: int,
        user_id: int,
        outcome: str,
        is_admin: bool | None = None,
    ) -> dict[str, Any]:
        """
        Add a resolution vote for a prediction.

        Args:
            prediction_id: ID of the prediction
            user_id: Discord ID of the voter
            outcome: "yes" or "no"
            is_admin: Override admin status (if None, checks internal list)

        Returns:
            Dict with vote counts and resolution status
        """
        pred = self.prediction_repo.get_prediction(prediction_id)
        if not pred:
            raise ValueError("Prediction not found.")

        if pred["status"] == "resolved":
            raise ValueError("This prediction has already been resolved.")
        if pred["status"] == "cancelled":
            raise ValueError("This prediction was cancelled.")

        # Check if betting period has closed
        now = int(time.time())
        if now < pred["closes_at"]:
            raise ValueError("Cannot vote until betting period closes.")

        # Use provided is_admin or check internal list
        if is_admin is None:
            is_admin = self.is_admin(user_id)

        vote_result = self.prediction_repo.add_resolution_vote(
            prediction_id=prediction_id,
            user_id=user_id,
            outcome=outcome,
            is_admin=is_admin,
        )

        # Check if we can resolve
        can_resolve = self._check_can_resolve(vote_result, outcome)

        return {
            **vote_result,
            "can_resolve": can_resolve,
            "votes_needed": self.MIN_RESOLUTION_VOTES,
            "is_admin": is_admin,
        }

    def _check_can_resolve(self, vote_result: dict, voted_outcome: str) -> bool:
        """Check if prediction can be resolved based on votes."""
        # Admin vote immediately resolves
        if vote_result.get("has_admin_vote"):
            return True

        # Need MIN_RESOLUTION_VOTES matching votes
        count = vote_result.get(f"{voted_outcome}_count", 0)
        return count >= self.MIN_RESOLUTION_VOTES

    def get_resolution_votes(self, prediction_id: int) -> dict:
        """Get current resolution vote counts."""
        return self.prediction_repo.get_resolution_votes(prediction_id)

    def can_resolve(self, prediction_id: int, outcome: str | None = None) -> bool:
        """
        Check if a prediction can be resolved.

        If outcome is provided, checks if that specific outcome has enough votes.
        Otherwise, checks if any outcome has enough votes.
        """
        pred = self.prediction_repo.get_prediction(prediction_id)
        if not pred:
            return False
        if pred["status"] != "open" and pred["status"] != "locked":
            return False

        votes = self.prediction_repo.get_resolution_votes(prediction_id)

        if outcome:
            return votes.get(outcome, 0) >= self.MIN_RESOLUTION_VOTES

        return (
            votes.get("yes", 0) >= self.MIN_RESOLUTION_VOTES
            or votes.get("no", 0) >= self.MIN_RESOLUTION_VOTES
        )

    def get_pending_outcome(self, prediction_id: int) -> str | None:
        """
        Get the outcome that has reached threshold, if any.

        Returns "yes", "no", or None.
        """
        votes = self.prediction_repo.get_resolution_votes(prediction_id)

        if votes.get("yes", 0) >= self.MIN_RESOLUTION_VOTES:
            return "yes"
        if votes.get("no", 0) >= self.MIN_RESOLUTION_VOTES:
            return "no"
        return None

    def resolve(
        self,
        prediction_id: int,
        outcome: str,
        resolved_by: int,
    ) -> dict[str, Any]:
        """
        Resolve a prediction and settle all bets.

        Args:
            prediction_id: ID of the prediction
            outcome: "yes" or "no"
            resolved_by: Discord ID of user who triggered resolution

        Returns:
            Dict with settlement results (winners, losers, payouts)
        """
        pred = self.prediction_repo.get_prediction(prediction_id)
        if not pred:
            raise ValueError("Prediction not found.")
        if pred["status"] == "resolved":
            raise ValueError("Prediction already resolved.")
        if pred["status"] == "cancelled":
            raise ValueError("Prediction was cancelled.")

        # Mark as resolved
        self.prediction_repo.resolve_prediction(
            prediction_id=prediction_id,
            outcome=outcome,
            resolved_by=resolved_by,
        )

        # Calculate consensus before settlement (for easter egg hooks)
        totals = self.prediction_repo.get_prediction_totals(prediction_id)
        yes_total = totals.get("yes_total", 0) or 0
        no_total = totals.get("no_total", 0) or 0
        total_pool = yes_total + no_total
        consensus_data = None
        if total_pool > 0:
            winning_total = yes_total if outcome == "yes" else no_total
            losing_total = no_total if outcome == "yes" else yes_total
            losing_pct = (losing_total / total_pool) * 100
            # If 90%+ bet on the losing side, flag as unanimous wrong
            if losing_pct >= 90:
                loser_count = totals.get("yes_bettors", 0) if outcome == "no" else totals.get("no_bettors", 0)
                consensus_data = {
                    "consensus_percentage": losing_pct,
                    "winning_side": outcome,
                    "loser_count": loser_count or 0,
                }

        # Settle bets
        settlement = self.prediction_repo.settle_prediction_bets(
            prediction_id=prediction_id,
            winning_position=outcome,
        )

        return {
            "prediction_id": prediction_id,
            "outcome": outcome,
            "resolved_by": resolved_by,
            "unanimous_wrong": consensus_data,
            **settlement,
        }

    def cancel(self, prediction_id: int, admin_id: int) -> dict[str, Any]:
        """
        Cancel a prediction and refund all bets.

        Only admins can cancel predictions (checks internal admin list).

        Args:
            prediction_id: ID of the prediction
            admin_id: Discord ID of the admin

        Returns:
            Dict with refund results
        """
        if not self.is_admin(admin_id):
            raise ValueError("Only admins can cancel predictions.")

        return self.cancel_by_admin(prediction_id, admin_id)

    def cancel_by_admin(self, prediction_id: int, admin_id: int) -> dict[str, Any]:
        """
        Cancel a prediction and refund all bets (no admin check).

        Use this when admin permission has already been verified externally.

        Args:
            prediction_id: ID of the prediction
            admin_id: Discord ID of the admin

        Returns:
            Dict with refund results
        """
        pred = self.prediction_repo.get_prediction(prediction_id)
        if not pred:
            raise ValueError("Prediction not found.")
        if pred["status"] != "open":
            raise ValueError(f"Can only cancel open predictions. This one is {pred['status']}.")

        # Cancel prediction
        self.prediction_repo.cancel_prediction(prediction_id)

        # Refund bets
        refund_result = self.prediction_repo.refund_prediction_bets(prediction_id)

        return {
            "prediction_id": prediction_id,
            "cancelled_by": admin_id,
            **refund_result,
        }

    def check_and_lock_expired(self, guild_id: int) -> list[int]:
        """
        Check for predictions past their close time and lock them.

        Returns list of prediction IDs that were locked.
        """
        now = int(time.time())
        predictions = self.prediction_repo.get_predictions_by_status(guild_id, "open")
        locked = []

        for pred in predictions:
            if pred["closes_at"] <= now:
                self.prediction_repo.update_prediction_status(
                    pred["prediction_id"], "locked"
                )
                locked.append(pred["prediction_id"])

        return locked

    def close_betting_early(self, prediction_id: int) -> dict[str, Any]:
        """
        Close betting on a prediction early (admin action).

        Returns prediction info.
        """
        pred = self.prediction_repo.get_prediction(prediction_id)
        if not pred:
            raise ValueError("Prediction not found.")
        if pred["status"] != "open":
            raise ValueError(f"Prediction is already {pred['status']}.")

        # Lock and set closes_at to now so resolution voting can proceed
        now = int(time.time())
        self.prediction_repo.close_prediction_betting(prediction_id, now)

        return {
            "prediction_id": prediction_id,
            "question": pred["question"],
            "status": "locked",
        }

    def get_prediction_leaderboard(self, guild_id: int | None, limit: int = 10) -> dict:
        """
        Get prediction leaderboard data.

        Args:
            guild_id: Guild ID to filter by
            limit: Maximum number of entries per category

        Returns:
            Dict with top_earners and down_bad lists
        """
        return self.prediction_repo.get_prediction_leaderboard(guild_id, limit)

    def get_server_prediction_stats(self, guild_id: int | None) -> dict:
        """
        Get server-wide prediction stats.

        Args:
            guild_id: Guild ID to filter by

        Returns:
            Dict with total_predictions, total_pool, total_bets, etc.
        """
        return self.prediction_repo.get_server_prediction_stats(guild_id)

    def get_user_prediction_stats(self, discord_id: int, guild_id: int | None = None) -> dict | None:
        """
        Get prediction statistics for a specific user.

        Args:
            discord_id: User's Discord ID
            guild_id: Guild ID to filter by

        Returns:
            Dict with user's prediction stats or None if no stats found
        """
        return self.prediction_repo.get_user_prediction_stats(discord_id, guild_id)

    def get_predictions_by_status(self, guild_id: int, status: str) -> list[dict]:
        """
        Get predictions filtered by status.

        Args:
            guild_id: Guild ID
            status: Status to filter by (open, locked, resolved)

        Returns:
            List of prediction dicts
        """
        return self.prediction_repo.get_predictions_by_status(guild_id, status)

    def get_prediction_totals(self, prediction_id: int) -> dict:
        """
        Get bet totals for a prediction.

        Args:
            prediction_id: Prediction ID

        Returns:
            Dict with yes_total, no_total, etc.
        """
        return self.prediction_repo.get_prediction_totals(prediction_id)

    def get_resolution_summary(self, prediction_id: int) -> dict:
        """
        Get resolution vote summary for a prediction.

        Args:
            prediction_id: Prediction ID

        Returns:
            Dict with vote counts and voters
        """
        return self.prediction_repo.get_resolution_summary(prediction_id)
