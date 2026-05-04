"""
Repository for managing prediction market data.
"""

from __future__ import annotations

import json
import math
import time

from repositories.base_repository import BaseRepository
from repositories.interfaces import IPredictionRepository


def _quote_total(raw_jopa_x10: int, kind: str) -> int:
    """Convert a price-weighted qty (in % units) to integer jopa.

    Contract value is 10 jopa per winning contract, so the per-trade jopa cost
    is `sum(price_pct * qty) / 10`. Rounding favors the house: buys ceil at the
    half-tick, sells floor at the half-tick. Buys carry a 1-jopa floor so any
    non-zero trade has non-zero cost; sells have no floor (zero proceeds is OK
    when the user is voluntarily closing).
    """
    if raw_jopa_x10 <= 0:
        return 0
    if kind == "buy":
        return max(1, (raw_jopa_x10 + 5) // 10)
    if kind == "sell":
        return (raw_jopa_x10 + 4) // 10
    raise ValueError(f"unknown kind: {kind}")


class PredictionRepository(BaseRepository, IPredictionRepository):
    """
    Handles CRUD operations for predictions and prediction_bets tables.
    """

    VALID_POSITIONS = {"yes", "no"}
    VALID_STATUSES = {"open", "locked", "resolved", "cancelled"}

    def create_prediction(
        self,
        guild_id: int,
        creator_id: int,
        question: str,
        closes_at: int,
        channel_id: int | None = None,
        thread_id: int | None = None,
        embed_message_id: int | None = None,
    ) -> int:
        """Create a new prediction and return its ID."""
        normalized_guild = self.normalize_guild_id(guild_id)
        created_at = int(time.time())

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO predictions (
                    guild_id, creator_id, question, status, channel_id,
                    thread_id, embed_message_id, created_at, closes_at
                )
                VALUES (?, ?, ?, 'open', ?, ?, ?, ?, ?)
                """,
                (
                    normalized_guild,
                    creator_id,
                    question,
                    channel_id,
                    thread_id,
                    embed_message_id,
                    created_at,
                    closes_at,
                ),
            )
            return cursor.lastrowid

    def get_prediction(self, prediction_id: int) -> dict | None:
        """Get a prediction by ID."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT * FROM predictions WHERE prediction_id = ?
                """,
                (prediction_id,),
            )
            row = cursor.fetchone()
            return dict(row) if row else None

    def get_active_predictions(self, guild_id: int) -> list[dict]:
        """Get all open/locked predictions for a guild."""
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT * FROM predictions
                WHERE guild_id = ? AND status IN ('open', 'locked')
                ORDER BY created_at DESC
                """,
                (normalized_guild,),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_predictions_by_status(self, guild_id: int, status: str) -> list[dict]:
        """Get predictions filtered by status."""
        if status not in self.VALID_STATUSES:
            raise ValueError(f"Invalid status: {status}")
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT * FROM predictions
                WHERE guild_id = ? AND status = ?
                ORDER BY created_at DESC
                """,
                (normalized_guild, status),
            )
            return [dict(row) for row in cursor.fetchall()]

    def update_prediction_status(self, prediction_id: int, status: str) -> None:
        """Update prediction status (open -> locked -> resolved/cancelled)."""
        if status not in self.VALID_STATUSES:
            raise ValueError(f"Invalid status: {status}")
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE predictions SET status = ? WHERE prediction_id = ?
                """,
                (status, prediction_id),
            )

    def close_prediction_betting(self, prediction_id: int, closes_at: int) -> None:
        """Lock a prediction and set closes_at to the given timestamp.

        Used to close betting early so resolution voting can proceed.
        """
        with self.connection() as conn:
            conn.execute(
                "UPDATE predictions SET status = 'locked', closes_at = ? WHERE prediction_id = ?",
                (closes_at, prediction_id),
            )

    def update_prediction_discord_ids(
        self,
        prediction_id: int,
        thread_id: int | None = None,
        embed_message_id: int | None = None,
        channel_message_id: int | None = None,
        close_message_id: int | None = None,
    ) -> None:
        """Update Discord IDs for a prediction (thread, embed message, channel message, close message)."""
        with self.connection() as conn:
            cursor = conn.cursor()
            if thread_id is not None:
                cursor.execute(
                    "UPDATE predictions SET thread_id = ? WHERE prediction_id = ?",
                    (thread_id, prediction_id),
                )
            if embed_message_id is not None:
                cursor.execute(
                    "UPDATE predictions SET embed_message_id = ? WHERE prediction_id = ?",
                    (embed_message_id, prediction_id),
                )
            if channel_message_id is not None:
                cursor.execute(
                    "UPDATE predictions SET channel_message_id = ? WHERE prediction_id = ?",
                    (channel_message_id, prediction_id),
                )
            if close_message_id is not None:
                cursor.execute(
                    "UPDATE predictions SET close_message_id = ? WHERE prediction_id = ?",
                    (close_message_id, prediction_id),
                )

    def add_resolution_vote(
        self, prediction_id: int, user_id: int, outcome: str, is_admin: bool
    ) -> dict:
        """
        Add a resolution vote. Returns vote counts and admin status.

        Votes are stored as JSON in the resolution_votes column:
        {"user_id": {"outcome": "yes", "is_admin": false}, ...}
        """
        if outcome not in self.VALID_POSITIONS:
            raise ValueError(f"Invalid outcome: {outcome}")

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            # Get current votes
            cursor.execute(
                "SELECT resolution_votes FROM predictions WHERE prediction_id = ?",
                (prediction_id,),
            )
            row = cursor.fetchone()
            if not row:
                raise ValueError("Prediction not found.")

            votes_json = row["resolution_votes"]
            votes = json.loads(votes_json) if votes_json else {}

            # Check if user already voted differently
            user_key = str(user_id)
            existing = votes.get(user_key)
            if existing and existing.get("outcome") != outcome:
                raise ValueError("You already voted for a different outcome.")

            # Add/update vote
            votes[user_key] = {"outcome": outcome, "is_admin": is_admin}

            cursor.execute(
                "UPDATE predictions SET resolution_votes = ? WHERE prediction_id = ?",
                (json.dumps(votes), prediction_id),
            )

            # Calculate vote counts
            yes_count = sum(1 for v in votes.values() if v["outcome"] == "yes")
            no_count = sum(1 for v in votes.values() if v["outcome"] == "no")
            has_admin_vote = any(
                v["is_admin"] and v["outcome"] == outcome for v in votes.values()
            )

            return {
                "yes_count": yes_count,
                "no_count": no_count,
                "has_admin_vote": has_admin_vote,
                "voted_outcome": outcome,
            }

    def get_resolution_votes(self, prediction_id: int) -> dict:
        """Get current resolution vote counts: {"yes": n, "no": m}."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT resolution_votes FROM predictions WHERE prediction_id = ?",
                (prediction_id,),
            )
            row = cursor.fetchone()
            if not row:
                return {"yes": 0, "no": 0}

            votes_json = row["resolution_votes"]
            if not votes_json:
                return {"yes": 0, "no": 0}

            votes = json.loads(votes_json)
            yes_count = sum(1 for v in votes.values() if v["outcome"] == "yes")
            no_count = sum(1 for v in votes.values() if v["outcome"] == "no")
            return {"yes": yes_count, "no": no_count}

    def resolve_prediction(
        self, prediction_id: int, outcome: str, resolved_by: int
    ) -> None:
        """Mark prediction as resolved with outcome."""
        if outcome not in self.VALID_POSITIONS:
            raise ValueError(f"Invalid outcome: {outcome}")
        resolved_at = int(time.time())
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE predictions
                SET status = 'resolved', outcome = ?, resolved_at = ?, resolved_by = ?
                WHERE prediction_id = ?
                """,
                (outcome, resolved_at, resolved_by, prediction_id),
            )

    def cancel_prediction(self, prediction_id: int) -> None:
        """Cancel a prediction (status -> cancelled)."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE predictions SET status = 'cancelled' WHERE prediction_id = ?
                """,
                (prediction_id,),
            )

    def place_bet_atomic(
        self, prediction_id: int, discord_id: int, position: str, amount: int
    ) -> dict:
        """
        Place a bet atomically (debit balance, insert bet).

        - Validates prediction is open and not past closes_at
        - Checks user isn't betting on opposite side
        - Debits balance
        - Inserts bet

        Returns bet info including new totals.
        """
        if amount <= 0:
            raise ValueError("Bet amount must be positive.")
        if position not in self.VALID_POSITIONS:
            raise ValueError("Invalid position. Must be 'yes' or 'no'.")

        bet_time = int(time.time())

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            # Get prediction and validate
            cursor.execute(
                """
                SELECT status, closes_at, guild_id FROM predictions WHERE prediction_id = ?
                """,
                (prediction_id,),
            )
            pred = cursor.fetchone()
            if not pred:
                raise ValueError("Prediction not found.")
            if pred["status"] != "open":
                raise ValueError("Betting is closed for this prediction.")
            if bet_time >= pred["closes_at"]:
                raise ValueError("Betting period has ended.")

            # Get guild_id from prediction for player balance operations
            pred_guild_id = pred["guild_id"]

            # Check for existing bet on opposite side
            cursor.execute(
                """
                SELECT position FROM prediction_bets
                WHERE prediction_id = ? AND discord_id = ?
                """,
                (prediction_id, discord_id),
            )
            existing = cursor.fetchone()
            if existing and existing["position"] != position:
                raise ValueError(
                    f"You already have a position on {existing['position'].upper()}. "
                    "You can only add to your existing position."
                )

            # Check balance
            cursor.execute(
                "SELECT COALESCE(jopacoin_balance, 0) as balance FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, pred_guild_id),
            )
            player = cursor.fetchone()
            if not player:
                raise ValueError("Player not found. Please register first.")

            balance = int(player["balance"])
            if balance < 0:
                raise ValueError(
                    "You cannot place bets while in debt. Win some games to pay it off!"
                )
            if balance < amount:
                raise ValueError(f"Insufficient balance. You have {balance} jopacoin.")

            # Debit balance
            cursor.execute(
                """
                UPDATE players
                SET jopacoin_balance = jopacoin_balance - ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (amount, discord_id, pred_guild_id),
            )

            # Insert bet
            cursor.execute(
                """
                INSERT INTO prediction_bets (prediction_id, discord_id, position, amount, bet_time)
                VALUES (?, ?, ?, ?, ?)
                """,
                (prediction_id, discord_id, position, amount, bet_time),
            )
            bet_id = cursor.lastrowid

            # Get updated totals
            totals = self._get_totals_internal(cursor, prediction_id)

            return {
                "bet_id": bet_id,
                "amount": amount,
                "position": position,
                "new_balance": balance - amount,
                **totals,
            }

    def _get_totals_internal(self, cursor, prediction_id: int) -> dict:
        """Get bet totals using existing cursor (for use in transactions)."""
        cursor.execute(
            """
            SELECT
                position,
                SUM(amount) as total,
                COUNT(DISTINCT discord_id) as bettors
            FROM prediction_bets
            WHERE prediction_id = ?
            GROUP BY position
            """,
            (prediction_id,),
        )
        rows = cursor.fetchall()
        totals = {
            "yes_total": 0,
            "no_total": 0,
            "yes_count": 0,
            "no_count": 0,
        }
        for row in rows:
            pos = row["position"]
            totals[f"{pos}_total"] = row["total"]
            totals[f"{pos}_count"] = row["bettors"]
        return totals

    def get_prediction_bets(self, prediction_id: int) -> list[dict]:
        """Get all bets for a prediction."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT * FROM prediction_bets
                WHERE prediction_id = ?
                ORDER BY bet_time ASC
                """,
                (prediction_id,),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_user_bet_on_prediction(
        self, prediction_id: int, discord_id: int
    ) -> dict | None:
        """Get user's total bet on a specific prediction (aggregated if multiple bets)."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    position,
                    SUM(amount) as total_amount,
                    MIN(bet_time) as first_bet_time,
                    COUNT(*) as bet_count
                FROM prediction_bets
                WHERE prediction_id = ? AND discord_id = ?
                GROUP BY position
                """,
                (prediction_id, discord_id),
            )
            row = cursor.fetchone()
            return dict(row) if row else None

    def get_user_active_positions(self, discord_id: int, guild_id: int | None = None) -> list[dict]:
        """Get all active (unresolved) positions for a user."""
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    p.prediction_id,
                    p.question,
                    p.status,
                    p.closes_at,
                    pb.position,
                    SUM(pb.amount) as total_amount
                FROM prediction_bets pb
                JOIN predictions p ON pb.prediction_id = p.prediction_id
                WHERE pb.discord_id = ? AND p.guild_id = ? AND p.status IN ('open', 'locked')
                GROUP BY p.prediction_id, pb.position
                ORDER BY p.closes_at ASC
                """,
                (discord_id, normalized_guild),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_user_resolved_positions(self, discord_id: int, guild_id: int | None = None, limit: int = 20) -> list[dict]:
        """Get user's resolved positions with payout info."""
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    p.prediction_id,
                    p.question,
                    p.outcome,
                    p.resolved_at,
                    pb.position,
                    SUM(pb.amount) as total_amount,
                    MAX(pb.payout) as payout
                FROM prediction_bets pb
                JOIN predictions p ON pb.prediction_id = p.prediction_id
                WHERE pb.discord_id = ? AND p.guild_id = ? AND p.status = 'resolved'
                GROUP BY p.prediction_id, pb.position
                ORDER BY p.resolved_at DESC
                LIMIT ?
                """,
                (discord_id, normalized_guild, limit),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_player_prediction_history(
        self, discord_id: int, guild_id: int | None = None
    ) -> list[dict]:
        """
        Return every resolved prediction bet for ``discord_id`` in a guild.

        Each row is ``{prediction_id, settle_time, total_amount, payout, position, status}``
        where ``settle_time`` is ``resolved_at``. Cancelled predictions don't carry a
        ``resolved_at`` timestamp so they're excluded; refunded stakes never moved a
        net balance anyway. Rows are ordered by ``settle_time`` ascending.
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    p.prediction_id,
                    p.resolved_at as settle_time,
                    p.status,
                    pb.position,
                    SUM(pb.amount) as total_amount,
                    MAX(pb.payout) as payout
                FROM prediction_bets pb
                JOIN predictions p ON pb.prediction_id = p.prediction_id
                WHERE pb.discord_id = ?
                  AND p.guild_id = ?
                  AND p.status = 'resolved'
                  AND p.resolved_at IS NOT NULL
                GROUP BY p.prediction_id, pb.position
                ORDER BY p.resolved_at ASC
                """,
                (discord_id, normalized_guild),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_prediction_totals(self, prediction_id: int) -> dict:
        """Get bet totals: {"yes_total": n, "no_total": m, "yes_count": x, "no_count": y}."""
        with self.connection() as conn:
            cursor = conn.cursor()
            return self._get_totals_internal(cursor, prediction_id)

    def get_resolution_summary(self, prediction_id: int) -> dict:
        """Get summary of a resolved prediction: winners, losers, payouts."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    discord_id,
                    position,
                    SUM(amount) as total_bet,
                    MAX(payout) as payout
                FROM prediction_bets
                WHERE prediction_id = ?
                GROUP BY discord_id, position
                """,
                (prediction_id,),
            )
            bets = [dict(row) for row in cursor.fetchall()]

            winners = []
            losers = []
            total_pool = 0

            for bet in bets:
                total_pool += bet["total_bet"]
                if bet["payout"]:
                    profit = bet["payout"] - bet["total_bet"]
                    winners.append({
                        "discord_id": bet["discord_id"],
                        "bet": bet["total_bet"],
                        "payout": bet["payout"],
                        "profit": profit,
                    })
                else:
                    losers.append({
                        "discord_id": bet["discord_id"],
                        "bet": bet["total_bet"],
                    })

            return {
                "winners": winners,
                "losers": losers,
                "winner_count": len(winners),
                "loser_count": len(losers),
                "total_pool": total_pool,
            }

    def settle_prediction_bets(
        self, prediction_id: int, winning_position: str
    ) -> dict:
        """
        Settle all bets for a resolved prediction using pool mode.

        Winners get proportional share of the total pool.
        Returns payout summary.
        """
        if winning_position not in self.VALID_POSITIONS:
            raise ValueError(f"Invalid winning position: {winning_position}")

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            return self._settle_bets_internal(cursor, prediction_id, winning_position)

    def resolve_and_settle_atomic(
        self, prediction_id: int, outcome: str, resolved_by: int
    ) -> dict:
        """Atomically flip status to resolved and settle all bets.

        Fuses resolve_prediction + settle_prediction_bets inside one
        BEGIN IMMEDIATE so a crash can't leave a prediction marked resolved
        with no payouts distributed (or payouts without a status flip).
        """
        if outcome not in self.VALID_POSITIONS:
            raise ValueError(f"Invalid outcome: {outcome}")

        resolved_at = int(time.time())
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE predictions
                SET status = 'resolved', outcome = ?, resolved_at = ?, resolved_by = ?
                WHERE prediction_id = ?
                """,
                (outcome, resolved_at, resolved_by, prediction_id),
            )
            return self._settle_bets_internal(cursor, prediction_id, outcome)

    def _settle_bets_internal(
        self, cursor, prediction_id: int, winning_position: str
    ) -> dict:
        """Settle bets using an existing cursor inside a caller-owned transaction."""
        # Get guild_id from prediction for player balance operations
        cursor.execute(
            "SELECT guild_id FROM predictions WHERE prediction_id = ?",
            (prediction_id,),
        )
        pred_row = cursor.fetchone()
        pred_guild_id = pred_row["guild_id"] if pred_row else 0

        # Get all bets grouped by user and position
        cursor.execute(
            """
            SELECT discord_id, position, SUM(amount) as total_amount
            FROM prediction_bets
            WHERE prediction_id = ?
            GROUP BY discord_id, position
            """,
            (prediction_id,),
        )
        bets = [dict(row) for row in cursor.fetchall()]

        if not bets:
            return {"winners": [], "losers": [], "total_pool": 0}

        total_pool = sum(b["total_amount"] for b in bets)
        winner_pool = sum(
            b["total_amount"] for b in bets if b["position"] == winning_position
        )

        winners = []
        losers = []
        balance_updates = {}
        payout_by_user = {}

        # Edge case: no bets on winning side - refund everyone
        if winner_pool == 0:
            for bet in bets:
                balance_updates[bet["discord_id"]] = (
                    balance_updates.get(bet["discord_id"], 0) + bet["total_amount"]
                )
                losers.append({
                    "discord_id": bet["discord_id"],
                    "position": bet["position"],
                    "amount": bet["total_amount"],
                    "refunded": True,
                })
        else:
            for bet in bets:
                if bet["position"] == winning_position:
                    # Proportional payout
                    payout = math.ceil(
                        (bet["total_amount"] / winner_pool) * total_pool
                    )
                    profit = payout - bet["total_amount"]
                    balance_updates[bet["discord_id"]] = (
                        balance_updates.get(bet["discord_id"], 0) + payout
                    )
                    payout_by_user[bet["discord_id"]] = payout
                    winners.append({
                        "discord_id": bet["discord_id"],
                        "amount": bet["total_amount"],
                        "payout": payout,
                        "profit": profit,
                    })
                else:
                    losers.append({
                        "discord_id": bet["discord_id"],
                        "position": bet["position"],
                        "amount": bet["total_amount"],
                    })

        # Update balances
        for discord_id, delta in balance_updates.items():
            cursor.execute(
                """
                UPDATE players
                SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?,
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (delta, discord_id, pred_guild_id),
            )

        # Update payout on winning bets
        for discord_id, payout in payout_by_user.items():
            cursor.execute(
                """
                UPDATE prediction_bets
                SET payout = ?
                WHERE prediction_id = ? AND discord_id = ? AND position = ?
                """,
                (payout, prediction_id, discord_id, winning_position),
            )

        return {
            "winners": winners,
            "losers": losers,
            "total_pool": total_pool,
            "winner_pool": winner_pool,
        }

    def refund_prediction_bets(self, prediction_id: int) -> dict:
        """Refund all bets for a cancelled prediction. Returns refund summary."""
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            # Get guild_id from prediction for player balance operations
            cursor.execute(
                "SELECT guild_id FROM predictions WHERE prediction_id = ?",
                (prediction_id,),
            )
            pred_row = cursor.fetchone()
            pred_guild_id = pred_row["guild_id"] if pred_row else 0

            # Get all bets grouped by user
            cursor.execute(
                """
                SELECT discord_id, SUM(amount) as total_amount
                FROM prediction_bets
                WHERE prediction_id = ?
                GROUP BY discord_id
                """,
                (prediction_id,),
            )
            bets = [dict(row) for row in cursor.fetchall()]

            if not bets:
                return {"refunded": [], "total_refunded": 0}

            total_refunded = 0
            refunded = []

            for bet in bets:
                cursor.execute(
                    """
                    UPDATE players
                    SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?,
                        updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    (bet["total_amount"], bet["discord_id"], pred_guild_id),
                )
                total_refunded += bet["total_amount"]
                refunded.append({
                    "discord_id": bet["discord_id"],
                    "amount": bet["total_amount"],
                })

            return {"refunded": refunded, "total_refunded": total_refunded}

    def get_user_prediction_stats(self, discord_id: int, guild_id: int | None = None) -> dict | None:
        """
        Get prediction betting stats for a user.

        Returns dict with total_bets, wins, losses, total_wagered, net_pnl, etc.
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    COUNT(*) as total_bets,
                    SUM(amount) as total_wagered,
                    SUM(CASE WHEN payout IS NOT NULL AND payout > 0 THEN 1 ELSE 0 END) as wins,
                    SUM(CASE WHEN payout IS NULL AND p.status = 'resolved' THEN 1 ELSE 0 END) as losses,
                    SUM(CASE WHEN payout IS NOT NULL THEN payout - amount ELSE 0 END) as gross_profit,
                    SUM(CASE WHEN payout IS NULL AND p.status = 'resolved' THEN amount ELSE 0 END) as total_lost,
                    MAX(CASE WHEN payout IS NOT NULL THEN payout - amount ELSE 0 END) as best_win,
                    MAX(CASE WHEN payout IS NULL AND p.status = 'resolved' THEN amount ELSE 0 END) as worst_loss
                FROM prediction_bets pb
                JOIN predictions p ON pb.prediction_id = p.prediction_id
                WHERE pb.discord_id = ? AND p.guild_id = ?
                """,
                (discord_id, normalized_guild),
            )
            row = cursor.fetchone()
            if not row or row["total_bets"] == 0:
                return None

            stats = dict(row)
            stats["net_pnl"] = (stats["gross_profit"] or 0) - (stats["total_lost"] or 0)
            stats["win_rate"] = stats["wins"] / (stats["wins"] + stats["losses"]) if (stats["wins"] + stats["losses"]) > 0 else 0
            return stats

    def get_prediction_leaderboard(self, guild_id: int | None, limit: int = 10) -> dict:
        """
        Get prediction leaderboard data.

        Returns dict with top_earners and down_bad lists.
        """
        normalized_guild = self.normalize_guild_id(guild_id)

        with self.connection() as conn:
            cursor = conn.cursor()

            # Top earners - users with most profit
            cursor.execute(
                """
                SELECT
                    pb.discord_id,
                    COUNT(*) as total_bets,
                    SUM(CASE WHEN payout IS NOT NULL AND payout > 0 THEN 1 ELSE 0 END) as wins,
                    SUM(CASE WHEN payout IS NULL AND p.status = 'resolved' THEN 1 ELSE 0 END) as losses,
                    SUM(CASE WHEN payout IS NOT NULL THEN payout - amount ELSE -amount END) as net_pnl
                FROM prediction_bets pb
                JOIN predictions p ON pb.prediction_id = p.prediction_id
                WHERE p.guild_id = ? AND p.status = 'resolved'
                GROUP BY pb.discord_id
                HAVING (wins + losses) >= 2
                ORDER BY net_pnl DESC
                LIMIT ?
                """,
                (normalized_guild, limit),
            )
            top_earners = [dict(row) for row in cursor.fetchall()]
            for entry in top_earners:
                total = entry["wins"] + entry["losses"]
                entry["win_rate"] = entry["wins"] / total if total > 0 else 0

            # Down bad - users with most losses
            cursor.execute(
                """
                SELECT
                    pb.discord_id,
                    COUNT(*) as total_bets,
                    SUM(CASE WHEN payout IS NOT NULL AND payout > 0 THEN 1 ELSE 0 END) as wins,
                    SUM(CASE WHEN payout IS NULL AND p.status = 'resolved' THEN 1 ELSE 0 END) as losses,
                    SUM(CASE WHEN payout IS NOT NULL THEN payout - amount ELSE -amount END) as net_pnl
                FROM prediction_bets pb
                JOIN predictions p ON pb.prediction_id = p.prediction_id
                WHERE p.guild_id = ? AND p.status = 'resolved'
                GROUP BY pb.discord_id
                HAVING (wins + losses) >= 2
                ORDER BY net_pnl ASC
                LIMIT ?
                """,
                (normalized_guild, limit),
            )
            down_bad = [dict(row) for row in cursor.fetchall()]
            for entry in down_bad:
                total = entry["wins"] + entry["losses"]
                entry["win_rate"] = entry["wins"] / total if total > 0 else 0

            # Most accurate - highest win rate (min 3 bets)
            cursor.execute(
                """
                SELECT
                    pb.discord_id,
                    COUNT(*) as total_bets,
                    SUM(CASE WHEN payout IS NOT NULL AND payout > 0 THEN 1 ELSE 0 END) as wins,
                    SUM(CASE WHEN payout IS NULL AND p.status = 'resolved' THEN 1 ELSE 0 END) as losses,
                    SUM(CASE WHEN payout IS NOT NULL THEN payout - amount ELSE -amount END) as net_pnl
                FROM prediction_bets pb
                JOIN predictions p ON pb.prediction_id = p.prediction_id
                WHERE p.guild_id = ? AND p.status = 'resolved'
                GROUP BY pb.discord_id
                HAVING (wins + losses) >= 3
                ORDER BY (CAST(wins AS REAL) / (wins + losses)) DESC, wins DESC
                LIMIT ?
                """,
                (normalized_guild, limit),
            )
            most_accurate = [dict(row) for row in cursor.fetchall()]
            for entry in most_accurate:
                total = entry["wins"] + entry["losses"]
                entry["win_rate"] = entry["wins"] / total if total > 0 else 0

            return {
                "top_earners": top_earners,
                "down_bad": down_bad,
                "most_accurate": most_accurate,
            }

    def get_server_prediction_stats(self, guild_id: int | None) -> dict:
        """
        Get server-wide prediction stats.

        Returns dict with total_predictions, total_pool, total_bets, etc.
        """
        normalized_guild = self.normalize_guild_id(guild_id)

        with self.connection() as conn:
            cursor = conn.cursor()

            # Prediction counts
            cursor.execute(
                """
                SELECT
                    COUNT(*) as total_predictions,
                    SUM(CASE WHEN status = 'open' THEN 1 ELSE 0 END) as open_count,
                    SUM(CASE WHEN status = 'locked' THEN 1 ELSE 0 END) as locked_count,
                    SUM(CASE WHEN status = 'resolved' THEN 1 ELSE 0 END) as resolved_count,
                    SUM(CASE WHEN status = 'cancelled' THEN 1 ELSE 0 END) as cancelled_count
                FROM predictions
                WHERE guild_id = ?
                """,
                (normalized_guild,),
            )
            pred_stats = dict(cursor.fetchone())

            # Betting stats
            cursor.execute(
                """
                SELECT
                    COUNT(*) as total_bets,
                    COUNT(DISTINCT pb.discord_id) as unique_bettors,
                    SUM(pb.amount) as total_wagered,
                    AVG(pb.amount) as avg_bet,
                    MAX(pb.amount) as biggest_bet,
                    MAX(CASE WHEN pb.payout IS NOT NULL THEN pb.payout - pb.amount ELSE 0 END) as biggest_win
                FROM prediction_bets pb
                JOIN predictions p ON pb.prediction_id = p.prediction_id
                WHERE p.guild_id = ?
                """,
                (normalized_guild,),
            )
            bet_stats = dict(cursor.fetchone())

            return {**pred_stats, **bet_stats}

    # =========================================================================
    # Order-book mechanic (feat/predict-orderbook)
    # =========================================================================

    VALID_BOOK_SIDES = {"yes_ask", "yes_bid"}
    VALID_TRADE_SIDES = {"yes", "no"}

    def create_orderbook_prediction(
        self,
        guild_id: int,
        creator_id: int,
        question: str,
        initial_fair: int,
        channel_id: int | None = None,
        initial_levels: list[tuple[str, int, int]] | None = None,
    ) -> int:
        """Create a prediction in the new order-book mechanic.

        Stores ``current_price = initial_fair`` and uses ``closes_at = 0``
        as a sentinel meaning 'no scheduled close' (the legacy NOT NULL column
        is satisfied; new code never reads it).

        ``initial_levels`` is inserted in the same transaction so the market
        never lands in storage with status='open' and no book. Callers that
        omit it (legacy paths) get an empty book and must call
        ``replace_levels`` themselves.
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        now = int(time.time())

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO predictions (
                    guild_id, creator_id, question, status, channel_id,
                    created_at, closes_at,
                    current_price, initial_fair, last_refresh_at, lp_pnl
                )
                VALUES (?, ?, ?, 'open', ?, ?, 0, ?, ?, ?, 0)
                """,
                (
                    normalized_guild,
                    creator_id,
                    question,
                    channel_id,
                    now,
                    initial_fair,
                    initial_fair,
                    now,
                ),
            )
            prediction_id = cursor.lastrowid
            cursor.execute(
                """
                INSERT INTO prediction_fair_snapshots
                    (market_id, guild_id, snapshot_at, fair_pct, reason)
                VALUES (?, ?, ?, ?, 'create')
                """,
                (prediction_id, normalized_guild, now, initial_fair),
            )
            if initial_levels:
                for side, price, size in initial_levels:
                    if side not in self.VALID_BOOK_SIDES:
                        raise ValueError(f"Invalid book side: {side}")
                    cursor.execute(
                        """
                        INSERT INTO prediction_levels
                            (prediction_id, side, price, remaining_size, posted_at)
                        VALUES (?, ?, ?, ?, ?)
                        """,
                        (prediction_id, side, price, size, now),
                    )
            return prediction_id

    def replace_levels(
        self, prediction_id: int, levels: list[tuple[str, int, int]]
    ) -> None:
        """Atomically delete all current levels for a market and insert a fresh ladder."""
        now = int(time.time())
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "DELETE FROM prediction_levels WHERE prediction_id = ?",
                (prediction_id,),
            )
            for side, price, size in levels:
                if side not in self.VALID_BOOK_SIDES:
                    raise ValueError(f"Invalid book side: {side}")
                cursor.execute(
                    """
                    INSERT INTO prediction_levels
                        (prediction_id, side, price, remaining_size, posted_at)
                    VALUES (?, ?, ?, ?, ?)
                    """,
                    (prediction_id, side, price, size, now),
                )

    def get_book(self, prediction_id: int) -> dict:
        """Read the current ladder + fair price."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT current_price FROM predictions WHERE prediction_id = ?",
                (prediction_id,),
            )
            row = cursor.fetchone()
            current_price = row["current_price"] if row else None

            cursor.execute(
                """
                SELECT side, price, remaining_size FROM prediction_levels
                WHERE prediction_id = ? AND remaining_size > 0
                """,
                (prediction_id,),
            )
            asks: list[tuple[int, int]] = []
            bids: list[tuple[int, int]] = []
            for r in cursor.fetchall():
                if r["side"] == "yes_ask":
                    asks.append((int(r["price"]), int(r["remaining_size"])))
                elif r["side"] == "yes_bid":
                    bids.append((int(r["price"]), int(r["remaining_size"])))
            asks.sort(key=lambda x: x[0])           # ascending price
            bids.sort(key=lambda x: x[0], reverse=True)  # descending price
            return {
                "current_price": current_price,
                "yes_asks": asks,
                "yes_bids": bids,
            }

    def buy_contracts_atomic(
        self, prediction_id: int, discord_id: int, side: str, contracts: int
    ) -> dict:
        """Atomically execute a BUY YES or BUY NO sweep across the book."""
        from config import PREDICTION_MAX_CONTRACTS_PER_TRADE
        if side not in self.VALID_TRADE_SIDES:
            raise ValueError("side must be 'yes' or 'no'")
        if contracts <= 0:
            raise ValueError("contracts must be positive")
        if contracts > PREDICTION_MAX_CONTRACTS_PER_TRADE:
            raise ValueError(
                f"contracts capped at {PREDICTION_MAX_CONTRACTS_PER_TRADE} per trade."
            )

        now = int(time.time())
        # BUY YES consumes the yes_ask side (cheapest first).
        # BUY NO  consumes the yes_bid side (highest bid first => cheapest NO ask).
        if side == "yes":
            book_side = "yes_ask"
            order_clause = "price ASC"
        else:
            book_side = "yes_bid"
            order_clause = "price DESC"

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            cursor.execute(
                "SELECT status, guild_id FROM predictions WHERE prediction_id = ?",
                (prediction_id,),
            )
            pred = cursor.fetchone()
            if not pred:
                raise ValueError("Prediction not found.")
            if pred["status"] != "open":
                raise ValueError("Market is not open for trading.")
            guild_id = pred["guild_id"]

            cursor.execute(
                f"""
                SELECT level_id, price, remaining_size FROM prediction_levels
                WHERE prediction_id = ? AND side = ? AND remaining_size > 0
                ORDER BY {order_clause}
                """,
                (prediction_id, book_side),
            )
            levels = [dict(r) for r in cursor.fetchall()]

            remaining = contracts
            fills: list[tuple[int, int, int]] = []  # (level_id, price, take)
            for level in levels:
                if remaining <= 0:
                    break
                take = min(remaining, int(level["remaining_size"]))
                fills.append((int(level["level_id"]), int(level["price"]), take))
                remaining -= take

            if remaining > 0:
                available = contracts - remaining
                raise ValueError(
                    f"Insufficient depth: only {available} contracts available "
                    f"(requested {contracts}). Wait for next refresh."
                )

            if side == "yes":
                weighted_pct = sum(price * take for _, price, take in fills)
            else:  # no — cost per contract = 100 - bid_price
                weighted_pct = sum((100 - price) * take for _, price, take in fills)
            total_cost = _quote_total(weighted_pct, "buy")

            cursor.execute(
                """
                SELECT COALESCE(jopacoin_balance, 0) AS balance
                FROM players WHERE discord_id = ? AND guild_id = ?
                """,
                (discord_id, guild_id),
            )
            player = cursor.fetchone()
            if not player:
                raise ValueError("Player not found. Use /player register first.")
            balance = int(player["balance"])
            if balance < 0:
                raise ValueError(
                    "You cannot trade contracts while in debt. Win some games first."
                )
            if balance < total_cost:
                raise ValueError(
                    f"Insufficient balance: need {total_cost}, have {balance}."
                )

            cursor.execute(
                """
                UPDATE players
                SET jopacoin_balance = jopacoin_balance - ?,
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (total_cost, discord_id, guild_id),
            )

            for level_id, _, take in fills:
                cursor.execute(
                    "UPDATE prediction_levels SET remaining_size = remaining_size - ? WHERE level_id = ?",
                    (take, level_id),
                )
            cursor.execute(
                "DELETE FROM prediction_levels WHERE prediction_id = ? AND remaining_size <= 0",
                (prediction_id,),
            )

            yes_c, yes_t, no_c, no_t = self._read_position(cursor, prediction_id, discord_id)
            if side == "yes":
                yes_c += contracts
                yes_t += total_cost
            else:
                no_c += contracts
                no_t += total_cost
            self._write_position(cursor, prediction_id, discord_id, yes_c, yes_t, no_c, no_t)

            vwap_x100 = (
                (weighted_pct * 100 + contracts // 2) // contracts
                if contracts > 0
                else 0
            )
            action = "buy_yes" if side == "yes" else "buy_no"
            cursor.execute(
                """
                INSERT INTO prediction_trades
                    (prediction_id, discord_id, action, contracts, jopacoins, vwap_x100, trade_time)
                VALUES (?, ?, ?, ?, ?, ?, ?)
                """,
                (prediction_id, discord_id, action, contracts, total_cost, vwap_x100, now),
            )

            cursor.execute(
                "UPDATE predictions SET lp_pnl = COALESCE(lp_pnl, 0) + ? WHERE prediction_id = ?",
                (total_cost, prediction_id),
            )

            return {
                "side": side,
                "contracts": contracts,
                "total_cost": total_cost,
                "vwap_x100": vwap_x100,
                "fills": [(price, take) for _, price, take in fills],
                "new_balance": balance - total_cost,
                "yes_contracts": yes_c,
                "no_contracts": no_c,
            }

    def sell_contracts_atomic(
        self, prediction_id: int, discord_id: int, side: str, contracts: int
    ) -> dict:
        """Atomically execute a SELL YES or SELL NO sweep against the bids."""
        from config import PREDICTION_MAX_CONTRACTS_PER_TRADE
        if side not in self.VALID_TRADE_SIDES:
            raise ValueError("side must be 'yes' or 'no'")
        if contracts <= 0:
            raise ValueError("contracts must be positive")
        if contracts > PREDICTION_MAX_CONTRACTS_PER_TRADE:
            raise ValueError(
                f"contracts capped at {PREDICTION_MAX_CONTRACTS_PER_TRADE} per trade."
            )

        now = int(time.time())
        # SELL YES consumes yes_bids (highest first; best price for seller).
        # SELL NO  consumes yes_asks (lowest first => highest NO bid).
        if side == "yes":
            book_side = "yes_bid"
            order_clause = "price DESC"
        else:
            book_side = "yes_ask"
            order_clause = "price ASC"

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            cursor.execute(
                "SELECT status, guild_id FROM predictions WHERE prediction_id = ?",
                (prediction_id,),
            )
            pred = cursor.fetchone()
            if not pred:
                raise ValueError("Prediction not found.")
            if pred["status"] != "open":
                raise ValueError("Market is not open for trading.")
            guild_id = pred["guild_id"]

            yes_c, yes_t, no_c, no_t = self._read_position(cursor, prediction_id, discord_id)
            if side == "yes":
                if yes_c < contracts:
                    raise ValueError(
                        f"You only hold {yes_c} YES contracts (requested to sell {contracts})."
                    )
                old_qty = yes_c
                old_basis = yes_t
            else:
                if no_c < contracts:
                    raise ValueError(
                        f"You only hold {no_c} NO contracts (requested to sell {contracts})."
                    )
                old_qty = no_c
                old_basis = no_t

            cursor.execute(
                f"""
                SELECT level_id, price, remaining_size FROM prediction_levels
                WHERE prediction_id = ? AND side = ? AND remaining_size > 0
                ORDER BY {order_clause}
                """,
                (prediction_id, book_side),
            )
            levels = [dict(r) for r in cursor.fetchall()]

            remaining = contracts
            fills: list[tuple[int, int, int]] = []
            for level in levels:
                if remaining <= 0:
                    break
                take = min(remaining, int(level["remaining_size"]))
                fills.append((int(level["level_id"]), int(level["price"]), take))
                remaining -= take

            if remaining > 0:
                available = contracts - remaining
                raise ValueError(
                    f"Insufficient depth on the bid side: only {available} contracts "
                    f"can be sold (requested {contracts}). Wait for next refresh."
                )

            if side == "yes":
                weighted_pct = sum(price * take for _, price, take in fills)
            else:  # NO — proceeds per contract = 100 - ask_price
                weighted_pct = sum((100 - price) * take for _, price, take in fills)
            total_proceeds = _quote_total(weighted_pct, "sell")

            cursor.execute(
                """
                UPDATE players
                SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?,
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (total_proceeds, discord_id, guild_id),
            )

            for level_id, _, take in fills:
                cursor.execute(
                    "UPDATE prediction_levels SET remaining_size = remaining_size - ? WHERE level_id = ?",
                    (take, level_id),
                )
            cursor.execute(
                "DELETE FROM prediction_levels WHERE prediction_id = ? AND remaining_size <= 0",
                (prediction_id,),
            )

            # Reduce cost basis proportionally; integer floor.
            basis_reduction = (old_basis * contracts) // old_qty if old_qty > 0 else 0
            new_qty = old_qty - contracts
            new_basis = old_basis - basis_reduction
            if side == "yes":
                yes_c, yes_t = new_qty, new_basis
            else:
                no_c, no_t = new_qty, new_basis
            self._write_position(cursor, prediction_id, discord_id, yes_c, yes_t, no_c, no_t)

            vwap_x100 = (
                (weighted_pct * 100 + contracts // 2) // contracts
                if contracts > 0
                else 0
            )
            action = "sell_yes" if side == "yes" else "sell_no"
            cursor.execute(
                """
                INSERT INTO prediction_trades
                    (prediction_id, discord_id, action, contracts, jopacoins, vwap_x100, trade_time)
                VALUES (?, ?, ?, ?, ?, ?, ?)
                """,
                (prediction_id, discord_id, action, contracts, -total_proceeds, vwap_x100, now),
            )

            cursor.execute(
                "UPDATE predictions SET lp_pnl = COALESCE(lp_pnl, 0) - ? WHERE prediction_id = ?",
                (total_proceeds, prediction_id),
            )

            cursor.execute(
                "SELECT COALESCE(jopacoin_balance, 0) AS balance FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, guild_id),
            )
            new_balance = int(cursor.fetchone()["balance"])

            return {
                "side": side,
                "contracts": contracts,
                "total_proceeds": total_proceeds,
                "vwap_x100": vwap_x100,
                "fills": [(price, take) for _, price, take in fills],
                "new_balance": new_balance,
                "yes_contracts": yes_c,
                "no_contracts": no_c,
            }

    def _read_position(self, cursor, prediction_id: int, discord_id: int) -> tuple[int, int, int, int]:
        """Return (yes_contracts, yes_cost_basis_total, no_contracts, no_cost_basis_total)."""
        cursor.execute(
            """
            SELECT yes_contracts, yes_cost_basis_total, no_contracts, no_cost_basis_total
            FROM prediction_positions
            WHERE prediction_id = ? AND discord_id = ?
            """,
            (prediction_id, discord_id),
        )
        row = cursor.fetchone()
        if not row:
            return (0, 0, 0, 0)
        return (
            int(row["yes_contracts"]),
            int(row["yes_cost_basis_total"]),
            int(row["no_contracts"]),
            int(row["no_cost_basis_total"]),
        )

    def _write_position(
        self, cursor, prediction_id: int, discord_id: int,
        yes_c: int, yes_t: int, no_c: int, no_t: int,
    ) -> None:
        """Upsert position; delete the row when both sides hit 0."""
        if yes_c == 0 and no_c == 0:
            cursor.execute(
                "DELETE FROM prediction_positions WHERE prediction_id = ? AND discord_id = ?",
                (prediction_id, discord_id),
            )
            return
        cursor.execute(
            """
            INSERT INTO prediction_positions
                (prediction_id, discord_id, yes_contracts, yes_cost_basis_total,
                 no_contracts, no_cost_basis_total)
            VALUES (?, ?, ?, ?, ?, ?)
            ON CONFLICT(prediction_id, discord_id) DO UPDATE SET
                yes_contracts = excluded.yes_contracts,
                yes_cost_basis_total = excluded.yes_cost_basis_total,
                no_contracts = excluded.no_contracts,
                no_cost_basis_total = excluded.no_cost_basis_total
            """,
            (prediction_id, discord_id, yes_c, yes_t, no_c, no_t),
        )

    def get_position(self, prediction_id: int, discord_id: int) -> dict | None:
        with self.connection() as conn:
            cursor = conn.cursor()
            yes_c, yes_t, no_c, no_t = self._read_position(cursor, prediction_id, discord_id)
            if yes_c == 0 and no_c == 0:
                return None
            return {
                "prediction_id": prediction_id,
                "discord_id": discord_id,
                "yes_contracts": yes_c,
                "yes_cost_basis_total": yes_t,
                "no_contracts": no_c,
                "no_cost_basis_total": no_t,
            }

    def get_user_open_positions(
        self, discord_id: int, guild_id: int | None = None
    ) -> list[dict]:
        """Return user's open positions across markets, joined with market metadata."""
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    pp.prediction_id,
                    p.question,
                    p.current_price,
                    p.status,
                    pp.yes_contracts,
                    pp.yes_cost_basis_total,
                    pp.no_contracts,
                    pp.no_cost_basis_total
                FROM prediction_positions pp
                JOIN predictions p ON pp.prediction_id = p.prediction_id
                WHERE pp.discord_id = ?
                  AND p.guild_id = ?
                  AND p.status = 'open'
                  AND (pp.yes_contracts > 0 OR pp.no_contracts > 0)
                ORDER BY p.created_at DESC
                """,
                (discord_id, normalized_guild),
            )
            return [dict(r) for r in cursor.fetchall()]

    def get_recent_trades(self, prediction_id: int, limit: int = 5) -> list[dict]:
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT discord_id, action, contracts, jopacoins, vwap_x100, trade_time
                FROM prediction_trades
                WHERE prediction_id = ?
                ORDER BY trade_id DESC
                LIMIT ?
                """,
                (prediction_id, limit),
            )
            return [dict(r) for r in cursor.fetchall()]

    def get_trade_summary_since(self, prediction_id: int, since_ts: int) -> dict:
        """Aggregate trades since ``since_ts`` for the daily summary message."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT action, contracts, jopacoins, vwap_x100, trade_time, discord_id
                FROM prediction_trades
                WHERE prediction_id = ? AND trade_time >= ?
                ORDER BY trade_id ASC
                """,
                (prediction_id, since_ts),
            )
            rows = [dict(r) for r in cursor.fetchall()]

            total_volume = 0
            yes_volume = 0
            no_volume = 0
            biggest = None
            for r in rows:
                qty = int(r["contracts"])
                cash = int(r["jopacoins"])
                total_volume += qty
                if r["action"] in ("buy_yes", "sell_yes"):
                    yes_volume += qty
                else:
                    no_volume += qty
                if biggest is None or abs(cash) > abs(int(biggest["jopacoins"])):
                    biggest = r
            return {
                "trade_count": len(rows),
                "total_volume": total_volume,
                "yes_volume": yes_volume,
                "no_volume": no_volume,
                "biggest_trade": biggest,
            }

    def get_markets_due_for_refresh(
        self, refresh_interval_seconds: int, now_ts: int
    ) -> list[dict]:
        """Open markets whose ``last_refresh_at`` is older than the cutoff."""
        cutoff = now_ts - refresh_interval_seconds
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT prediction_id, guild_id, question, current_price, last_refresh_at, thread_id, embed_message_id
                FROM predictions
                WHERE status = 'open' AND COALESCE(last_refresh_at, 0) <= ?
                ORDER BY COALESCE(last_refresh_at, 0) ASC
                """,
                (cutoff,),
            )
            return [dict(r) for r in cursor.fetchall()]

    def apply_refresh(
        self,
        prediction_id: int,
        new_price: int,
        levels: list[tuple[str, int, int]],
        now_ts: int,
        reason: str = "refresh",
    ) -> None:
        """Layer fresh size onto the ladder and stamp the new fair / refresh time.

        For each (side, price) in ``levels``: if a matching row already exists,
        ADD the size to its remaining (this is the 'layering' behavior — quiet
        markets accumulate depth at unchanged price levels). If no match, insert
        a fresh level. Old levels at orphan positions (positions not in the new
        ladder) are left untouched, so the book widens over time as fair drifts.

        Re-checks status inside the write lock so a concurrent /predict resolve
        or /predict cancel can't be clobbered by a stale refresh.
        """
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT status, guild_id FROM predictions WHERE prediction_id = ?",
                (prediction_id,),
            )
            row = cursor.fetchone()
            if not row or row["status"] != "open":
                return  # market was resolved/cancelled while we were processing
            guild_id = int(row["guild_id"])

            # Crossing levels from earlier flow are left in place on purpose:
            # they're the arb pockets that drive engagement.

            for side, price, size in levels:
                if side not in self.VALID_BOOK_SIDES:
                    raise ValueError(f"Invalid book side: {side}")
                cursor.execute(
                    """
                    SELECT level_id, remaining_size FROM prediction_levels
                    WHERE prediction_id = ? AND side = ? AND price = ?
                    """,
                    (prediction_id, side, price),
                )
                existing = cursor.fetchone()
                if existing:
                    cursor.execute(
                        """
                        UPDATE prediction_levels
                        SET remaining_size = remaining_size + ?, posted_at = ?
                        WHERE level_id = ?
                        """,
                        (size, now_ts, existing["level_id"]),
                    )
                else:
                    cursor.execute(
                        """
                        INSERT INTO prediction_levels
                            (prediction_id, side, price, remaining_size, posted_at)
                        VALUES (?, ?, ?, ?, ?)
                        """,
                        (prediction_id, side, price, size, now_ts),
                    )

            # Stamp prev_price with the OLD current_price so the digest can
            # render a price-change arrow on the next render.
            cursor.execute(
                """
                UPDATE predictions
                SET prev_price = current_price,
                    current_price = ?,
                    last_refresh_at = ?
                WHERE prediction_id = ?
                """,
                (new_price, now_ts, prediction_id),
            )

            cursor.execute(
                """
                INSERT INTO prediction_fair_snapshots
                    (market_id, guild_id, snapshot_at, fair_pct, reason)
                VALUES (?, ?, ?, ?, ?)
                """,
                (prediction_id, guild_id, now_ts, new_price, reason),
            )

    def pop_one_shot_flag(self, guild_id: int, key: str) -> bool:
        """Return True if ``app_kv[(guild, key)]`` was '0' (and atomically flip to '1').

        Used for one-shot digest banners. Subsequent calls return False.
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT value FROM app_kv WHERE guild_id = ? AND key = ?",
                (normalized_guild, key),
            )
            row = cursor.fetchone()
            if not row or str(row["value"]) != "0":
                return False
            cursor.execute(
                "UPDATE app_kv SET value = '1' WHERE guild_id = ? AND key = ?",
                (normalized_guild, key),
            )
            return True

    def get_fair_history(
        self, prediction_id: int, guild_id: int
    ) -> list[tuple[int, int]]:
        """Return ``[(snapshot_at, fair_pct), ...]`` ordered oldest first.

        Powers the per-market price chart in the embed.
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT snapshot_at, fair_pct
                FROM prediction_fair_snapshots
                WHERE market_id = ? AND guild_id = ?
                ORDER BY snapshot_at ASC
                """,
                (prediction_id, normalized_guild),
            )
            return [(int(r["snapshot_at"]), int(r["fair_pct"])) for r in cursor.fetchall()]

    def settle_prediction_orderbook(
        self, prediction_id: int, outcome: str, resolved_by: int | None = None
    ) -> dict:
        """Atomic resolve: cancel levels, pay winners, mark resolved.

        ``outcome`` is 'yes' or 'no'. Pays ``PREDICTION_CONTRACT_VALUE`` per
        winning contract; losing contracts pay 0. Cost basis is irrelevant —
        payout is purely a function of contract count. ``resolved_by`` is
        recorded for the audit trail.
        """
        if outcome not in self.VALID_POSITIONS:
            raise ValueError(f"Invalid outcome: {outcome}")
        from config import PREDICTION_CONTRACT_VALUE

        now = int(time.time())
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            cursor.execute(
                "SELECT status, guild_id FROM predictions WHERE prediction_id = ?",
                (prediction_id,),
            )
            pred = cursor.fetchone()
            if not pred:
                raise ValueError("Prediction not found.")
            if pred["status"] != "open":
                raise ValueError(
                    f"Cannot settle market in status '{pred['status']}'."
                )
            guild_id = int(pred["guild_id"])

            cursor.execute(
                "DELETE FROM prediction_levels WHERE prediction_id = ?",
                (prediction_id,),
            )

            cursor.execute(
                """
                SELECT discord_id, yes_contracts, yes_cost_basis_total,
                       no_contracts, no_cost_basis_total
                FROM prediction_positions
                WHERE prediction_id = ?
                """,
                (prediction_id,),
            )
            positions = [dict(r) for r in cursor.fetchall()]

            winners: list[dict] = []
            losers: list[dict] = []
            total_payout = 0

            for p in positions:
                yes_c = int(p["yes_contracts"])
                no_c = int(p["no_contracts"])
                yes_t = int(p["yes_cost_basis_total"])
                no_t = int(p["no_cost_basis_total"])
                if outcome == "yes":
                    payout = yes_c * PREDICTION_CONTRACT_VALUE
                    losing_basis = no_t
                    winning_qty = yes_c
                    losing_qty = no_c
                else:
                    payout = no_c * PREDICTION_CONTRACT_VALUE
                    losing_basis = yes_t
                    winning_qty = no_c
                    losing_qty = yes_c

                if payout > 0:
                    cursor.execute(
                        """
                        UPDATE players
                        SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?,
                            updated_at = CURRENT_TIMESTAMP
                        WHERE discord_id = ? AND guild_id = ?
                        """,
                        (payout, p["discord_id"], guild_id),
                    )
                    total_payout += payout
                    winning_basis = yes_t if outcome == "yes" else no_t
                    winners.append({
                        "discord_id": int(p["discord_id"]),
                        "contracts": winning_qty,
                        "payout": payout,
                        "profit": payout - winning_basis,
                    })
                if losing_qty > 0:
                    losers.append({
                        "discord_id": int(p["discord_id"]),
                        "contracts": losing_qty,
                        "loss": losing_basis,
                    })

            cursor.execute(
                "UPDATE predictions SET lp_pnl = COALESCE(lp_pnl, 0) - ? WHERE prediction_id = ?",
                (total_payout, prediction_id),
            )

            cursor.execute(
                """
                UPDATE predictions
                SET status = 'resolved', outcome = ?, resolved_at = ?, resolved_by = ?
                WHERE prediction_id = ?
                """,
                (outcome, now, resolved_by, prediction_id),
            )

            cursor.execute(
                "SELECT lp_pnl FROM predictions WHERE prediction_id = ?",
                (prediction_id,),
            )
            lp_pnl = int(cursor.fetchone()["lp_pnl"] or 0)

            return {
                "prediction_id": prediction_id,
                "outcome": outcome,
                "winners": winners,
                "losers": losers,
                "total_payout": total_payout,
                "lp_pnl": lp_pnl,
            }

    def cancel_orderbook_prediction(self, prediction_id: int) -> dict:
        """Refund each holder's cost basis (yes + no totals); zero out positions."""
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            cursor.execute(
                "SELECT status, guild_id FROM predictions WHERE prediction_id = ?",
                (prediction_id,),
            )
            pred = cursor.fetchone()
            if not pred:
                raise ValueError("Prediction not found.")
            if pred["status"] != "open":
                raise ValueError(f"Cannot cancel market in status '{pred['status']}'.")
            guild_id = int(pred["guild_id"])

            cursor.execute(
                "DELETE FROM prediction_levels WHERE prediction_id = ?",
                (prediction_id,),
            )

            cursor.execute(
                """
                SELECT discord_id, yes_cost_basis_total, no_cost_basis_total
                FROM prediction_positions
                WHERE prediction_id = ?
                """,
                (prediction_id,),
            )
            holders = [dict(r) for r in cursor.fetchall()]

            refunded: list[dict] = []
            total_refunded = 0
            for h in holders:
                refund = int(h["yes_cost_basis_total"]) + int(h["no_cost_basis_total"])
                if refund > 0:
                    cursor.execute(
                        """
                        UPDATE players
                        SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?,
                            updated_at = CURRENT_TIMESTAMP
                        WHERE discord_id = ? AND guild_id = ?
                        """,
                        (refund, h["discord_id"], guild_id),
                    )
                    total_refunded += refund
                refunded.append({
                    "discord_id": int(h["discord_id"]),
                    "refund": refund,
                })

            cursor.execute(
                "DELETE FROM prediction_positions WHERE prediction_id = ?",
                (prediction_id,),
            )

            cursor.execute(
                "UPDATE predictions SET status = 'cancelled' WHERE prediction_id = ?",
                (prediction_id,),
            )

            return {
                "prediction_id": prediction_id,
                "refunded": refunded,
                "total_refunded": total_refunded,
            }

    def get_open_orderbook_predictions(self, guild_id: int) -> list[dict]:
        """List open markets in a guild, enriched with current_price + top-of-book + today vol."""
        from config import PREDICTION_REFRESH_SECONDS

        normalized_guild = self.normalize_guild_id(guild_id)
        now = int(time.time())
        since = now - PREDICTION_REFRESH_SECONDS
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT prediction_id, question, creator_id, current_price,
                       prev_price, last_refresh_at, created_at, thread_id,
                       channel_id, embed_message_id, guild_id
                FROM predictions
                WHERE guild_id = ? AND status = 'open'
                ORDER BY created_at DESC
                """,
                (normalized_guild,),
            )
            preds = [dict(r) for r in cursor.fetchall()]

            for pred in preds:
                pid = pred["prediction_id"]
                book = self._book_summary(cursor, pid)
                pred["top_ask"] = book["top_ask"]
                pred["top_bid"] = book["top_bid"]
                cursor.execute(
                    "SELECT COALESCE(SUM(contracts), 0) AS vol FROM prediction_trades WHERE prediction_id = ? AND trade_time >= ?",
                    (pid, since),
                )
                pred["volume_recent"] = int(cursor.fetchone()["vol"])
            return preds

    def _book_summary(self, cursor, prediction_id: int) -> dict:
        cursor.execute(
            """
            SELECT side, MIN(CASE WHEN side='yes_ask' THEN price END) AS top_ask,
                         MAX(CASE WHEN side='yes_bid' THEN price END) AS top_bid
            FROM prediction_levels
            WHERE prediction_id = ? AND remaining_size > 0
            """,
            (prediction_id,),
        )
        row = cursor.fetchone()
        if not row:
            return {"top_ask": None, "top_bid": None}
        return {"top_ask": row["top_ask"], "top_bid": row["top_bid"]}
