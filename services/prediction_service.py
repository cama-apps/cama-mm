"""
Handles prediction market business logic.
"""

import random
import time
from typing import Any

from config import (
    PREDICTION_CONTRACT_VALUE,
    PREDICTION_DRIFT_MAX,
    PREDICTION_DRIFT_MIN,
    PREDICTION_FADE_TICKS,
    PREDICTION_INITIAL_FAIR_DEFAULT,
    PREDICTION_LEVELS_PER_SIDE,
    PREDICTION_PRICE_HIGH,
    PREDICTION_PRICE_LOW,
    PREDICTION_REFRESH_LEVELS_PER_SIDE,
    PREDICTION_REFRESH_SECONDS,
    PREDICTION_REFRESH_SIZE_PER_LEVEL,
    PREDICTION_REFRESH_SPREAD_TICKS,
    PREDICTION_SIZE_PER_LEVEL,
    PREDICTION_SPREAD_TICKS,
    PREDICTION_TICK_SIZE,
)
from repositories.interfaces import IPredictionRepository
from repositories.player_repository import PlayerRepository


class PredictionService:
    """
    Encapsulates order-book prediction market operations:
    - Creating markets and posting ladders
    - Buying/selling contracts
    - Resolution voting
    - Settlement
    """

    MIN_RESOLUTION_VOTES = 3  # Same threshold as match recording

    def __init__(
        self,
        prediction_repo: IPredictionRepository,
        player_repo: PlayerRepository,
        admin_user_ids: list[int] | None = None,
        bankruptcy_service=None,
    ):
        self.prediction_repo = prediction_repo
        self.player_repo = player_repo
        self.admin_user_ids = set(admin_user_ids or [])
        self.bankruptcy_service = bankruptcy_service

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

    def get_prediction(self, prediction_id: int) -> dict | None:
        return self.prediction_repo.get_prediction(prediction_id)

    def get_predictions_by_status(self, guild_id: int, status: str) -> list[dict]:
        return self.prediction_repo.get_predictions_by_status(guild_id, status)

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

    # =========================================================================
    # Order-book mechanic (feat/predict-orderbook)
    # =========================================================================

    @staticmethod
    def _build_initial_levels(
        fair: int,
        *,
        levels_per_side: int | None = None,
        size_per_level: int | None = None,
        spread_ticks: int | None = None,
    ) -> list[tuple[str, int, int]]:
        """Construct a ladder of asks and bids around ``fair``.

        Defaults to the *initial-seed* params (`PREDICTION_LEVELS_PER_SIDE`,
        `_SIZE_PER_LEVEL`, `_SPREAD_TICKS`). Pass overrides to use the *refresh*
        params (smaller and wider) — the daily-refresh worker calls with those
        so the layered depth sits further from fair than the initial book.
        Levels outside ``{1..99}`` are dropped (the price clamp prevents this
        in practice).
        """
        n_levels = (
            levels_per_side if levels_per_side is not None else PREDICTION_LEVELS_PER_SIDE
        )
        size = (
            size_per_level if size_per_level is not None else PREDICTION_SIZE_PER_LEVEL
        )
        spread = (
            spread_ticks if spread_ticks is not None else PREDICTION_SPREAD_TICKS
        )
        levels: list[tuple[str, int, int]] = []
        for k in range(0, n_levels):
            ask_price = fair + (spread + k) * PREDICTION_TICK_SIZE
            bid_price = fair - (spread + k) * PREDICTION_TICK_SIZE
            if 1 <= ask_price <= 99:
                levels.append(("yes_ask", ask_price, size))
            if 1 <= bid_price <= 99:
                levels.append(("yes_bid", bid_price, size))
        return levels

    @staticmethod
    def clamp_price(price: int) -> int:
        return max(PREDICTION_PRICE_LOW, min(PREDICTION_PRICE_HIGH, price))

    def create_orderbook_prediction(
        self,
        guild_id: int,
        creator_id: int,
        question: str,
        initial_fair: int | None = None,
        channel_id: int | None = None,
    ) -> dict[str, Any]:
        """Create a new order-book market and post the initial ladder.

        Both the market row and its initial book go in via a single repo call
        so the market never lands as ``status='open'`` with an empty book.
        """
        if not question or len(question.strip()) < 5:
            raise ValueError("Question must be at least 5 characters.")
        if initial_fair is None:
            initial_fair = PREDICTION_INITIAL_FAIR_DEFAULT
        if not (PREDICTION_PRICE_LOW <= initial_fair <= PREDICTION_PRICE_HIGH):
            raise ValueError(
                f"initial_fair must be in [{PREDICTION_PRICE_LOW}, {PREDICTION_PRICE_HIGH}]."
            )

        levels = self._build_initial_levels(initial_fair)
        prediction_id = self.prediction_repo.create_orderbook_prediction(
            guild_id=guild_id,
            creator_id=creator_id,
            question=question.strip(),
            initial_fair=initial_fair,
            channel_id=channel_id,
            initial_levels=levels,
        )

        return {
            "prediction_id": prediction_id,
            "question": question.strip(),
            "creator_id": creator_id,
            "initial_fair": initial_fair,
            "current_price": initial_fair,
        }

    def buy_contracts(
        self, prediction_id: int, discord_id: int, side: str, contracts: int
    ) -> dict[str, Any]:
        return self.prediction_repo.buy_contracts_atomic(
            prediction_id=prediction_id,
            discord_id=discord_id,
            side=side,
            contracts=contracts,
        )

    def sell_contracts(
        self, prediction_id: int, discord_id: int, side: str, contracts: int
    ) -> dict[str, Any]:
        return self.prediction_repo.sell_contracts_atomic(
            prediction_id=prediction_id,
            discord_id=discord_id,
            side=side,
            contracts=contracts,
        )

    def get_market_view(
        self, prediction_id: int, viewer_id: int | None = None
    ) -> dict | None:
        """Bundle everything the embed / view command needs.

        Returns: prediction row + book + recent trades + (optional) viewer position.
        """
        pred = self.prediction_repo.get_prediction(prediction_id)
        if not pred:
            return None
        book = self.prediction_repo.get_book(prediction_id)
        recent = self.prediction_repo.get_recent_trades(prediction_id, limit=5)
        position = (
            self.prediction_repo.get_position(prediction_id, viewer_id)
            if viewer_id is not None
            else None
        )
        # Volume since last refresh window
        since = pred.get("last_refresh_at") or 0
        summary = self.prediction_repo.get_trade_summary_since(prediction_id, since)
        return {
            **pred,
            "book": book,
            "recent_trades": recent,
            "viewer_position": position,
            "volume_since_refresh": summary.get("total_volume", 0),
        }

    def get_user_position(self, prediction_id: int, discord_id: int) -> dict | None:
        return self.prediction_repo.get_position(prediction_id, discord_id)

    def get_user_open_positions(
        self, discord_id: int, guild_id: int | None = None
    ) -> list[dict]:
        return self.prediction_repo.get_user_open_positions(discord_id, guild_id)

    def get_user_orderbook_stats(
        self, discord_id: int, guild_id: int | None = None
    ) -> dict:
        return self.prediction_repo.get_user_orderbook_stats(discord_id, guild_id)

    def list_open_orderbook_markets(self, guild_id: int) -> list[dict]:
        return self.prediction_repo.get_open_orderbook_predictions(guild_id)

    def refresh_market(self, prediction_id: int) -> dict:
        """Drift fair toward observed mid + small uniform integer drift; repost ladder.

        Returns the refresh summary plus the trade aggregation since last refresh
        so the caller can post the daily-summary message.
        """
        pred = self.prediction_repo.get_prediction(prediction_id)
        if not pred or pred["status"] != "open":
            return {"skipped": True, "reason": "not open"}

        book = self.prediction_repo.get_book(prediction_id)
        old_price = int(pred.get("current_price") or PREDICTION_INITIAL_FAIR_DEFAULT)
        prev_refresh = int(pred.get("last_refresh_at") or 0)
        asks = book["yes_asks"]
        bids = book["yes_bids"]
        if asks and bids:
            observed_mid = (asks[0][0] + bids[0][0]) / 2
        elif bids and not asks:
            # Asks fully consumed: anchor from the last lifted ask, if recorded.
            last_lifted_ask = self.prediction_repo.get_last_fill_price_since(
                prediction_id, ["buy_yes", "sell_no"], prev_refresh
            )
            observed_mid = (last_lifted_ask or bids[0][0]) + PREDICTION_FADE_TICKS
        elif asks and not bids:
            # Bids fully consumed: anchor from the last hit bid, if recorded.
            last_hit_bid = self.prediction_repo.get_last_fill_price_since(
                prediction_id, ["buy_no", "sell_yes"], prev_refresh
            )
            observed_mid = (last_hit_bid or asks[0][0]) - PREDICTION_FADE_TICKS
        else:
            last_lifted_ask = self.prediction_repo.get_last_fill_price_since(
                prediction_id, ["buy_yes", "sell_no"], prev_refresh
            )
            last_hit_bid = self.prediction_repo.get_last_fill_price_since(
                prediction_id, ["buy_no", "sell_yes"], prev_refresh
            )
            if last_lifted_ask is not None and last_hit_bid is not None:
                observed_mid = (last_lifted_ask + last_hit_bid) / 2
            elif last_lifted_ask is not None:
                observed_mid = last_lifted_ask + PREDICTION_FADE_TICKS
            elif last_hit_bid is not None:
                observed_mid = last_hit_bid - PREDICTION_FADE_TICKS
            else:
                observed_mid = old_price

        drift = random.randint(PREDICTION_DRIFT_MIN, PREDICTION_DRIFT_MAX)
        new_price = self.clamp_price(round(observed_mid) + drift)

        # Daily refresh layers thinner / wider than the initial seed: fewer
        # levels, smaller per-level size, larger spread offset. Legacy quotes
        # inside the current minimum spread are pruned while crossing arb stays.
        levels = self._build_initial_levels(
            new_price,
            levels_per_side=PREDICTION_REFRESH_LEVELS_PER_SIDE,
            size_per_level=PREDICTION_REFRESH_SIZE_PER_LEVEL,
            spread_ticks=PREDICTION_REFRESH_SPREAD_TICKS,
        )
        now = int(time.time())
        self.prediction_repo.apply_refresh(
            prediction_id,
            new_price,
            levels,
            now,
            min_quote_offset=PREDICTION_SPREAD_TICKS * PREDICTION_TICK_SIZE,
        )

        summary = self.prediction_repo.get_trade_summary_since(
            prediction_id, prev_refresh
        )
        return {
            "skipped": False,
            "prediction_id": prediction_id,
            "old_price": old_price,
            "new_price": new_price,
            "drift": drift,
            "trade_summary": summary,
        }

    def get_markets_due_for_refresh(self, now_ts: int | None = None) -> list[dict]:
        if now_ts is None:
            now_ts = int(time.time())
        return self.prediction_repo.get_markets_due_for_refresh(
            PREDICTION_REFRESH_SECONDS, now_ts
        )

    def resolve_orderbook(
        self, prediction_id: int, outcome: str, resolved_by: int | None = None
    ) -> dict:
        """Atomic settle: cancel levels, pay contract holders, mark resolved.

        Applies the bankruptcy debuff to each winner's profit as a follow-up
        debit: the gross payout is credited inside the atomic settlement, then
        the penalty share of profit (payout − cost basis) is docked here as a
        coin sink. Stake (cost basis) is always returned whole.
        """
        # The bankruptcy debuff is folded into the settlement txn: the penalty
        # share of each penalized winner's profit is netted out of their credit
        # there (no follow-up debit with a crash window). Winners returned
        # already carry netted payout/profit and a ``bankruptcy_penalty`` field.
        return self.prediction_repo.settle_prediction_orderbook(
            prediction_id, outcome, resolved_by=resolved_by,
            bankruptcy_penalty_rate=(
                self.bankruptcy_service.penalty_rate if self.bankruptcy_service else None
            ),
        )

    def rollback_orderbook(
        self,
        prediction_id: int,
        guild_id: int | None,
        rolled_back_by: int | None = None,
    ) -> dict:
        """Reverse a settlement and reopen the market with a fresh ladder."""
        pred = self.prediction_repo.get_prediction(prediction_id)
        if not pred:
            raise ValueError("Prediction not found.")
        if pred.get("current_price") is None:
            raise ValueError("Prediction has no current price.")
        levels = self._build_initial_levels(int(pred["current_price"]))
        return self.prediction_repo.rollback_prediction_orderbook(
            prediction_id,
            guild_id,
            levels,
            rolled_back_by=rolled_back_by,
        )

    def cancel_orderbook(self, prediction_id: int) -> dict:
        """Cost-basis refund. Same admin gating enforced at the command layer."""
        return self.prediction_repo.cancel_orderbook_prediction(prediction_id)

    def set_fair_manual(self, prediction_id: int, new_price: int) -> dict:
        """Admin override: stamp a new fair and layer a fresh ladder around it.

        Skips the random drift / observed-mid logic of the daily refresh — admin
        is explicitly saying "the market should be at this price now". Uses the
        same `apply_refresh` path so layering applies. Crossing leftovers from
        prior flow are deliberately preserved as arb opportunities.
        """
        if not (PREDICTION_PRICE_LOW <= new_price <= PREDICTION_PRICE_HIGH):
            raise ValueError(
                f"new_price must be in [{PREDICTION_PRICE_LOW}, {PREDICTION_PRICE_HIGH}]."
            )
        pred = self.prediction_repo.get_prediction(prediction_id)
        if not pred:
            raise ValueError("Prediction not found.")
        if pred.get("status") != "open":
            raise ValueError(
                f"Cannot set fair on a {pred.get('status')} market."
            )
        old_price = int(pred.get("current_price") or PREDICTION_INITIAL_FAIR_DEFAULT)
        levels = self._build_initial_levels(new_price)
        now = int(time.time())
        self.prediction_repo.apply_refresh(
            prediction_id,
            new_price,
            levels,
            now,
            reason="set_fair",
            min_quote_offset=PREDICTION_SPREAD_TICKS * PREDICTION_TICK_SIZE,
        )
        return {
            "prediction_id": prediction_id,
            "old_price": old_price,
            "new_price": new_price,
        }

    @staticmethod
    def position_mark(book: dict, side: str) -> int | None:
        """Compute the mark price for a held position.

        YES holdings are marked at the top YES bid (what you'd net selling now).
        NO holdings are marked at ``100 - top YES ask`` (the top NO bid).
        """
        if side == "yes":
            bids = book.get("yes_bids", [])
            return bids[0][0] if bids else None
        if side == "no":
            asks = book.get("yes_asks", [])
            return (100 - asks[0][0]) if asks else None
        raise ValueError("side must be 'yes' or 'no'")

    @staticmethod
    def contract_value() -> int:
        return PREDICTION_CONTRACT_VALUE
