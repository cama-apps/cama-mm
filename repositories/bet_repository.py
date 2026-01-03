"""
Repository for managing betting data.
"""

from __future__ import annotations

import json
from typing import Dict, List, Optional

from repositories.base_repository import BaseRepository
from repositories.interfaces import IBetRepository


class BetRepository(BaseRepository, IBetRepository):
    """
    Handles CRUD operations against the bets table.
    """

    VALID_TEAMS = {"radiant", "dire"}

    @staticmethod
    def _normalize_guild_id(guild_id: Optional[int]) -> int:
        return guild_id if guild_id is not None else 0

    def create_bet(self, guild_id: Optional[int], discord_id: int, team: str, amount: int, bet_time: int) -> int:
        """
        Place a bet for the current pending match.
        """
        normalized_guild = self._normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO bets (guild_id, match_id, discord_id, team_bet_on, amount, bet_time)
                VALUES (?, NULL, ?, ?, ?, ?)
                """,
                (normalized_guild, discord_id, team, amount, bet_time),
            )
            return cursor.lastrowid

    def place_bet_atomic(
        self,
        *,
        guild_id: Optional[int],
        discord_id: int,
        team: str,
        amount: int,
        bet_time: int,
        since_ts: int,
        leverage: int = 1,
        max_debt: int = 500,
    ) -> int:
        """
        Atomically place a bet with optional leverage:
        - ensure player has no pending bet for the current match window
        - ensure player has sufficient balance (or won't exceed max debt with leverage)
        - debit effective bet amount (amount * leverage)
        - insert bet row with leverage

        This prevents race conditions where concurrent calls could double-spend.
        """
        if amount <= 0:
            raise ValueError("Bet amount must be positive.")
        if team not in self.VALID_TEAMS:
            raise ValueError("Invalid team selection.")
        if leverage < 1:
            raise ValueError("Leverage must be at least 1.")

        effective_bet = amount * leverage
        normalized_guild = self._normalize_guild_id(guild_id)

        with self.connection() as conn:
            cursor = conn.cursor()
            # Take a write lock up-front so two concurrent bet attempts can't interleave.
            cursor.execute("BEGIN IMMEDIATE")

            cursor.execute(
                """
                SELECT 1
                FROM bets
                WHERE guild_id = ? AND discord_id = ? AND match_id IS NULL AND bet_time >= ?
                LIMIT 1
                """,
                (normalized_guild, discord_id, int(since_ts)),
            )
            if cursor.fetchone():
                raise ValueError("You already have a bet on the current match.")

            cursor.execute(
                "SELECT COALESCE(jopacoin_balance, 0) as balance FROM players WHERE discord_id = ?",
                (discord_id,),
            )
            row = cursor.fetchone()
            if not row:
                raise ValueError("Player not found.")

            balance = int(row["balance"])

            # Check debt floor - new balance after bet can't go below -max_debt
            new_balance = balance - effective_bet
            if new_balance < -max_debt:
                raise ValueError(f"Bet would exceed maximum debt limit of {max_debt} jopacoin.")

            cursor.execute(
                """
                UPDATE players
                SET jopacoin_balance = COALESCE(jopacoin_balance, 0) - ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
                """,
                (effective_bet, discord_id),
            )

            cursor.execute(
                """
                INSERT INTO bets (guild_id, match_id, discord_id, team_bet_on, amount, bet_time, leverage)
                VALUES (?, NULL, ?, ?, ?, ?, ?)
                """,
                (normalized_guild, discord_id, team, amount, bet_time, leverage),
            )
            return cursor.lastrowid

    def place_bet_against_pending_match_atomic(
        self,
        *,
        guild_id: Optional[int],
        discord_id: int,
        team: str,
        amount: int,
        bet_time: int,
        leverage: int = 1,
        max_debt: int = 500,
    ) -> int:
        """
        Atomically place a bet with optional leverage using the DB as the source of truth.

        Uses `pending_matches.payload` to enforce:
        - there is an active pending match
        - betting is still open (bet_lock_until)
        - participants may only bet on their own team
        - per-match-window duplicate-bet prevention (shuffle_timestamp)
        - sufficient balance or debt limit, then debits + inserts bet in the same transaction
        """
        if amount <= 0:
            raise ValueError("Bet amount must be positive.")
        if team not in self.VALID_TEAMS:
            raise ValueError("Invalid team selection.")
        if leverage < 1:
            raise ValueError("Leverage must be at least 1.")

        effective_bet = amount * leverage
        normalized_guild = self._normalize_guild_id(guild_id)

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("BEGIN IMMEDIATE")

            cursor.execute(
                "SELECT payload FROM pending_matches WHERE guild_id = ?",
                (normalized_guild,),
            )
            row = cursor.fetchone()
            if not row:
                raise ValueError("No pending match to bet on.")

            try:
                payload = json.loads(row["payload"])
            except Exception:
                raise ValueError("No pending match to bet on.")

            lock_until = payload.get("bet_lock_until")
            if lock_until is None or int(bet_time) >= int(lock_until):
                raise ValueError("Betting is closed for the current match.")

            since_ts = payload.get("shuffle_timestamp")
            if since_ts is None:
                raise ValueError("No pending match to bet on.")

            radiant_ids = set(payload.get("radiant_team_ids") or [])
            dire_ids = set(payload.get("dire_team_ids") or [])
            if discord_id in radiant_ids and team != "radiant":
                raise ValueError("Participants on Radiant can only bet on Radiant.")
            if discord_id in dire_ids and team != "dire":
                raise ValueError("Participants on Dire can only bet on Dire.")

            cursor.execute(
                """
                SELECT 1
                FROM bets
                WHERE guild_id = ? AND discord_id = ? AND match_id IS NULL AND bet_time >= ?
                LIMIT 1
                """,
                (normalized_guild, discord_id, int(since_ts)),
            )
            if cursor.fetchone():
                raise ValueError("You already have a bet on the current match.")

            cursor.execute(
                "SELECT COALESCE(jopacoin_balance, 0) as balance FROM players WHERE discord_id = ?",
                (discord_id,),
            )
            prow = cursor.fetchone()
            if not prow:
                raise ValueError("Player not found.")

            balance = int(prow["balance"])

            # Check debt floor - new balance after bet can't go below -max_debt
            new_balance = balance - effective_bet
            if new_balance < -max_debt:
                raise ValueError(f"Bet would exceed maximum debt limit of {max_debt} jopacoin.")

            cursor.execute(
                """
                UPDATE players
                SET jopacoin_balance = COALESCE(jopacoin_balance, 0) - ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
                """,
                (effective_bet, discord_id),
            )
            cursor.execute(
                """
                INSERT INTO bets (guild_id, match_id, discord_id, team_bet_on, amount, bet_time, leverage)
                VALUES (?, NULL, ?, ?, ?, ?, ?)
                """,
                (normalized_guild, discord_id, team, amount, bet_time, leverage),
            )
            return cursor.lastrowid

    def get_player_pending_bet(
        self, guild_id: Optional[int], discord_id: int, since_ts: Optional[int] = None
    ) -> Optional[Dict]:
        """
        Return the bet placed by a player for the pending match in the guild.
        """
        normalized_guild = self._normalize_guild_id(guild_id)
        ts_filter = "AND bet_time >= ?" if since_ts is not None else ""
        params = (normalized_guild, discord_id) if since_ts is None else (normalized_guild, discord_id, since_ts)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT bet_id, guild_id, match_id, discord_id, team_bet_on, amount, bet_time, created_at,
                       COALESCE(leverage, 1) as leverage
                FROM bets
                WHERE guild_id = ? AND discord_id = ? AND match_id IS NULL
                {ts_filter}
                """.format(ts_filter=ts_filter),
                params,
            )

            row = cursor.fetchone()
            return dict(row) if row else None

    def get_bets_for_pending_match(self, guild_id: Optional[int], since_ts: Optional[int] = None) -> List[Dict]:
        """
        Return bets associated with the pending match for a guild.
        """
        normalized_guild = self._normalize_guild_id(guild_id)
        ts_filter = "AND bet_time >= ?" if since_ts is not None else ""
        params = (normalized_guild,) if since_ts is None else (normalized_guild, since_ts)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT bet_id, guild_id, match_id, discord_id, team_bet_on, amount, bet_time, created_at
                FROM bets
                WHERE guild_id = ? AND match_id IS NULL
                {ts_filter}
                """.format(ts_filter=ts_filter),
                params,
            )
            return [dict(row) for row in cursor.fetchall()]

    def delete_bets_for_guild(self, guild_id: Optional[int]) -> int:
        """Remove all bets for the specified guild."""
        normalized_guild = self._normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("DELETE FROM bets WHERE guild_id = ?", (normalized_guild,))
            return cursor.rowcount

    def get_total_bets_by_guild(self, guild_id: Optional[int], since_ts: Optional[int] = None) -> Dict[str, int]:
        """Return total effective wager amounts grouped by team for a guild.

        Effective amount = amount * leverage, used for pool mode calculations.
        """
        normalized_guild = self._normalize_guild_id(guild_id)
        ts_filter = "AND bet_time >= ?" if since_ts is not None else ""
        params = (normalized_guild,) if since_ts is None else (normalized_guild, since_ts)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT team_bet_on, SUM(amount * COALESCE(leverage, 1)) as total
                FROM bets
                WHERE guild_id = ? AND match_id IS NULL
                {ts_filter}
                GROUP BY team_bet_on
                """.format(ts_filter=ts_filter),
                params,
            )
            totals = {row["team_bet_on"]: row["total"] for row in cursor.fetchall()}
            return {team: totals.get(team, 0) for team in self.VALID_TEAMS}

    def assign_match_id(self, guild_id: Optional[int], match_id: int, since_ts: Optional[int] = None) -> None:
        """Tie all pending bets for the current match window to a recorded match."""
        normalized_guild = self._normalize_guild_id(guild_id)
        ts_filter = "AND bet_time >= ?" if since_ts is not None else ""
        params = (match_id, normalized_guild) if since_ts is None else (match_id, normalized_guild, since_ts)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE bets
                SET match_id = ?
                WHERE guild_id = ? AND match_id IS NULL
                {ts_filter}
                """.format(ts_filter=ts_filter),
                params,
            )

    def delete_pending_bets(self, guild_id: Optional[int], since_ts: Optional[int] = None) -> int:
        """Delete pending bets (match_id IS NULL) for the current match window."""
        normalized_guild = self._normalize_guild_id(guild_id)
        ts_filter = "AND bet_time >= ?" if since_ts is not None else ""
        params = (normalized_guild,) if since_ts is None else (normalized_guild, since_ts)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"DELETE FROM bets WHERE guild_id = ? AND match_id IS NULL {ts_filter}",
                params,
            )
            return cursor.rowcount

    def settle_pending_bets_atomic(
        self,
        *,
        match_id: int,
        guild_id: Optional[int],
        since_ts: int,
        winning_team: str,
        house_payout_multiplier: float,
        betting_mode: str = "house",
    ) -> Dict[str, List[Dict]]:
        """
        Atomically settle bets for the current match window:
        - credit winners in players.jopacoin_balance (based on effective bet with leverage)
        - tag all pending bets with match_id

        Args:
            betting_mode: "house" for 1:1 payouts, "pool" for parimutuel betting
        """
        normalized_guild = self._normalize_guild_id(guild_id)
        distributions: Dict[str, List[Dict]] = {"winners": [], "losers": []}

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT bet_id, discord_id, team_bet_on, amount, COALESCE(leverage, 1) as leverage
                FROM bets
                WHERE guild_id = ? AND match_id IS NULL AND bet_time >= ?
                """,
                (normalized_guild, since_ts),
            )
            rows = cursor.fetchall()
            if not rows:
                return distributions

            if betting_mode == "pool":
                distributions, balance_deltas = self._calculate_pool_payouts(rows, winning_team)
            else:
                distributions, balance_deltas = self._calculate_house_payouts(
                    rows, winning_team, house_payout_multiplier
                )

            if balance_deltas:
                cursor.executemany(
                    """
                    UPDATE players
                    SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?, updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ?
                    """,
                    [(delta, discord_id) for discord_id, delta in balance_deltas.items()],
                )

            cursor.execute(
                """
                UPDATE bets
                SET match_id = ?
                WHERE guild_id = ? AND match_id IS NULL AND bet_time >= ?
                """,
                (match_id, normalized_guild, since_ts),
            )

        return distributions

    def _calculate_house_payouts(
        self, rows: List, winning_team: str, house_payout_multiplier: float
    ) -> tuple:
        """Calculate house mode payouts (1:1) with leverage support."""
        distributions: Dict[str, List[Dict]] = {"winners": [], "losers": []}
        balance_deltas: Dict[int, int] = {}

        for row in rows:
            bet = dict(row)
            leverage = bet.get("leverage", 1) or 1
            effective_bet = bet["amount"] * leverage

            entry = {
                "discord_id": bet["discord_id"],
                "amount": bet["amount"],
                "leverage": leverage,
                "effective_bet": effective_bet,
                "team": bet["team_bet_on"],
            }

            if bet["team_bet_on"] != winning_team:
                distributions["losers"].append(entry)
                continue

            # Payout based on effective bet (amount * leverage)
            payout = int(effective_bet * (1 + house_payout_multiplier))
            balance_deltas[bet["discord_id"]] = balance_deltas.get(bet["discord_id"], 0) + payout
            entry["payout"] = payout
            distributions["winners"].append(entry)

        return distributions, balance_deltas

    def _calculate_pool_payouts(self, rows: List, winning_team: str) -> tuple:
        """Calculate pool mode payouts (proportional from total pool) with leverage support."""
        distributions: Dict[str, List[Dict]] = {"winners": [], "losers": []}
        balance_deltas: Dict[int, int] = {}

        # Convert rows to dicts for .get() support
        rows = [dict(row) for row in rows]

        # Calculate totals using effective bets (amount * leverage)
        total_pool = sum(row["amount"] * (row.get("leverage") or 1) for row in rows)
        winner_pool = sum(
            row["amount"] * (row.get("leverage") or 1)
            for row in rows
            if row["team_bet_on"] == winning_team
        )

        # Edge case: no bets on winning side - refund all effective bets
        if winner_pool == 0:
            for row in rows:
                bet = dict(row)
                leverage = bet.get("leverage", 1) or 1
                effective_bet = bet["amount"] * leverage
                balance_deltas[bet["discord_id"]] = (
                    balance_deltas.get(bet["discord_id"], 0) + effective_bet
                )
                distributions["losers"].append({
                    "discord_id": bet["discord_id"],
                    "amount": bet["amount"],
                    "leverage": leverage,
                    "effective_bet": effective_bet,
                    "team": bet["team_bet_on"],
                    "refunded": True,
                })
            return distributions, balance_deltas

        multiplier = total_pool / winner_pool

        for row in rows:
            bet = dict(row)
            leverage = bet.get("leverage", 1) or 1
            effective_bet = bet["amount"] * leverage

            entry = {
                "discord_id": bet["discord_id"],
                "amount": bet["amount"],
                "leverage": leverage,
                "effective_bet": effective_bet,
                "team": bet["team_bet_on"],
            }

            if bet["team_bet_on"] != winning_team:
                distributions["losers"].append(entry)
                continue

            # Proportional payout: (effective_bet / winner_pool) * total_pool
            payout = int((effective_bet / winner_pool) * total_pool)
            balance_deltas[bet["discord_id"]] = balance_deltas.get(bet["discord_id"], 0) + payout
            entry["payout"] = payout
            entry["multiplier"] = multiplier
            distributions["winners"].append(entry)

        return distributions, balance_deltas

    def refund_pending_bets_atomic(self, *, guild_id: Optional[int], since_ts: int) -> int:
        """
        Atomically refund + delete pending bets for the current match window.
        Returns number of bets refunded.

        Refunds the effective bet amount (amount * leverage).
        """
        normalized_guild = self._normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT discord_id, amount, COALESCE(leverage, 1) as leverage
                FROM bets
                WHERE guild_id = ? AND match_id IS NULL AND bet_time >= ?
                """,
                (normalized_guild, since_ts),
            )
            rows = cursor.fetchall()
            if not rows:
                return 0

            refund_deltas: Dict[int, int] = {}
            for row in rows:
                # Refund the effective bet (amount * leverage)
                effective_bet = int(row["amount"]) * int(row["leverage"])
                refund_deltas[row["discord_id"]] = refund_deltas.get(row["discord_id"], 0) + effective_bet

            cursor.executemany(
                """
                UPDATE players
                SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
                """,
                [(delta, discord_id) for discord_id, delta in refund_deltas.items()],
            )

            cursor.execute(
                "DELETE FROM bets WHERE guild_id = ? AND match_id IS NULL AND bet_time >= ?",
                (normalized_guild, since_ts),
            )
            return cursor.rowcount

