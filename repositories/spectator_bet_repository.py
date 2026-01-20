"""
Repository for managing spectator pool bets (parimutuel with player cut).
"""

from __future__ import annotations

from repositories.base_repository import BaseRepository
from repositories.interfaces import ISpectatorBetRepository


class SpectatorBetRepository(BaseRepository, ISpectatorBetRepository):
    """
    Handles CRUD operations for the spectator_bets table.

    The spectator pool is for non-participants betting on match outcomes.
    Payouts are 90% to winning bettors (parimutuel) and 10% to winning players.
    """

    VALID_TEAMS = {"radiant", "dire"}

    @staticmethod
    def _normalize_guild_id(guild_id: int | None) -> int:
        return guild_id if guild_id is not None else 0

    def create_bet(
        self,
        guild_id: int | None,
        discord_id: int,
        team: str,
        amount: int,
        bet_time: int,
    ) -> int:
        """
        Create a spectator bet.

        Args:
            guild_id: Guild ID for multi-guild support
            discord_id: Bettor's Discord ID
            team: 'radiant' or 'dire'
            amount: Bet amount (must be > 0)
            bet_time: Unix timestamp of bet placement

        Returns:
            bet_id of created bet
        """
        if team not in self.VALID_TEAMS:
            raise ValueError(f"Invalid team: {team}. Must be one of {self.VALID_TEAMS}")
        if amount <= 0:
            raise ValueError(f"Bet amount must be positive, got {amount}")

        normalized_guild = self._normalize_guild_id(guild_id)

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("BEGIN IMMEDIATE")

            # Deduct from player balance
            cursor.execute(
                """
                UPDATE players
                SET jopacoin_balance = jopacoin_balance - ?,
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND jopacoin_balance >= ?
                """,
                (amount, discord_id, amount),
            )
            if cursor.rowcount == 0:
                raise ValueError("Insufficient balance for bet")

            # Create bet entry
            cursor.execute(
                """
                INSERT INTO spectator_bets
                (guild_id, discord_id, team, amount, bet_time)
                VALUES (?, ?, ?, ?, ?)
                """,
                (normalized_guild, discord_id, team, amount, bet_time),
            )
            bet_id = cursor.lastrowid

        return bet_id

    def get_pending_bets(
        self, guild_id: int | None, since_ts: int
    ) -> list[dict]:
        """
        Get all pending (unsettled) spectator bets for a guild.

        Args:
            guild_id: Guild ID
            since_ts: Bet timestamp threshold

        Returns:
            List of bet dicts
        """
        normalized_guild = self._normalize_guild_id(guild_id)

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT bet_id, guild_id, match_id, discord_id, team,
                       amount, bet_time, payout, created_at
                FROM spectator_bets
                WHERE guild_id = ? AND match_id IS NULL AND bet_time >= ?
                ORDER BY bet_time ASC
                """,
                (normalized_guild, since_ts),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_player_pending_bet(
        self, guild_id: int | None, discord_id: int, since_ts: int | None = None
    ) -> dict | None:
        """
        Get a player's pending spectator bet.

        Args:
            guild_id: Guild ID
            discord_id: Player's Discord ID
            since_ts: Optional timestamp threshold

        Returns:
            Bet dict or None
        """
        normalized_guild = self._normalize_guild_id(guild_id)

        with self.connection() as conn:
            cursor = conn.cursor()
            if since_ts is not None:
                cursor.execute(
                    """
                    SELECT bet_id, guild_id, match_id, discord_id, team,
                           amount, bet_time, payout, created_at
                    FROM spectator_bets
                    WHERE guild_id = ? AND discord_id = ? AND match_id IS NULL
                          AND bet_time >= ?
                    ORDER BY bet_time DESC
                    LIMIT 1
                    """,
                    (normalized_guild, discord_id, since_ts),
                )
            else:
                cursor.execute(
                    """
                    SELECT bet_id, guild_id, match_id, discord_id, team,
                           amount, bet_time, payout, created_at
                    FROM spectator_bets
                    WHERE guild_id = ? AND discord_id = ? AND match_id IS NULL
                    ORDER BY bet_time DESC
                    LIMIT 1
                    """,
                    (normalized_guild, discord_id),
                )
            row = cursor.fetchone()
            return dict(row) if row else None

    def get_pool_totals(
        self, guild_id: int | None, since_ts: int
    ) -> dict:
        """
        Get pool totals by team.

        Args:
            guild_id: Guild ID
            since_ts: Bet timestamp threshold

        Returns:
            Dict with {radiant: n, dire: m, total: n+m}
        """
        normalized_guild = self._normalize_guild_id(guild_id)

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    COALESCE(SUM(CASE WHEN team = 'radiant' THEN amount ELSE 0 END), 0) as radiant,
                    COALESCE(SUM(CASE WHEN team = 'dire' THEN amount ELSE 0 END), 0) as dire
                FROM spectator_bets
                WHERE guild_id = ? AND match_id IS NULL AND bet_time >= ?
                """,
                (normalized_guild, since_ts),
            )
            row = cursor.fetchone()
            radiant = row["radiant"]
            dire = row["dire"]
            return {
                "radiant": radiant,
                "dire": dire,
                "total": radiant + dire,
            }

    def settle_bets_atomic(
        self,
        match_id: int,
        guild_id: int | None,
        since_ts: int,
        winning_team: str,
        payout_multiplier: float,
    ) -> dict:
        """
        Atomically settle spectator bets for a completed match.

        - Assigns match_id to all pending bets
        - Pays out to winning bettors based on multiplier
        - Updates player balances

        Args:
            match_id: The recorded match ID
            guild_id: Guild ID
            since_ts: Bet timestamp threshold
            winning_team: 'radiant' or 'dire'
            payout_multiplier: Multiplier for winning bets (from parimutuel calc)

        Returns:
            Dict with settlement summary
        """
        normalized_guild = self._normalize_guild_id(guild_id)
        winners: list[dict] = []
        losers: list[dict] = []

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("BEGIN IMMEDIATE")

            # Get all pending bets
            cursor.execute(
                """
                SELECT bet_id, discord_id, team, amount
                FROM spectator_bets
                WHERE guild_id = ? AND match_id IS NULL AND bet_time >= ?
                """,
                (normalized_guild, since_ts),
            )
            bets = cursor.fetchall()

            if not bets:
                return {
                    "winners": [],
                    "losers": [],
                    "total_payout": 0,
                    "total_wagered": 0,
                }

            balance_deltas: dict[int, int] = {}
            payout_updates: list[tuple[int, int]] = []  # (payout, bet_id)
            total_wagered = 0

            for row in bets:
                bet = dict(row)
                total_wagered += bet["amount"]
                entry = {
                    "bet_id": bet["bet_id"],
                    "discord_id": bet["discord_id"],
                    "team": bet["team"],
                    "amount": bet["amount"],
                }

                if bet["team"] == winning_team:
                    # Winner: payout = amount * multiplier
                    payout = int(bet["amount"] * payout_multiplier)
                    balance_deltas[bet["discord_id"]] = (
                        balance_deltas.get(bet["discord_id"], 0) + payout
                    )
                    payout_updates.append((payout, bet["bet_id"]))
                    entry["payout"] = payout
                    winners.append(entry)
                else:
                    losers.append(entry)

            # Update player balances
            if balance_deltas:
                cursor.executemany(
                    """
                    UPDATE players
                    SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?,
                        updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ?
                    """,
                    [(delta, discord_id) for discord_id, delta in balance_deltas.items()],
                )

            # Store payout values
            if payout_updates:
                cursor.executemany(
                    "UPDATE spectator_bets SET payout = ? WHERE bet_id = ?",
                    payout_updates,
                )

            # Assign match_id to all bets
            cursor.execute(
                """
                UPDATE spectator_bets
                SET match_id = ?
                WHERE guild_id = ? AND match_id IS NULL AND bet_time >= ?
                """,
                (match_id, normalized_guild, since_ts),
            )

        total_payout = sum(w.get("payout", 0) for w in winners)
        return {
            "winners": winners,
            "losers": losers,
            "total_payout": total_payout,
            "total_wagered": total_wagered,
            "winning_team": winning_team,
        }

    def delete_bets(
        self, guild_id: int | None, since_ts: int
    ) -> int:
        """
        Delete pending bets WITHOUT refunding (for error cases).

        For proper refunds, use refund_bets_atomic().

        Args:
            guild_id: Guild ID
            since_ts: Bet timestamp threshold

        Returns:
            Number of bets deleted
        """
        normalized_guild = self._normalize_guild_id(guild_id)

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                DELETE FROM spectator_bets
                WHERE guild_id = ? AND match_id IS NULL AND bet_time >= ?
                """,
                (normalized_guild, since_ts),
            )
            return cursor.rowcount

    def refund_bets_atomic(
        self, guild_id: int | None, since_ts: int
    ) -> dict:
        """
        Refund all pending spectator bets for a guild.

        Args:
            guild_id: Guild ID
            since_ts: Bet timestamp threshold

        Returns:
            Dict with refund summary
        """
        normalized_guild = self._normalize_guild_id(guild_id)

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("BEGIN IMMEDIATE")

            # Get all pending bets
            cursor.execute(
                """
                SELECT bet_id, discord_id, amount
                FROM spectator_bets
                WHERE guild_id = ? AND match_id IS NULL AND bet_time >= ?
                """,
                (normalized_guild, since_ts),
            )
            bets = cursor.fetchall()

            if not bets:
                return {"refunded": 0, "total_amount": 0, "bets": []}

            # Calculate refunds per player
            refunds: dict[int, int] = {}
            refunded_bets: list[dict] = []
            bet_ids: list[int] = []

            for row in bets:
                bet = dict(row)
                refunds[bet["discord_id"]] = (
                    refunds.get(bet["discord_id"], 0) + bet["amount"]
                )
                refunded_bets.append(bet)
                bet_ids.append(bet["bet_id"])

            # Refund balances
            cursor.executemany(
                """
                UPDATE players
                SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?,
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
                """,
                [(amount, discord_id) for discord_id, amount in refunds.items()],
            )

            # Delete refunded bets
            if bet_ids:
                placeholders = ",".join("?" * len(bet_ids))
                cursor.execute(
                    f"DELETE FROM spectator_bets WHERE bet_id IN ({placeholders})",
                    bet_ids,
                )

        total_amount = sum(bet["amount"] for bet in refunded_bets)
        return {
            "refunded": len(refunded_bets),
            "total_amount": total_amount,
            "bets": refunded_bets,
        }

    def assign_match_id(
        self, guild_id: int | None, match_id: int, since_ts: int
    ) -> int:
        """
        Assign match ID to pending bets without settling.

        Args:
            guild_id: Guild ID
            match_id: Match ID to assign
            since_ts: Bet timestamp threshold

        Returns:
            Number of bets updated
        """
        normalized_guild = self._normalize_guild_id(guild_id)

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE spectator_bets
                SET match_id = ?
                WHERE guild_id = ? AND match_id IS NULL AND bet_time >= ?
                """,
                (match_id, normalized_guild, since_ts),
            )
            return cursor.rowcount

    def get_player_bet_history(
        self, discord_id: int, limit: int = 50
    ) -> list[dict]:
        """
        Get spectator bet history for a player.

        Args:
            discord_id: Player's Discord ID
            limit: Maximum number of records to return

        Returns:
            List of bet dicts with match info
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT sb.bet_id, sb.guild_id, sb.match_id, sb.team,
                       sb.amount, sb.payout, sb.bet_time,
                       m.winning_team
                FROM spectator_bets sb
                LEFT JOIN matches m ON sb.match_id = m.match_id
                WHERE sb.discord_id = ? AND sb.match_id IS NOT NULL
                ORDER BY sb.bet_time DESC
                LIMIT ?
                """,
                (discord_id, limit),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_player_bet_stats(self, discord_id: int) -> dict:
        """
        Get aggregate spectator betting statistics for a player.

        Returns:
            Dict with total bets, wins, total wagered, total payout, etc.
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    COUNT(*) as total_bets,
                    SUM(CASE WHEN payout > 0 THEN 1 ELSE 0 END) as winning_bets,
                    SUM(amount) as total_wagered,
                    SUM(COALESCE(payout, 0)) as total_payout
                FROM spectator_bets
                WHERE discord_id = ? AND match_id IS NOT NULL
                """,
                (discord_id,),
            )
            row = cursor.fetchone()
            if row:
                return dict(row)
            return {
                "total_bets": 0,
                "winning_bets": 0,
                "total_wagered": 0,
                "total_payout": 0,
            }
