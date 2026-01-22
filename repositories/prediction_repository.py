"""
Repository for managing prediction market data.
"""

from __future__ import annotations

import json
import math
import time

from repositories.base_repository import BaseRepository
from repositories.interfaces import IPredictionRepository


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
                SELECT status, closes_at FROM predictions WHERE prediction_id = ?
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
                "SELECT COALESCE(jopacoin_balance, 0) as balance FROM players WHERE discord_id = ?",
                (discord_id,),
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
                WHERE discord_id = ?
                """,
                (amount, discord_id),
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

    def get_user_active_positions(self, discord_id: int) -> list[dict]:
        """Get all active (unresolved) positions for a user."""
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
                WHERE pb.discord_id = ? AND p.status IN ('open', 'locked')
                GROUP BY p.prediction_id, pb.position
                ORDER BY p.closes_at ASC
                """,
                (discord_id,),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_user_resolved_positions(self, discord_id: int, limit: int = 20) -> list[dict]:
        """Get user's resolved positions with payout info."""
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
                WHERE pb.discord_id = ? AND p.status = 'resolved'
                GROUP BY p.prediction_id, pb.position
                ORDER BY p.resolved_at DESC
                LIMIT ?
                """,
                (discord_id, limit),
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
                    WHERE discord_id = ?
                    """,
                    (delta, discord_id),
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
                    WHERE discord_id = ?
                    """,
                    (bet["total_amount"], bet["discord_id"]),
                )
                total_refunded += bet["total_amount"]
                refunded.append({
                    "discord_id": bet["discord_id"],
                    "amount": bet["total_amount"],
                })

            return {"refunded": refunded, "total_refunded": total_refunded}

    def get_user_prediction_stats(self, discord_id: int) -> dict | None:
        """
        Get prediction betting stats for a user.

        Returns dict with total_bets, wins, losses, total_wagered, net_pnl, etc.
        """
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
                WHERE pb.discord_id = ?
                """,
                (discord_id,),
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
