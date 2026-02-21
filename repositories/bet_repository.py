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

    def create_bet(
        self, guild_id: int | None, discord_id: int, team: str, amount: int, bet_time: int
    ) -> int:
        """
        Place a bet for the current pending match.
        """
        normalized_guild = self.normalize_guild_id(guild_id)
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
        allow_negative: bool = False,
        pending_match_id: int | None = None,
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
            allow_negative: If True, allows going into debt at 1x leverage (for bomb pot antes)
            pending_match_id: Optional ID of the pending match this bet is for (for concurrent matches)

        This prevents race conditions where concurrent calls could double-spend.
        """
        if amount <= 0:
            raise ValueError("Bet amount must be positive.")
        if team not in self.VALID_TEAMS:
            raise ValueError("Invalid team selection.")
        if leverage < 1:
            raise ValueError("Leverage must be at least 1.")

        effective_bet = amount * leverage
        normalized_guild = self.normalize_guild_id(guild_id)

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            # Check for existing bets - allow additional bets only on the same team
            # When pending_match_id is provided, filter by it; otherwise use since_ts
            if pending_match_id is not None:
                cursor.execute(
                    """
                    SELECT team_bet_on
                    FROM bets
                    WHERE guild_id = ? AND discord_id = ? AND match_id IS NULL AND pending_match_id = ?
                    """,
                    (normalized_guild, discord_id, pending_match_id),
                )
            else:
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
                "SELECT COALESCE(jopacoin_balance, 0) as balance FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, normalized_guild),
            )
            row = cursor.fetchone()
            if not row:
                raise ValueError("Player not found.")

            balance = int(row["balance"])

            # Users in debt cannot place any bets (unless allow_negative for bomb pot)
            if balance < 0 and not allow_negative:
                raise ValueError(
                    "You cannot place bets while in debt. Win some games to pay it off!"
                )

            # Balance check depends on leverage and allow_negative:
            # - No leverage (1x): cannot go negative, must have enough balance
            # - With leverage (>1x): can go into debt up to -max_debt
            # - allow_negative=True (bomb pot): can go into debt at 1x leverage up to -max_debt
            new_balance = balance - effective_bet
            if allow_negative:
                # Bomb pot mode: allow going into debt up to max_debt at 1x leverage
                if new_balance < -max_debt:
                    raise ValueError(f"Bet would exceed maximum debt limit of {max_debt} jopacoin.")
            elif leverage == 1:
                if balance < amount:
                    raise ValueError(f"Insufficient balance. You have {balance} jopacoin.")
            else:
                if new_balance < -max_debt:
                    raise ValueError(f"Bet would exceed maximum debt limit of {max_debt} jopacoin.")

            cursor.execute(
                """
                UPDATE players
                SET jopacoin_balance = COALESCE(jopacoin_balance, 0) - ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (effective_bet, discord_id, normalized_guild),
            )

            cursor.execute(
                """
                INSERT INTO bets (guild_id, match_id, discord_id, team_bet_on, amount, bet_time, leverage, is_blind, odds_at_placement, pending_match_id)
                VALUES (?, NULL, ?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (normalized_guild, discord_id, team, amount, bet_time, leverage, 1 if is_blind else 0, odds_at_placement, pending_match_id),
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
        pending_match_id: int | None = None,
    ) -> int:
        """
        Atomically place a bet with optional leverage using the DB as the source of truth.

        Uses `pending_matches.payload` to enforce:
        - there is an active pending match
        - betting is still open (bet_lock_until)
        - participants may only bet on their own team
        - per-match-window duplicate-bet prevention (pending_match_id)
        - sufficient balance or debt limit, then debits + inserts bet in the same transaction

        Args:
            pending_match_id: Optional ID of specific pending match. If None, auto-detects
                             (works for single pending match, or if player is in a match)
        """
        if amount <= 0:
            raise ValueError("Bet amount must be positive.")
        if team not in self.VALID_TEAMS:
            raise ValueError("Invalid team selection.")
        if leverage < 1:
            raise ValueError("Leverage must be at least 1.")

        effective_bet = amount * leverage
        normalized_guild = self.normalize_guild_id(guild_id)

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            # Get the pending match - either by ID or auto-detect
            if pending_match_id is not None:
                cursor.execute(
                    "SELECT pending_match_id, payload FROM pending_matches WHERE pending_match_id = ? AND guild_id = ?",
                    (pending_match_id, normalized_guild),
                )
                row = cursor.fetchone()
            else:
                # Auto-detect: if single match, use it; if multiple, check if player is in one
                cursor.execute(
                    "SELECT pending_match_id, payload FROM pending_matches WHERE guild_id = ?",
                    (normalized_guild,),
                )
                rows = cursor.fetchall()
                if not rows:
                    raise ValueError("No pending match to bet on.")

                if len(rows) == 1:
                    row = rows[0]
                else:
                    # Multiple matches - find the one the player is in
                    row = None
                    for r in rows:
                        try:
                            p = json.loads(r["payload"])
                            radiant = set(p.get("radiant_team_ids") or [])
                            dire = set(p.get("dire_team_ids") or [])
                            if discord_id in radiant or discord_id in dire:
                                row = r
                                break
                        except Exception:
                            continue
                    if row is None:
                        raise ValueError(
                            "Multiple pending matches exist. Please specify which match to bet on."
                        )

            if not row:
                raise ValueError("No pending match to bet on.")

            actual_pending_match_id = row["pending_match_id"]
            try:
                payload = json.loads(row["payload"])
            except Exception:
                raise ValueError("No pending match to bet on.") from None

            lock_until = payload.get("bet_lock_until")
            if lock_until is None or int(bet_time) >= int(lock_until):
                raise ValueError("Betting is closed for the current match.")

            radiant_ids = set(payload.get("radiant_team_ids") or [])
            dire_ids = set(payload.get("dire_team_ids") or [])
            if discord_id in radiant_ids and team != "radiant":
                raise ValueError("Participants on Radiant can only bet on Radiant.")
            if discord_id in dire_ids and team != "dire":
                raise ValueError("Participants on Dire can only bet on Dire.")

            # Check for existing bets - allow additional bets only on the same team
            # Filter by pending_match_id to support concurrent matches
            cursor.execute(
                """
                SELECT team_bet_on
                FROM bets
                WHERE guild_id = ? AND discord_id = ? AND match_id IS NULL AND pending_match_id = ?
                """,
                (normalized_guild, discord_id, actual_pending_match_id),
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
                "SELECT COALESCE(jopacoin_balance, 0) as balance FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, normalized_guild),
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
                WHERE discord_id = ? AND guild_id = ?
                """,
                (effective_bet, discord_id, normalized_guild),
            )
            cursor.execute(
                """
                INSERT INTO bets (guild_id, match_id, discord_id, team_bet_on, amount, bet_time, leverage, is_blind, odds_at_placement, pending_match_id)
                VALUES (?, NULL, ?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (normalized_guild, discord_id, team, amount, bet_time, leverage, 1 if is_blind else 0, odds_at_placement, actual_pending_match_id),
            )
            return cursor.lastrowid

    def get_player_pending_bet(
        self, guild_id: int | None, discord_id: int, since_ts: int | None = None,
        pending_match_id: int | None = None,
    ) -> dict | None:
        """
        Return the bet placed by a player for the pending match in the guild.

        Args:
            pending_match_id: If provided, filter by this specific pending match.
                             Also includes legacy bets (pending_match_id IS NULL) with matching since_ts.
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()

            if pending_match_id is not None and since_ts is not None:
                # Match by pending_match_id OR legacy bets (NULL) with matching timestamp
                cursor.execute(
                    """
                    SELECT bet_id, guild_id, match_id, discord_id, team_bet_on, amount, bet_time, created_at,
                           COALESCE(leverage, 1) as leverage,
                           COALESCE(is_blind, 0) as is_blind,
                           odds_at_placement, pending_match_id
                    FROM bets
                    WHERE guild_id = ? AND discord_id = ? AND match_id IS NULL
                          AND (pending_match_id = ? OR (pending_match_id IS NULL AND bet_time >= ?))
                    """,
                    (normalized_guild, discord_id, pending_match_id, since_ts),
                )
            elif pending_match_id is not None:
                cursor.execute(
                    """
                    SELECT bet_id, guild_id, match_id, discord_id, team_bet_on, amount, bet_time, created_at,
                           COALESCE(leverage, 1) as leverage,
                           COALESCE(is_blind, 0) as is_blind,
                           odds_at_placement, pending_match_id
                    FROM bets
                    WHERE guild_id = ? AND discord_id = ? AND match_id IS NULL AND pending_match_id = ?
                    """,
                    (normalized_guild, discord_id, pending_match_id),
                )
            elif since_ts is not None:
                cursor.execute(
                    """
                    SELECT bet_id, guild_id, match_id, discord_id, team_bet_on, amount, bet_time, created_at,
                           COALESCE(leverage, 1) as leverage,
                           COALESCE(is_blind, 0) as is_blind,
                           odds_at_placement, pending_match_id
                    FROM bets
                    WHERE guild_id = ? AND discord_id = ? AND match_id IS NULL AND bet_time >= ?
                    """,
                    (normalized_guild, discord_id, since_ts),
                )
            else:
                cursor.execute(
                    """
                    SELECT bet_id, guild_id, match_id, discord_id, team_bet_on, amount, bet_time, created_at,
                           COALESCE(leverage, 1) as leverage,
                           COALESCE(is_blind, 0) as is_blind,
                           odds_at_placement, pending_match_id
                    FROM bets
                    WHERE guild_id = ? AND discord_id = ? AND match_id IS NULL
                    """,
                    (normalized_guild, discord_id),
                )

            row = cursor.fetchone()
            return dict(row) if row else None

    def get_player_pending_bets(
        self, guild_id: int | None, discord_id: int, since_ts: int | None = None,
        pending_match_id: int | None = None,
    ) -> list[dict]:
        """
        Return all bets placed by a player for the pending match in the guild.
        Ordered by bet_time ascending.

        Args:
            pending_match_id: If provided, filter by this specific pending match.
                             Also includes legacy bets (pending_match_id IS NULL) with matching since_ts.
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()

            if pending_match_id is not None and since_ts is not None:
                # Match by pending_match_id OR legacy bets (NULL) with matching timestamp
                cursor.execute(
                    """
                    SELECT bet_id, guild_id, match_id, discord_id, team_bet_on, amount, bet_time, created_at,
                           COALESCE(leverage, 1) as leverage,
                           COALESCE(is_blind, 0) as is_blind,
                           odds_at_placement, pending_match_id
                    FROM bets
                    WHERE guild_id = ? AND discord_id = ? AND match_id IS NULL
                          AND (pending_match_id = ? OR (pending_match_id IS NULL AND bet_time >= ?))
                    ORDER BY bet_time ASC
                    """,
                    (normalized_guild, discord_id, pending_match_id, since_ts),
                )
            elif pending_match_id is not None:
                cursor.execute(
                    """
                    SELECT bet_id, guild_id, match_id, discord_id, team_bet_on, amount, bet_time, created_at,
                           COALESCE(leverage, 1) as leverage,
                           COALESCE(is_blind, 0) as is_blind,
                           odds_at_placement, pending_match_id
                    FROM bets
                    WHERE guild_id = ? AND discord_id = ? AND match_id IS NULL AND pending_match_id = ?
                    ORDER BY bet_time ASC
                    """,
                    (normalized_guild, discord_id, pending_match_id),
                )
            elif since_ts is not None:
                cursor.execute(
                    """
                    SELECT bet_id, guild_id, match_id, discord_id, team_bet_on, amount, bet_time, created_at,
                           COALESCE(leverage, 1) as leverage,
                           COALESCE(is_blind, 0) as is_blind,
                           odds_at_placement, pending_match_id
                    FROM bets
                    WHERE guild_id = ? AND discord_id = ? AND match_id IS NULL AND bet_time >= ?
                    ORDER BY bet_time ASC
                    """,
                    (normalized_guild, discord_id, since_ts),
                )
            else:
                cursor.execute(
                    """
                    SELECT bet_id, guild_id, match_id, discord_id, team_bet_on, amount, bet_time, created_at,
                           COALESCE(leverage, 1) as leverage,
                           COALESCE(is_blind, 0) as is_blind,
                           odds_at_placement, pending_match_id
                    FROM bets
                    WHERE guild_id = ? AND discord_id = ? AND match_id IS NULL
                    ORDER BY bet_time ASC
                    """,
                    (normalized_guild, discord_id),
                )
            return [dict(row) for row in cursor.fetchall()]

    def get_all_player_pending_bets(
        self, guild_id: int | None, discord_id: int
    ) -> list[dict]:
        """
        Return all pending bets for a player across ALL pending matches.
        Useful for /mybets display when multiple matches are pending.

        Returns bets grouped by pending_match_id.
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT bet_id, guild_id, match_id, discord_id, team_bet_on, amount, bet_time, created_at,
                       COALESCE(leverage, 1) as leverage,
                       COALESCE(is_blind, 0) as is_blind,
                       odds_at_placement, pending_match_id
                FROM bets
                WHERE guild_id = ? AND discord_id = ? AND match_id IS NULL
                ORDER BY pending_match_id, bet_time ASC
                """,
                (normalized_guild, discord_id),
            )
            return [dict(row) for row in cursor.fetchall()]

    def get_bets_for_pending_match(
        self, guild_id: int | None, since_ts: int | None = None,
        pending_match_id: int | None = None,
    ) -> list[dict]:
        """
        Return bets associated with the pending match for a guild.

        Args:
            pending_match_id: If provided, filter by this specific pending match.
                             Also includes legacy bets (pending_match_id IS NULL) with matching since_ts.
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()

            if pending_match_id is not None and since_ts is not None:
                # Match by pending_match_id OR legacy bets (NULL) with matching timestamp
                cursor.execute(
                    """
                    SELECT bet_id, guild_id, match_id, discord_id, team_bet_on, amount, bet_time, created_at,
                           COALESCE(leverage, 1) as leverage,
                           COALESCE(is_blind, 0) as is_blind,
                           odds_at_placement, pending_match_id
                    FROM bets
                    WHERE guild_id = ? AND match_id IS NULL
                          AND (pending_match_id = ? OR (pending_match_id IS NULL AND bet_time >= ?))
                    ORDER BY bet_time ASC
                    """,
                    (normalized_guild, pending_match_id, since_ts),
                )
            elif pending_match_id is not None:
                cursor.execute(
                    """
                    SELECT bet_id, guild_id, match_id, discord_id, team_bet_on, amount, bet_time, created_at,
                           COALESCE(leverage, 1) as leverage,
                           COALESCE(is_blind, 0) as is_blind,
                           odds_at_placement, pending_match_id
                    FROM bets
                    WHERE guild_id = ? AND match_id IS NULL AND pending_match_id = ?
                    ORDER BY bet_time ASC
                    """,
                    (normalized_guild, pending_match_id),
                )
            elif since_ts is not None:
                cursor.execute(
                    """
                    SELECT bet_id, guild_id, match_id, discord_id, team_bet_on, amount, bet_time, created_at,
                           COALESCE(leverage, 1) as leverage,
                           COALESCE(is_blind, 0) as is_blind,
                           odds_at_placement, pending_match_id
                    FROM bets
                    WHERE guild_id = ? AND match_id IS NULL AND bet_time >= ?
                    ORDER BY bet_time ASC
                    """,
                    (normalized_guild, since_ts),
                )
            else:
                cursor.execute(
                    """
                    SELECT bet_id, guild_id, match_id, discord_id, team_bet_on, amount, bet_time, created_at,
                           COALESCE(leverage, 1) as leverage,
                           COALESCE(is_blind, 0) as is_blind,
                           odds_at_placement, pending_match_id
                    FROM bets
                    WHERE guild_id = ? AND match_id IS NULL
                    ORDER BY bet_time ASC
                    """,
                    (normalized_guild,),
                )
            return [dict(row) for row in cursor.fetchall()]

    def delete_bets_for_guild(self, guild_id: int | None) -> int:
        """Remove all bets for the specified guild."""
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("DELETE FROM bets WHERE guild_id = ?", (normalized_guild,))
            return cursor.rowcount

    def get_total_bets_by_guild(
        self, guild_id: int | None, since_ts: int | None = None,
        pending_match_id: int | None = None,
    ) -> dict[str, int]:
        """Return total effective wager amounts grouped by team for a guild.

        Effective amount = amount * leverage, used for pool mode calculations.

        Args:
            pending_match_id: If provided, filter by this specific pending match
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()

            if pending_match_id is not None:
                cursor.execute(
                    """
                    SELECT team_bet_on, SUM(amount * COALESCE(leverage, 1)) as total
                    FROM bets
                    WHERE guild_id = ? AND match_id IS NULL AND pending_match_id = ?
                    GROUP BY team_bet_on
                    """,
                    (normalized_guild, pending_match_id),
                )
            elif since_ts is not None:
                cursor.execute(
                    """
                    SELECT team_bet_on, SUM(amount * COALESCE(leverage, 1)) as total
                    FROM bets
                    WHERE guild_id = ? AND match_id IS NULL AND bet_time >= ?
                    GROUP BY team_bet_on
                    """,
                    (normalized_guild, since_ts),
                )
            else:
                cursor.execute(
                    """
                    SELECT team_bet_on, SUM(amount * COALESCE(leverage, 1)) as total
                    FROM bets
                    WHERE guild_id = ? AND match_id IS NULL
                    GROUP BY team_bet_on
                    """,
                    (normalized_guild,),
                )
            totals = {row["team_bet_on"]: row["total"] for row in cursor.fetchall()}
            return {team: totals.get(team, 0) for team in self.VALID_TEAMS}

    def assign_match_id(
        self, guild_id: int | None, match_id: int, since_ts: int | None = None,
        pending_match_id: int | None = None,
    ) -> None:
        """Tie all pending bets for the current match window to a recorded match.

        Args:
            pending_match_id: If provided, only update bets for this specific pending match
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()

            if pending_match_id is not None:
                cursor.execute(
                    """
                    UPDATE bets
                    SET match_id = ?
                    WHERE guild_id = ? AND match_id IS NULL AND pending_match_id = ?
                    """,
                    (match_id, normalized_guild, pending_match_id),
                )
            elif since_ts is not None:
                cursor.execute(
                    """
                    UPDATE bets
                    SET match_id = ?
                    WHERE guild_id = ? AND match_id IS NULL AND bet_time >= ?
                    """,
                    (match_id, normalized_guild, since_ts),
                )
            else:
                cursor.execute(
                    """
                    UPDATE bets
                    SET match_id = ?
                    WHERE guild_id = ? AND match_id IS NULL
                    """,
                    (match_id, normalized_guild),
                )

    def delete_pending_bets(
        self, guild_id: int | None, since_ts: int | None = None,
        pending_match_id: int | None = None,
    ) -> int:
        """Delete pending bets (match_id IS NULL) for the current match window.

        Args:
            pending_match_id: If provided, only delete bets for this specific pending match
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()

            if pending_match_id is not None:
                cursor.execute(
                    "DELETE FROM bets WHERE guild_id = ? AND match_id IS NULL AND pending_match_id = ?",
                    (normalized_guild, pending_match_id),
                )
            elif since_ts is not None:
                cursor.execute(
                    "DELETE FROM bets WHERE guild_id = ? AND match_id IS NULL AND bet_time >= ?",
                    (normalized_guild, since_ts),
                )
            else:
                cursor.execute(
                    "DELETE FROM bets WHERE guild_id = ? AND match_id IS NULL",
                    (normalized_guild,),
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
        pending_match_id: int | None = None,
    ) -> dict[str, list[dict]]:
        """
        Atomically settle bets for the current match window:
        - credit winners in players.jopacoin_balance (based on effective bet with leverage)
        - tag all pending bets with match_id

        Args:
            betting_mode: "pool" for parimutuel betting, "house" for 1:1 payouts
            pending_match_id: If provided, settle bets for this specific pending match.
                             Also includes legacy bets (pending_match_id IS NULL) with matching since_ts.
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        distributions: dict[str, list[dict]] = {"winners": [], "losers": []}

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            if pending_match_id is not None:
                # Match by pending_match_id OR legacy bets (NULL) with matching timestamp
                cursor.execute(
                    """
                    SELECT bet_id, discord_id, team_bet_on, amount, COALESCE(leverage, 1) as leverage
                    FROM bets
                    WHERE guild_id = ? AND match_id IS NULL
                          AND (pending_match_id = ? OR (pending_match_id IS NULL AND bet_time >= ?))
                    """,
                    (normalized_guild, pending_match_id, since_ts),
                )
            else:
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
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    [(delta, discord_id, normalized_guild) for discord_id, delta in balance_deltas.items()],
                )

            # Store payout for winning bets
            if payout_updates:
                cursor.executemany(
                    "UPDATE bets SET payout = ? WHERE bet_id = ?",
                    payout_updates,
                )

            # Tag settled bets with match_id
            if pending_match_id is not None:
                cursor.execute(
                    """
                    UPDATE bets
                    SET match_id = ?
                    WHERE guild_id = ? AND match_id IS NULL AND pending_match_id = ?
                    """,
                    (match_id, normalized_guild, pending_match_id),
                )
            else:
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
        """Calculate pool mode payouts (proportional from total pool) with leverage support.

        Payouts are aggregated per user before applying ceiling to prevent exploits
        where splitting bets into many small wagers gains extra coins from rounding.
        """
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

        # First pass: calculate raw payouts and group winning bets by user
        winning_bets_by_user: dict[int, list[dict]] = {}
        raw_payout_by_user: dict[int, float] = {}

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

            # Calculate raw (unrounded) payout for this bet
            raw_payout = (effective_bet / winner_pool) * total_pool
            entry["raw_payout"] = raw_payout
            entry["multiplier"] = multiplier

            discord_id = bet["discord_id"]
            if discord_id not in winning_bets_by_user:
                winning_bets_by_user[discord_id] = []
                raw_payout_by_user[discord_id] = 0.0
            winning_bets_by_user[discord_id].append(entry)
            raw_payout_by_user[discord_id] += raw_payout

        # Second pass: apply ceiling once per user and distribute to individual bets
        for discord_id, bets in winning_bets_by_user.items():
            user_raw_total = raw_payout_by_user[discord_id]
            user_final_payout = math.ceil(user_raw_total)
            balance_deltas[discord_id] = user_final_payout

            # Distribute payout proportionally across user's bets
            # Use floor for all but the last bet to avoid over-allocation
            allocated = 0
            for i, entry in enumerate(bets):
                if i == len(bets) - 1:
                    # Last bet gets the remainder to ensure exact total
                    bet_payout = user_final_payout - allocated
                else:
                    # Proportional share, floored
                    bet_payout = int((entry["raw_payout"] / user_raw_total) * user_final_payout)
                    allocated += bet_payout

                entry["payout"] = bet_payout
                payout_updates.append((bet_payout, entry["bet_id"]))
                del entry["raw_payout"]  # Clean up internal field
                distributions["winners"].append(entry)

        return distributions, balance_deltas, payout_updates

    def refund_pending_bets_atomic(
        self, *, guild_id: int | None, since_ts: int, pending_match_id: int | None = None
    ) -> int:
        """
        Atomically refund + delete pending bets for the current match window.
        Returns number of bets refunded.

        Refunds the effective bet amount (amount * leverage).

        Args:
            pending_match_id: If provided, refund bets for this specific pending match.
                             Also includes legacy bets (pending_match_id IS NULL) with matching since_ts.
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            if pending_match_id is not None:
                # Match by pending_match_id OR legacy bets (NULL) with matching timestamp
                cursor.execute(
                    """
                    SELECT discord_id, amount, COALESCE(leverage, 1) as leverage, bet_id
                    FROM bets
                    WHERE guild_id = ? AND match_id IS NULL
                          AND (pending_match_id = ? OR (pending_match_id IS NULL AND bet_time >= ?))
                    """,
                    (normalized_guild, pending_match_id, since_ts),
                )
            else:
                cursor.execute(
                    """
                    SELECT discord_id, amount, COALESCE(leverage, 1) as leverage, bet_id
                    FROM bets
                    WHERE guild_id = ? AND match_id IS NULL AND bet_time >= ?
                    """,
                    (normalized_guild, since_ts),
                )
            rows = cursor.fetchall()
            if not rows:
                return 0

            refund_deltas: dict[int, int] = {}
            bet_ids = []
            for row in rows:
                # Refund the effective bet (amount * leverage)
                effective_bet = int(row["amount"]) * int(row["leverage"])
                refund_deltas[row["discord_id"]] = (
                    refund_deltas.get(row["discord_id"], 0) + effective_bet
                )
                bet_ids.append(row["bet_id"])

            cursor.executemany(
                """
                UPDATE players
                SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                [(delta, discord_id, normalized_guild) for discord_id, delta in refund_deltas.items()],
            )

            # Delete by bet_id for precise targeting
            if bet_ids:
                placeholders = ",".join("?" * len(bet_ids))
                cursor.execute(
                    f"DELETE FROM bets WHERE bet_id IN ({placeholders})",
                    bet_ids,
                )
            return len(bet_ids)

    def get_player_bet_history(self, discord_id: int, guild_id: int | None = None) -> list[dict]:
        """
        Get all settled bets for a player with outcome derived from match result.

        Returns list of dicts with: bet_id, amount, leverage, effective_bet, team_bet_on,
        bet_time, match_id, payout, outcome ('won'/'lost'), profit (net P&L for this bet)
        """
        normalized_guild_id = guild_id if guild_id is not None else 0
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
                WHERE b.discord_id = ? AND b.guild_id = ? AND b.match_id IS NOT NULL
                ORDER BY b.bet_time ASC
                """,
                (discord_id, normalized_guild_id),
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
        normalized_guild = self.normalize_guild_id(guild_id)
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

    def get_player_matches_without_self_bet(self, discord_id: int, guild_id: int | None = None) -> dict:
        """
        Count matches where player participated but didn't bet on themselves.

        Returns dict with: matches_played, matches_bet_on_self, paper_hands_count
        """
        normalized_guild_id = guild_id if guild_id is not None else 0
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
                WHERE mp.discord_id = ? AND mp.guild_id = ?
                """,
                (discord_id, normalized_guild_id),
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
                WHERE discord_id = ? AND guild_id = ? AND match_id IN ({placeholders})
                """,
                (discord_id, normalized_guild_id, *player_matches.keys()),
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

    def get_player_leverage_distribution(self, discord_id: int, guild_id: int | None = None) -> dict[int, int]:
        """Get count of bets at each leverage level for a player."""
        normalized_guild_id = guild_id if guild_id is not None else 0
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT COALESCE(leverage, 1) as leverage, COUNT(*) as count
                FROM bets
                WHERE discord_id = ? AND guild_id = ? AND match_id IS NOT NULL
                GROUP BY COALESCE(leverage, 1)
                """,
                (discord_id, normalized_guild_id),
            )
            return {row["leverage"]: row["count"] for row in cursor.fetchall()}

    def get_player_bankruptcy_count(self, discord_id: int, guild_id: int | None = None) -> int:
        """Get the number of times a player has declared bankruptcy."""
        normalized_guild_id = guild_id if guild_id is not None else 0
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT COALESCE(bankruptcy_count, 0) as count
                FROM bankruptcy_state
                WHERE discord_id = ? AND guild_id = ?
                """,
                (discord_id, normalized_guild_id),
            )
            row = cursor.fetchone()
            return row["count"] if row else 0

    def count_player_loss_chasing(self, discord_id: int, guild_id: int | None = None) -> dict:
        """
        Analyze loss chasing behavior: how often does player increase bet after a loss?

        Returns dict with: sequences_analyzed, times_increased_after_loss, loss_chase_rate
        """
        history = self.get_player_bet_history(discord_id, guild_id)
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

    def get_bulk_leverage_distribution(
        self, guild_id: int | None, discord_ids: list[int]
    ) -> dict[int, dict[int, int]]:
        """
        Get leverage distribution for multiple players in a single query.

        Returns dict[discord_id, dict[leverage, count]] for efficient batch processing.
        """
        if not discord_ids:
            return {}

        normalized_guild = self.normalize_guild_id(guild_id)
        placeholders = ",".join("?" * len(discord_ids))

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"""
                SELECT discord_id, COALESCE(leverage, 1) as leverage, COUNT(*) as count
                FROM bets
                WHERE guild_id = ? AND match_id IS NOT NULL AND discord_id IN ({placeholders})
                GROUP BY discord_id, COALESCE(leverage, 1)
                """,
                (normalized_guild, *discord_ids),
            )
            rows = cursor.fetchall()

        # Build nested dict structure
        result: dict[int, dict[int, int]] = {did: {} for did in discord_ids}
        for row in rows:
            discord_id = row["discord_id"]
            leverage = row["leverage"]
            count = row["count"]
            if discord_id in result:
                result[discord_id][leverage] = count

        return result

    def get_bulk_loss_chasing_data(
        self, guild_id: int | None, discord_ids: list[int]
    ) -> dict[int, dict]:
        """
        Get loss chasing data for multiple players in a single query.

        Returns dict[discord_id, {"sequences_analyzed": int, "times_increased_after_loss": int}]
        """
        if not discord_ids:
            return {}

        normalized_guild = self.normalize_guild_id(guild_id)
        placeholders = ",".join("?" * len(discord_ids))

        with self.connection() as conn:
            cursor = conn.cursor()
            # Get bet history with outcome for all players in one query
            cursor.execute(
                f"""
                SELECT
                    b.discord_id,
                    b.bet_id,
                    b.amount * COALESCE(b.leverage, 1) as effective_bet,
                    b.bet_time,
                    CASE
                        WHEN (m.winning_team = 1 AND b.team_bet_on = 'radiant')
                          OR (m.winning_team = 2 AND b.team_bet_on = 'dire')
                        THEN 'won' ELSE 'lost'
                    END as outcome
                FROM bets b
                JOIN matches m ON b.match_id = m.match_id
                WHERE b.guild_id = ? AND b.match_id IS NOT NULL AND b.discord_id IN ({placeholders})
                ORDER BY b.discord_id, b.bet_time ASC
                """,
                (normalized_guild, *discord_ids),
            )
            rows = cursor.fetchall()

        # Group by discord_id and calculate loss chasing
        result: dict[int, dict] = {
            did: {"sequences_analyzed": 0, "times_increased_after_loss": 0}
            for did in discord_ids
        }

        # Process rows grouped by player
        current_player: int | None = None
        player_history: list[dict] = []

        def process_player_history(player_id: int, history: list[dict]) -> None:
            if len(history) < 2:
                return
            times_increased = 0
            loss_sequences = 0
            for i in range(1, len(history)):
                if history[i - 1]["outcome"] == "lost":
                    loss_sequences += 1
                    if history[i]["effective_bet"] > history[i - 1]["effective_bet"]:
                        times_increased += 1
            result[player_id]["sequences_analyzed"] = loss_sequences
            result[player_id]["times_increased_after_loss"] = times_increased

        for row in rows:
            discord_id = row["discord_id"]
            if discord_id != current_player:
                if current_player is not None:
                    process_player_history(current_player, player_history)
                current_player = discord_id
                player_history = []
            player_history.append(dict(row))

        # Process last player
        if current_player is not None:
            process_player_history(current_player, player_history)

        return result

    def get_bulk_bankruptcy_counts(self, discord_ids: list[int], guild_id: int | None = None) -> dict[int, int]:
        """
        Get bankruptcy counts for multiple players in a single query.

        Returns dict[discord_id, count].
        """
        if not discord_ids:
            return {}

        normalized_guild = self.normalize_guild_id(guild_id)
        placeholders = ",".join("?" * len(discord_ids))

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"""
                SELECT discord_id, COALESCE(bankruptcy_count, 0) as count
                FROM bankruptcy_state
                WHERE guild_id = ? AND discord_id IN ({placeholders})
                """,
                [normalized_guild] + discord_ids,
            )
            rows = cursor.fetchall()

        result = {did: 0 for did in discord_ids}
        for row in rows:
            result[row["discord_id"]] = row["count"]

        return result

    def get_total_settled_matches(self, guild_id: int | None = None) -> int:
        """
        Get total count of settled matches (for degen score frequency calculation).

        Args:
            guild_id: Guild filter for match count.
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT COUNT(*) as count FROM matches WHERE guild_id = ? AND winning_team IS NOT NULL",
                (normalized_guild,),
            )
            row = cursor.fetchone()
            return row["count"] if row else 0

    def get_settled_bets_for_match(self, match_id: int) -> list[dict]:
        """
        Get all bets with their payout amounts for a specific match.

        Used for match correction to reverse payouts.

        Returns:
            List of dicts with bet_id, discord_id, team_bet_on, amount, leverage,
            effective_bet, payout, and outcome
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    b.bet_id,
                    b.discord_id,
                    b.team_bet_on,
                    b.amount,
                    COALESCE(b.leverage, 1) as leverage,
                    b.amount * COALESCE(b.leverage, 1) as effective_bet,
                    b.payout,
                    CASE
                        WHEN m.winning_team = 1 AND b.team_bet_on = 'radiant' THEN 'won'
                        WHEN m.winning_team = 2 AND b.team_bet_on = 'dire' THEN 'won'
                        ELSE 'lost'
                    END as outcome
                FROM bets b
                JOIN matches m ON b.match_id = m.match_id
                WHERE b.match_id = ?
                ORDER BY b.bet_time ASC
                """,
                (match_id,),
            )
            return [dict(row) for row in cursor.fetchall()]

    def reverse_bet_payouts_for_correction(
        self,
        match_id: int,
        old_winners: list[dict],
    ) -> dict[int, int]:
        """
        Reverse bet payouts for match correction.

        Subtracts payout from winners who are now losers.
        Does NOT refund losers (they already lost their stake).

        Args:
            match_id: The match being corrected
            old_winners: List of bet dicts for bets that previously won

        Returns:
            Dict mapping discord_id -> amount subtracted from their balance
        """
        balance_deltas: dict[int, int] = {}

        for bet in old_winners:
            payout = bet.get("payout") or 0
            if payout > 0:
                discord_id = bet["discord_id"]
                balance_deltas[discord_id] = balance_deltas.get(discord_id, 0) - payout

        return balance_deltas

    def apply_new_bet_payouts_for_correction(
        self,
        match_id: int,
        new_winners: list[dict],
        pool_mode: bool = True,
    ) -> dict[int, int]:
        """
        Apply new bet payouts after correction.

        For the new winners: they get their stakes back + winnings.
        For pool mode: recalculate based on pool proportions.
        For house mode: double the effective bet.

        Args:
            match_id: The match being corrected
            new_winners: List of bet dicts for bets that now win
            pool_mode: True for parimutuel, False for house mode

        Returns:
            Dict mapping discord_id -> amount to add to their balance
        """
        balance_deltas: dict[int, int] = {}
        payout_updates: list[tuple[int, int]] = []  # (payout, bet_id)

        if pool_mode:
            # Get all bets for the match to calculate pool
            all_bets = self.get_settled_bets_for_match(match_id)
            total_pool = sum(b["effective_bet"] for b in all_bets)
            winner_pool = sum(b["effective_bet"] for b in new_winners)

            if winner_pool == 0:
                # Edge case: no bets on winning side - this shouldn't happen
                # but if it does, no payouts
                return balance_deltas

            multiplier = total_pool / winner_pool

            # Group winners by user
            winners_by_user: dict[int, list[dict]] = {}
            for bet in new_winners:
                discord_id = bet["discord_id"]
                if discord_id not in winners_by_user:
                    winners_by_user[discord_id] = []
                winners_by_user[discord_id].append(bet)

            # Calculate payouts per user with single ceiling
            import math
            for discord_id, bets in winners_by_user.items():
                raw_total = sum((b["effective_bet"] / winner_pool) * total_pool for b in bets)
                user_payout = math.ceil(raw_total)
                balance_deltas[discord_id] = user_payout

                # Distribute across individual bets
                allocated = 0
                for i, bet in enumerate(bets):
                    if i == len(bets) - 1:
                        bet_payout = user_payout - allocated
                    else:
                        bet_payout = int((bet["effective_bet"] / sum(b["effective_bet"] for b in bets)) * user_payout)
                        allocated += bet_payout
                    payout_updates.append((bet_payout, bet["bet_id"]))
        else:
            # House mode: 2x effective bet
            for bet in new_winners:
                payout = bet["effective_bet"] * 2
                discord_id = bet["discord_id"]
                balance_deltas[discord_id] = balance_deltas.get(discord_id, 0) + payout
                payout_updates.append((payout, bet["bet_id"]))

        # Update payout column for new winners and clear payout for new losers atomically
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            if payout_updates:
                cursor.executemany(
                    "UPDATE bets SET payout = ? WHERE bet_id = ?",
                    payout_updates,
                )

            # Clear payout for new losers (old winners)
            cursor.execute(
                """
                UPDATE bets
                SET payout = NULL
                WHERE match_id = ?
                  AND bet_id NOT IN (SELECT bet_id FROM bets WHERE match_id = ? AND payout IS NOT NULL)
                """,
                (match_id, match_id),
            )

        return balance_deltas

    def get_bets_on_player_matches(self, target_discord_id: int, guild_id: int | None = None) -> list[dict]:
        """
        Get all bets by OTHER players on matches where target_discord_id participated.

        This is used to calculate "betting impact" stats - how others' bets fared
        when betting for or against this player's team.

        Returns list of dicts with: bettor_id, match_id, team_bet_on, effective_bet,
        payout, player_team, bet_direction ('for'/'against'), won (bool)
        """
        normalized_guild_id = guild_id if guild_id is not None else 0
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT
                    b.discord_id as bettor_id,
                    b.match_id,
                    b.team_bet_on,
                    b.amount * COALESCE(b.leverage, 1) as effective_bet,
                    b.payout,
                    CASE WHEN mp.team_number = 1 THEN 'radiant' ELSE 'dire' END as player_team,
                    m.winning_team,
                    CASE
                        WHEN b.team_bet_on = CASE WHEN mp.team_number = 1 THEN 'radiant' ELSE 'dire' END
                        THEN 'for'
                        ELSE 'against'
                    END as bet_direction,
                    CASE
                        WHEN (m.winning_team = 1 AND b.team_bet_on = 'radiant')
                          OR (m.winning_team = 2 AND b.team_bet_on = 'dire')
                        THEN 1 ELSE 0
                    END as won
                FROM match_participants mp
                JOIN matches m ON mp.match_id = m.match_id
                JOIN bets b ON b.match_id = m.match_id
                    AND b.discord_id != ?
                    AND b.guild_id = ?
                WHERE mp.discord_id = ?
                    AND mp.guild_id = ?
                    AND m.winning_team IS NOT NULL
                    AND m.guild_id = ?
                ORDER BY b.match_id, b.bet_time
                """,
                (target_discord_id, normalized_guild_id, target_discord_id, normalized_guild_id, normalized_guild_id),
            )
            rows = cursor.fetchall()
            results = []
            for row in rows:
                bet = dict(row)
                effective_bet = bet["effective_bet"]
                # Calculate profit: won = payout - effective_bet, lost = -effective_bet
                if bet["won"]:
                    payout = bet["payout"] if bet["payout"] else effective_bet * 2
                    bet["profit"] = payout - effective_bet
                else:
                    bet["profit"] = -effective_bet
                results.append(bet)
            return results
