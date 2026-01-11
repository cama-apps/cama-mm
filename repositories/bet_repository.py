"""
Repository for managing betting data.
"""

from __future__ import annotations

import json
import math

from repositories.base_repository import BaseRepository
from repositories.interfaces import IBetRepository


class BetRepository(BaseRepository, IBetRepository):
    """
    Handles CRUD operations against the bets table.
    """

    VALID_TEAMS = {"radiant", "dire"}

    @staticmethod
    def _normalize_guild_id(guild_id: int | None) -> int:
        return guild_id if guild_id is not None else 0

    def create_bet(
        self, guild_id: int | None, discord_id: int, team: str, amount: int, bet_time: int
    ) -> int:
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
        guild_id: int | None,
        discord_id: int,
        team: str,
        amount: int,
        bet_time: int,
        since_ts: int,
        leverage: int = 1,
        max_debt: int = 500,
        is_blind: bool = False,
        odds_at_placement: float | None = None,
    ) -> int:
        """
        Atomically place a bet with optional leverage:
        - ensure player has no pending bet for the current match window
        - ensure player has sufficient balance (or won't exceed max debt with leverage)
        - debit effective bet amount (amount * leverage)
        - insert bet row with leverage

        Args:
            is_blind: True if this is an auto-liquidity blind bet
            odds_at_placement: The odds multiplier at time of bet placement (for /bets display)

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

            # Check for existing bets - allow additional bets only on the same team
            cursor.execute(
                """
                SELECT team_bet_on
                FROM bets
                WHERE guild_id = ? AND discord_id = ? AND match_id IS NULL AND bet_time >= ?
                """,
                (normalized_guild, discord_id, int(since_ts)),
            )
            existing_bets = cursor.fetchall()
            if existing_bets:
                existing_team = existing_bets[0]["team_bet_on"]
                if existing_team != team:
                    raise ValueError(
                        f"You already have bets on {existing_team.title()}. "
                        "You can only add more bets on the same team."
                    )

            cursor.execute(
                "SELECT COALESCE(jopacoin_balance, 0) as balance FROM players WHERE discord_id = ?",
                (discord_id,),
            )
            row = cursor.fetchone()
            if not row:
                raise ValueError("Player not found.")

            balance = int(row["balance"])

            # Users in debt cannot place any bets
            if balance < 0:
                raise ValueError(
                    "You cannot place bets while in debt. Win some games to pay it off!"
                )

            # Balance check depends on leverage:
            # - No leverage (1x): cannot go negative, must have enough balance
            # - With leverage (>1x): can go into debt up to -max_debt
            new_balance = balance - effective_bet
            if leverage == 1:
                if balance < amount:
                    raise ValueError(f"Insufficient balance. You have {balance} jopacoin.")
            else:
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
                INSERT INTO bets (guild_id, match_id, discord_id, team_bet_on, amount, bet_time, leverage, is_blind, odds_at_placement)
                VALUES (?, NULL, ?, ?, ?, ?, ?, ?, ?)
                """,
                (normalized_guild, discord_id, team, amount, bet_time, leverage, 1 if is_blind else 0, odds_at_placement),
            )
            return cursor.lastrowid

    def place_bet_against_pending_match_atomic(
        self,
        *,
        guild_id: int | None,
        discord_id: int,
        team: str,
        amount: int,
        bet_time: int,
        leverage: int = 1,
        max_debt: int = 500,
        is_blind: bool = False,
        odds_at_placement: float | None = None,
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
                raise ValueError("No pending match to bet on.") from None

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

            # Check for existing bets - allow additional bets only on the same team
            cursor.execute(
                """
                SELECT team_bet_on
                FROM bets
                WHERE guild_id = ? AND discord_id = ? AND match_id IS NULL AND bet_time >= ?
                """,
                (normalized_guild, discord_id, int(since_ts)),
            )
            existing_bets = cursor.fetchall()
            if existing_bets:
                existing_team = existing_bets[0]["team_bet_on"]
                if existing_team != team:
                    raise ValueError(
                        f"You already have bets on {existing_team.title()}. "
                        "You can only add more bets on the same team."
                    )

            cursor.execute(
                "SELECT COALESCE(jopacoin_balance, 0) as balance FROM players WHERE discord_id = ?",
                (discord_id,),
            )
            prow = cursor.fetchone()
            if not prow:
                raise ValueError("Player not found.")

            balance = int(prow["balance"])

            # Users in debt cannot place any bets
            if balance < 0:
                raise ValueError(
                    "You cannot place bets while in debt. Win some games to pay it off!"
                )

            # Balance check depends on leverage:
            # - No leverage (1x): cannot go negative, must have enough balance
            # - With leverage (>1x): can go into debt up to -max_debt
            new_balance = balance - effective_bet
            if leverage == 1:
                if balance < amount:
                    raise ValueError(f"Insufficient balance. You have {balance} jopacoin.")
            else:
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
                INSERT INTO bets (guild_id, match_id, discord_id, team_bet_on, amount, bet_time, leverage, is_blind, odds_at_placement)
                VALUES (?, NULL, ?, ?, ?, ?, ?, ?, ?)
                """,
                (normalized_guild, discord_id, team, amount, bet_time, leverage, 1 if is_blind else 0, odds_at_placement),
            )
            return cursor.lastrowid

    def get_player_pending_bet(
        self, guild_id: int | None, discord_id: int, since_ts: int | None = None
    ) -> dict | None:
        """
        Return the bet placed by a player for the pending match in the guild.
        """
        normalized_guild = self._normalize_guild_id(guild_id)
        ts_filter = "AND bet_time >= ?" if since_ts is not None else ""
        params = (
            (normalized_guild, discord_id)
            if since_ts is None
            else (normalized_guild, discord_id, since_ts)
        )
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"""
                SELECT bet_id, guild_id, match_id, discord_id, team_bet_on, amount, bet_time, created_at,
                       COALESCE(leverage, 1) as leverage,
                       COALESCE(is_blind, 0) as is_blind,
                       odds_at_placement
                FROM bets
                WHERE guild_id = ? AND discord_id = ? AND match_id IS NULL
                {ts_filter}
                """,
                params,
            )

            row = cursor.fetchone()
            return dict(row) if row else None

    def get_player_pending_bets(
        self, guild_id: int | None, discord_id: int, since_ts: int | None = None
    ) -> list[dict]:
        """
        Return all bets placed by a player for the pending match in the guild.
        Ordered by bet_time ascending.
        """
        normalized_guild = self._normalize_guild_id(guild_id)
        ts_filter = "AND bet_time >= ?" if since_ts is not None else ""
        params = (
            (normalized_guild, discord_id)
            if since_ts is None
            else (normalized_guild, discord_id, since_ts)
        )
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"""
                SELECT bet_id, guild_id, match_id, discord_id, team_bet_on, amount, bet_time, created_at,
                       COALESCE(leverage, 1) as leverage,
                       COALESCE(is_blind, 0) as is_blind,
                       odds_at_placement
                FROM bets
                WHERE guild_id = ? AND discord_id = ? AND match_id IS NULL
                {ts_filter}
                ORDER BY bet_time ASC
                """,
                params,
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_bets_for_pending_match(
        self, guild_id: int | None, since_ts: int | None = None
    ) -> list[dict]:
        """
        Return bets associated with the pending match for a guild.
        """
        normalized_guild = self._normalize_guild_id(guild_id)
        ts_filter = "AND bet_time >= ?" if since_ts is not None else ""
        params = (normalized_guild,) if since_ts is None else (normalized_guild, since_ts)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"""
                SELECT bet_id, guild_id, match_id, discord_id, team_bet_on, amount, bet_time, created_at,
                       COALESCE(leverage, 1) as leverage,
                       COALESCE(is_blind, 0) as is_blind,
                       odds_at_placement
                FROM bets
                WHERE guild_id = ? AND match_id IS NULL
                {ts_filter}
                ORDER BY bet_time ASC
                """,
                params,
            )
            return [dict(row) for row in cursor.fetchall()]

    def delete_bets_for_guild(self, guild_id: int | None) -> int:
        """Remove all bets for the specified guild."""
        normalized_guild = self._normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("DELETE FROM bets WHERE guild_id = ?", (normalized_guild,))
            return cursor.rowcount

    def get_total_bets_by_guild(
        self, guild_id: int | None, since_ts: int | None = None
    ) -> dict[str, int]:
        """Return total effective wager amounts grouped by team for a guild.

        Effective amount = amount * leverage, used for pool mode calculations.
        """
        normalized_guild = self._normalize_guild_id(guild_id)
        ts_filter = "AND bet_time >= ?" if since_ts is not None else ""
        params = (normalized_guild,) if since_ts is None else (normalized_guild, since_ts)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"""
                SELECT team_bet_on, SUM(amount * COALESCE(leverage, 1)) as total
                FROM bets
                WHERE guild_id = ? AND match_id IS NULL
                {ts_filter}
                GROUP BY team_bet_on
                """,
                params,
            )
            totals = {row["team_bet_on"]: row["total"] for row in cursor.fetchall()}
            return {team: totals.get(team, 0) for team in self.VALID_TEAMS}

    def assign_match_id(
        self, guild_id: int | None, match_id: int, since_ts: int | None = None
    ) -> None:
        """Tie all pending bets for the current match window to a recorded match."""
        normalized_guild = self._normalize_guild_id(guild_id)
        ts_filter = "AND bet_time >= ?" if since_ts is not None else ""
        params = (
            (match_id, normalized_guild)
            if since_ts is None
            else (match_id, normalized_guild, since_ts)
        )
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"""
                UPDATE bets
                SET match_id = ?
                WHERE guild_id = ? AND match_id IS NULL
                {ts_filter}
                """,
                params,
            )

    def delete_pending_bets(self, guild_id: int | None, since_ts: int | None = None) -> int:
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
        guild_id: int | None,
        since_ts: int,
        winning_team: str,
        house_payout_multiplier: float,
        betting_mode: str = "pool",
    ) -> dict[str, list[dict]]:
        """
        Atomically settle bets for the current match window:
        - credit winners in players.jopacoin_balance (based on effective bet with leverage)
        - tag all pending bets with match_id

        Args:
            betting_mode: "pool" for parimutuel betting, "house" for 1:1 payouts
        """
        normalized_guild = self._normalize_guild_id(guild_id)
        distributions: dict[str, list[dict]] = {"winners": [], "losers": []}

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
                distributions, balance_deltas, payout_updates = self._calculate_pool_payouts(
                    rows, winning_team
                )
            else:
                distributions, balance_deltas, payout_updates = self._calculate_house_payouts(
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

            # Store payout for winning bets
            if payout_updates:
                cursor.executemany(
                    "UPDATE bets SET payout = ? WHERE bet_id = ?",
                    payout_updates,
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
        self, rows: list, winning_team: str, house_payout_multiplier: float
    ) -> tuple:
        """Calculate house mode payouts (1:1) with leverage support."""
        distributions: dict[str, list[dict]] = {"winners": [], "losers": []}
        balance_deltas: dict[int, int] = {}
        payout_updates: list[tuple[int, int]] = []  # (payout, bet_id)

        for row in rows:
            bet = dict(row)
            leverage = bet.get("leverage", 1) or 1
            effective_bet = bet["amount"] * leverage

            entry = {
                "bet_id": bet["bet_id"],
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
            payout_updates.append((payout, bet["bet_id"]))
            entry["payout"] = payout
            distributions["winners"].append(entry)

        return distributions, balance_deltas, payout_updates

    def _calculate_pool_payouts(self, rows: list, winning_team: str) -> tuple:
        """Calculate pool mode payouts (proportional from total pool) with leverage support."""
        distributions: dict[str, list[dict]] = {"winners": [], "losers": []}
        balance_deltas: dict[int, int] = {}
        payout_updates: list[tuple[int, int]] = []  # (payout, bet_id)

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
                distributions["losers"].append(
                    {
                        "bet_id": bet["bet_id"],
                        "discord_id": bet["discord_id"],
                        "amount": bet["amount"],
                        "leverage": leverage,
                        "effective_bet": effective_bet,
                        "team": bet["team_bet_on"],
                        "refunded": True,
                    }
                )
            return distributions, balance_deltas, payout_updates

        multiplier = total_pool / winner_pool

        for row in rows:
            bet = dict(row)
            leverage = bet.get("leverage", 1) or 1
            effective_bet = bet["amount"] * leverage

            entry = {
                "bet_id": bet["bet_id"],
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
            # Round up to ensure winners never lose fractional coins
            payout = math.ceil((effective_bet / winner_pool) * total_pool)
            balance_deltas[bet["discord_id"]] = balance_deltas.get(bet["discord_id"], 0) + payout
            payout_updates.append((payout, bet["bet_id"]))
            entry["payout"] = payout
            entry["multiplier"] = multiplier
            distributions["winners"].append(entry)

        return distributions, balance_deltas, payout_updates

    def refund_pending_bets_atomic(self, *, guild_id: int | None, since_ts: int) -> int:
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

            refund_deltas: dict[int, int] = {}
            for row in rows:
                # Refund the effective bet (amount * leverage)
                effective_bet = int(row["amount"]) * int(row["leverage"])
                refund_deltas[row["discord_id"]] = (
                    refund_deltas.get(row["discord_id"], 0) + effective_bet
                )

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

    def get_player_bet_history(self, discord_id: int) -> list[dict]:
        """
        Get all settled bets for a player with outcome derived from match result.

        Returns list of dicts with: bet_id, amount, leverage, effective_bet, team_bet_on,
        bet_time, match_id, payout, outcome ('won'/'lost'), profit (net P&L for this bet)
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    b.bet_id,
                    b.amount,
                    COALESCE(b.leverage, 1) as leverage,
                    b.amount * COALESCE(b.leverage, 1) as effective_bet,
                    b.team_bet_on,
                    b.bet_time,
                    b.match_id,
                    b.payout,
                    CASE
                        WHEN m.winning_team = 1 AND b.team_bet_on = 'radiant' THEN 'won'
                        WHEN m.winning_team = 2 AND b.team_bet_on = 'dire' THEN 'won'
                        ELSE 'lost'
                    END as outcome
                FROM bets b
                JOIN matches m ON b.match_id = m.match_id
                WHERE b.discord_id = ? AND b.match_id IS NOT NULL
                ORDER BY b.bet_time ASC
                """,
                (discord_id,),
            )
            rows = cursor.fetchall()
            results = []
            for row in rows:
                bet = dict(row)
                effective_bet = bet["effective_bet"]
                # Calculate profit: won = payout - effective_bet, lost = -effective_bet
                if bet["outcome"] == "won":
                    # If payout stored, use it; otherwise assume house mode (2x)
                    payout = bet["payout"] if bet["payout"] else effective_bet * 2
                    bet["profit"] = payout - effective_bet
                else:
                    bet["profit"] = -effective_bet
                results.append(bet)
            return results

    def get_guild_gambling_summary(
        self, guild_id: int | None, min_bets: int = 3
    ) -> list[dict]:
        """
        Get aggregated gambling stats for all players in a guild.

        Returns list of dicts with: discord_id, total_bets, wins, losses, win_rate,
        net_pnl, total_wagered, roi, avg_leverage
        """
        normalized_guild = self._normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    b.discord_id,
                    COUNT(*) as total_bets,
                    SUM(CASE
                        WHEN (m.winning_team = 1 AND b.team_bet_on = 'radiant')
                          OR (m.winning_team = 2 AND b.team_bet_on = 'dire')
                        THEN 1 ELSE 0
                    END) as wins,
                    SUM(CASE
                        WHEN (m.winning_team = 1 AND b.team_bet_on = 'radiant')
                          OR (m.winning_team = 2 AND b.team_bet_on = 'dire')
                        THEN 0 ELSE 1
                    END) as losses,
                    SUM(b.amount * COALESCE(b.leverage, 1)) as total_wagered,
                    AVG(COALESCE(b.leverage, 1)) as avg_leverage,
                    SUM(CASE
                        WHEN (m.winning_team = 1 AND b.team_bet_on = 'radiant')
                          OR (m.winning_team = 2 AND b.team_bet_on = 'dire')
                        THEN COALESCE(b.payout, b.amount * COALESCE(b.leverage, 1) * 2)
                             - (b.amount * COALESCE(b.leverage, 1))
                        ELSE -(b.amount * COALESCE(b.leverage, 1))
                    END) as net_pnl
                FROM bets b
                JOIN matches m ON b.match_id = m.match_id
                WHERE b.guild_id = ? AND b.match_id IS NOT NULL
                GROUP BY b.discord_id
                HAVING COUNT(*) >= ?
                ORDER BY net_pnl DESC
                """,
                (normalized_guild, min_bets),
            )
            rows = cursor.fetchall()
            results = []
            for row in rows:
                data = dict(row)
                data["win_rate"] = data["wins"] / data["total_bets"] if data["total_bets"] > 0 else 0
                data["roi"] = data["net_pnl"] / data["total_wagered"] if data["total_wagered"] > 0 else 0
                results.append(data)
            return results

    def get_player_matches_without_self_bet(self, discord_id: int) -> dict:
        """
        Count matches where player participated but didn't bet on themselves.

        Returns dict with: matches_played, matches_bet_on_self, paper_hands_count
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            # Get all matches the player participated in
            cursor.execute(
                """
                SELECT
                    mp.match_id,
                    mp.team_number,
                    CASE WHEN mp.team_number = 1 THEN 'radiant' ELSE 'dire' END as player_team
                FROM match_participants mp
                WHERE mp.discord_id = ?
                """,
                (discord_id,),
            )
            player_matches = {row["match_id"]: row["player_team"] for row in cursor.fetchall()}

            if not player_matches:
                return {"matches_played": 0, "matches_bet_on_self": 0, "paper_hands_count": 0}

            # Get bets this player made on those matches
            placeholders = ",".join("?" * len(player_matches))
            cursor.execute(
                f"""
                SELECT match_id, team_bet_on
                FROM bets
                WHERE discord_id = ? AND match_id IN ({placeholders})
                """,
                (discord_id, *player_matches.keys()),
            )
            bets_by_match = {row["match_id"]: row["team_bet_on"] for row in cursor.fetchall()}

            matches_played = len(player_matches)
            matches_bet_on_self = sum(
                1 for match_id, team in player_matches.items()
                if bets_by_match.get(match_id) == team
            )
            # Paper hands = played but either didn't bet or bet against self (shouldn't be possible)
            paper_hands_count = matches_played - matches_bet_on_self

            return {
                "matches_played": matches_played,
                "matches_bet_on_self": matches_bet_on_self,
                "paper_hands_count": paper_hands_count,
            }

    def get_player_leverage_distribution(self, discord_id: int) -> dict[int, int]:
        """Get count of bets at each leverage level for a player."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT COALESCE(leverage, 1) as leverage, COUNT(*) as count
                FROM bets
                WHERE discord_id = ? AND match_id IS NOT NULL
                GROUP BY COALESCE(leverage, 1)
                """,
                (discord_id,),
            )
            return {row["leverage"]: row["count"] for row in cursor.fetchall()}

    def get_player_bankruptcy_count(self, discord_id: int) -> int:
        """Get the number of times a player has declared bankruptcy."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT COALESCE(bankruptcy_count, 0) as count
                FROM bankruptcy_state
                WHERE discord_id = ?
                """,
                (discord_id,),
            )
            row = cursor.fetchone()
            return row["count"] if row else 0

    def count_player_loss_chasing(self, discord_id: int) -> dict:
        """
        Analyze loss chasing behavior: how often does player increase bet after a loss?

        Returns dict with: sequences_analyzed, times_increased_after_loss, loss_chase_rate
        """
        history = self.get_player_bet_history(discord_id)
        if len(history) < 2:
            return {"sequences_analyzed": 0, "times_increased_after_loss": 0, "loss_chase_rate": 0.0}

        times_increased_after_loss = 0
        loss_sequences = 0

        for i in range(1, len(history)):
            prev_bet = history[i - 1]
            curr_bet = history[i]

            if prev_bet["outcome"] == "lost":
                loss_sequences += 1
                if curr_bet["effective_bet"] > prev_bet["effective_bet"]:
                    times_increased_after_loss += 1

        loss_chase_rate = times_increased_after_loss / loss_sequences if loss_sequences > 0 else 0.0

        return {
            "sequences_analyzed": loss_sequences,
            "times_increased_after_loss": times_increased_after_loss,
            "loss_chase_rate": loss_chase_rate,
        }
