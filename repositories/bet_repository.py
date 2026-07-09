"""
Repository for managing betting data.
"""

from __future__ import annotations

import json
import logging
import math

from repositories.base_repository import BaseRepository

logger = logging.getLogger("cama_bot.repositories.bet")
from repositories.interfaces import IBetRepository


def _allows_shuffle_spectator_dual_team(
    payload: dict,
    *,
    discord_id: int,
    radiant_ids: set[int] | None = None,
    dire_ids: set[int] | None = None,
) -> bool:
    """Shuffle spectators (not on either team) may bet both Radiant and Dire."""
    if payload.get("is_draft") is not False:
        return False
    if radiant_ids is None:
        radiant_ids = set(payload.get("radiant_team_ids") or [])
    if dire_ids is None:
        dire_ids = set(payload.get("dire_team_ids") or [])
    return discord_id not in radiant_ids and discord_id not in dire_ids


def _raise_one_side_bet_required(existing_team: str, payload: dict | None) -> None:
    """Raise when a second team bet is not allowed."""
    side = existing_team.title()
    if payload and payload.get("is_draft"):
        raise ValueError(
            f"You already have bets on {side}. Draft matches only allow bets on one team."
        )
    raise ValueError(
        f"You already have bets on {side}. You can only add more bets on the same team."
    )


class BetRepository(BaseRepository, IBetRepository):
    """
    Handles CRUD operations against the bets table.
    """

    VALID_TEAMS = {"radiant", "dire"}
    _PENDING_SEED_FIELDS = (
        "bet_seed_reserved",
        "bet_seed_radiant",
        "bet_seed_dire",
        "bet_seed_bonus",
    )

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

            # Reject opposite-team bets unless shuffle spectator dual-team is allowed
            payload: dict | None = None
            if pending_match_id is not None:
                cursor.execute(
                    """
                    SELECT payload FROM pending_matches
                    WHERE pending_match_id = ? AND guild_id = ?
                    """,
                    (pending_match_id, normalized_guild),
                )
                pm_row = cursor.fetchone()
                if pm_row:
                    try:
                        payload = json.loads(pm_row["payload"])
                    except Exception:
                        payload = None

            if pending_match_id is not None:
                cursor.execute(
                    """
                    SELECT team_bet_on
                    FROM bets
                    WHERE guild_id = ? AND discord_id = ? AND match_id IS NULL AND pending_match_id = ?
                    ORDER BY bet_time ASC
                    """,
                    (normalized_guild, discord_id, pending_match_id),
                )
            else:
                cursor.execute(
                    """
                    SELECT team_bet_on
                    FROM bets
                    WHERE guild_id = ? AND discord_id = ? AND match_id IS NULL AND bet_time >= ?
                    ORDER BY bet_time ASC
                    """,
                    (normalized_guild, discord_id, int(since_ts)),
                )
            existing_bets = cursor.fetchall()
            if existing_bets:
                existing_team = existing_bets[0]["team_bet_on"]
                if existing_team != team:
                    allow_dual = payload is not None and _allows_shuffle_spectator_dual_team(
                        payload, discord_id=discord_id
                    )
                    if not allow_dual:
                        _raise_one_side_bet_required(existing_team, payload)

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

            self._set_economy_ledger_context(
                cursor,
                source="bet",
                related_type="pending_match" if pending_match_id is not None else "bet_window",
                related_id=pending_match_id if pending_match_id is not None else since_ts,
                reason="bet stake placed",
                metadata={
                    "team": team,
                    "amount": amount,
                    "effective_bet": effective_bet,
                    "leverage": leverage,
                    "is_blind": is_blind,
                    "allow_negative": allow_negative,
                },
            )
            try:
                cursor.execute(
                    """
                    UPDATE players
                    SET jopacoin_balance = COALESCE(jopacoin_balance, 0) - ?, updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    (effective_bet, discord_id, normalized_guild),
                )
            finally:
                self._clear_economy_ledger_context(cursor)

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
        - shuffle spectators may bet both teams; draft/participants one team per match
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
                        except Exception as e:
                            logger.warning("Failed to parse pending match payload: %s", e)
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

            # Reject opposite-team bets unless shuffle spectator dual-team is allowed
            cursor.execute(
                """
                SELECT team_bet_on
                FROM bets
                WHERE guild_id = ? AND discord_id = ? AND match_id IS NULL AND pending_match_id = ?
                ORDER BY bet_time ASC
                """,
                (normalized_guild, discord_id, actual_pending_match_id),
            )
            existing_bets = cursor.fetchall()
            if existing_bets:
                existing_team = existing_bets[0]["team_bet_on"]
                if existing_team != team:
                    allow_dual = _allows_shuffle_spectator_dual_team(
                        payload, discord_id=discord_id, radiant_ids=radiant_ids, dire_ids=dire_ids
                    )
                    if not allow_dual:
                        _raise_one_side_bet_required(existing_team, payload)

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

            self._set_economy_ledger_context(
                cursor,
                source="bet",
                related_type="pending_match",
                related_id=actual_pending_match_id,
                reason="pending match bet stake placed",
                metadata={
                    "team": team,
                    "amount": amount,
                    "effective_bet": effective_bet,
                    "leverage": leverage,
                    "is_blind": is_blind,
                },
            )
            try:
                cursor.execute(
                    """
                    UPDATE players
                    SET jopacoin_balance = COALESCE(jopacoin_balance, 0) - ?, updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    (effective_bet, discord_id, normalized_guild),
                )
            finally:
                self._clear_economy_ledger_context(cursor)
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
        bankruptcy_penalty_rate: float | None = None,
        bet_seed_radiant: int = 0,
        bet_seed_dire: int = 0,
        bet_seed_bonus: int = 0,
    ) -> dict[str, list[dict]]:
        """
        Atomically settle bets for the current match window:
        - credit winners in players.jopacoin_balance (based on effective bet with leverage)
        - optionally dock each penalized winner's bankruptcy penalty in the SAME txn
        - tag all pending bets with match_id

        Args:
            betting_mode: "pool" for parimutuel betting, "house" for 1:1 payouts
            pending_match_id: If provided, settle bets for this specific pending match.
                             Also includes legacy bets (pending_match_id IS NULL) with matching since_ts.
            bankruptcy_penalty_rate: When set, winners still under a bankruptcy
                             penalty keep only this fraction of their profit; the
                             withheld share is netted out of the credit inside the
                             settlement txn (no follow-up debit / crash window).
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        distributions: dict[str, list[dict]] = {"winners": [], "losers": []}

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            seed_fields = self._consume_pending_match_seed_in_txn(
                cursor,
                normalized_guild,
                pending_match_id,
                bet_seed_reserved=0,
                bet_seed_radiant=bet_seed_radiant,
                bet_seed_dire=bet_seed_dire,
                bet_seed_bonus=bet_seed_bonus,
            )
            bet_seed_radiant = seed_fields["bet_seed_radiant"]
            bet_seed_dire = seed_fields["bet_seed_dire"]
            bet_seed_bonus = seed_fields["bet_seed_bonus"]

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
                seed_return = int(bet_seed_radiant) + int(bet_seed_dire) + int(bet_seed_bonus)
                if seed_return > 0:
                    self._credit_nonprofit_fund_in_txn(
                        cursor,
                        normalized_guild,
                        seed_return,
                        source="dota_bet_seed_return",
                        related_id=pending_match_id if pending_match_id is not None else since_ts,
                        reason="unused betting seed returned",
                    )
                    distributions["seed_returned"] = [{"amount": seed_return}]
                return distributions

            if betting_mode == "pool":
                distributions, balance_deltas, payout_updates = self._calculate_pool_payouts(
                    rows,
                    winning_team,
                    bet_seed_radiant=bet_seed_radiant,
                    bet_seed_dire=bet_seed_dire,
                )
            else:
                distributions, balance_deltas, payout_updates = self._calculate_house_payouts(
                    rows,
                    winning_team,
                    house_payout_multiplier,
                    bet_seed_bonus=bet_seed_bonus,
                )

            seed_return = sum(item["amount"] for item in distributions.get("seed_returned", []))
            if seed_return > 0:
                self._credit_nonprofit_fund_in_txn(
                    cursor,
                    normalized_guild,
                    seed_return,
                    source="dota_bet_seed_return",
                    related_id=pending_match_id if pending_match_id is not None else since_ts,
                    reason="retained betting seed returned",
                )

            if bankruptcy_penalty_rate is not None:
                penalties = self._apply_bankruptcy_penalty_in_txn(
                    cursor, distributions, balance_deltas,
                    normalized_guild, bankruptcy_penalty_rate,
                )
                if penalties:
                    distributions["bankruptcy_penalties"] = penalties

            if balance_deltas:
                self._set_economy_ledger_context(
                    cursor,
                    source="bet_settlement",
                    related_type="pending_match" if pending_match_id is not None else "bet_window",
                    related_id=pending_match_id if pending_match_id is not None else since_ts,
                    reason="bet settlement payout",
                    metadata={
                        "winning_team": winning_team,
                        "betting_mode": betting_mode,
                        "payout_count": len(balance_deltas),
                    },
                )
                try:
                    cursor.executemany(
                        """
                        UPDATE players
                        SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?, updated_at = CURRENT_TIMESTAMP
                        WHERE discord_id = ? AND guild_id = ?
                        """,
                        [
                            (delta, discord_id, normalized_guild)
                            for discord_id, delta in balance_deltas.items()
                        ],
                    )
                finally:
                    self._clear_economy_ledger_context(cursor)

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

    def _consume_pending_match_seed_in_txn(
        self,
        cursor,
        normalized_guild: int,
        pending_match_id: int | None,
        *,
        bet_seed_reserved: int = 0,
        bet_seed_radiant: int = 0,
        bet_seed_dire: int = 0,
        bet_seed_bonus: int = 0,
    ) -> dict[str, int]:
        """Read and zero pending-match seed fields inside the active transaction."""
        fallback = {
            "bet_seed_reserved": int(bet_seed_reserved or 0),
            "bet_seed_radiant": int(bet_seed_radiant or 0),
            "bet_seed_dire": int(bet_seed_dire or 0),
            "bet_seed_bonus": int(bet_seed_bonus or 0),
        }
        if pending_match_id is None:
            return fallback

        cursor.execute(
            """
            SELECT payload
            FROM pending_matches
            WHERE pending_match_id = ? AND guild_id = ?
            """,
            (pending_match_id, normalized_guild),
        )
        row = cursor.fetchone()
        if row is None:
            return dict.fromkeys(self._PENDING_SEED_FIELDS, 0)

        try:
            payload = json.loads(row["payload"] or "{}")
        except (TypeError, json.JSONDecodeError):
            payload = {}

        consumed = {
            key: int(payload.get(key, 0) or 0)
            for key in self._PENDING_SEED_FIELDS
        }
        if any(consumed.values()):
            for key in self._PENDING_SEED_FIELDS:
                payload[key] = 0
            cursor.execute(
                """
                UPDATE pending_matches
                SET payload = ?, updated_at = CURRENT_TIMESTAMP
                WHERE pending_match_id = ? AND guild_id = ?
                """,
                (json.dumps(payload), pending_match_id, normalized_guild),
            )
        return consumed

    def _credit_nonprofit_fund_in_txn(
        self,
        cursor,
        normalized_guild: int,
        amount: int,
        *,
        source: str,
        related_id: str | int | None,
        reason: str,
    ) -> None:
        """Credit the nonprofit fund inside an existing transaction."""
        if amount <= 0:
            return
        self._set_economy_ledger_context(
            cursor,
            source=source,
            related_type="pending_match",
            related_id=related_id,
            reason=reason,
            metadata={"amount": amount},
        )
        try:
            cursor.execute(
                """
                INSERT INTO nonprofit_fund (guild_id, total_collected, updated_at)
                VALUES (?, ?, CURRENT_TIMESTAMP)
                ON CONFLICT(guild_id) DO UPDATE SET
                    total_collected = total_collected + excluded.total_collected,
                    updated_at = CURRENT_TIMESTAMP
                """,
                (normalized_guild, amount),
            )
        finally:
            self._clear_economy_ledger_context(cursor)

    def _apply_bankruptcy_penalty_in_txn(
        self, cursor, distributions: dict, balance_deltas: dict[int, int],
        normalized_guild: int, penalty_rate: float,
    ) -> dict[int, int]:
        """Net each penalized winner's bankruptcy penalty out of their credit,
        inside the settlement txn.

        Mirrors ``mafia_repository.finalize_day_resolution``: profit basis is
        gross payout minus total at-risk stake (effective bet), aggregated per
        user; the withheld share is ``floor(profit * (1 - keep_rate))`` and only
        applies to winners still under penalty (``penalty_games_remaining > 0``).
        Folding it here removes the post-settlement debit's crash window. The
        bet ``payout`` column stays gross; only the balance credit is netted.
        """
        agg: dict[int, dict[str, int]] = {}
        for w in distributions.get("winners", []):
            a = agg.setdefault(w["discord_id"], {"payout": 0, "stake": 0})
            a["payout"] += int(w.get("payout", 0))
            a["stake"] += int(w.get("effective_bet", w.get("amount", 0)))

        penalties: dict[int, int] = {}
        for discord_id, a in agg.items():
            profit = a["payout"] - a["stake"]
            if profit <= 0:
                continue
            cursor.execute(
                "SELECT COALESCE(penalty_games_remaining, 0) AS pg "
                "FROM bankruptcy_state WHERE discord_id = ? AND guild_id = ?",
                (discord_id, normalized_guild),
            )
            row = cursor.fetchone()
            if row is None or int(row["pg"]) <= 0:
                continue
            penalty = int(profit * (1 - penalty_rate))
            if penalty <= 0:
                continue
            balance_deltas[discord_id] = balance_deltas.get(discord_id, 0) - penalty
            penalties[discord_id] = penalty
        return penalties

    def _calculate_house_payouts(
        self,
        rows: list,
        winning_team: str,
        house_payout_multiplier: float,
        *,
        bet_seed_bonus: int = 0,
    ) -> tuple:
        """Calculate house mode payouts (1:1) with leverage support."""
        distributions: dict[str, list[dict]] = {"winners": [], "losers": []}
        balance_deltas: dict[int, int] = {}
        payout_updates: list[tuple[int, int]] = []  # (payout, bet_id)
        winning_entries: list[dict] = []
        effective_by_user: dict[int, int] = {}

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
            entry["payout"] = payout
            winning_entries.append(entry)
            effective_by_user[bet["discord_id"]] = (
                effective_by_user.get(bet["discord_id"], 0) + effective_bet
            )

        total_winning_effective = sum(effective_by_user.values())
        if total_winning_effective <= 0:
            if bet_seed_bonus > 0:
                distributions["seed_returned"] = [{"amount": int(bet_seed_bonus)}]
            return distributions, balance_deltas, payout_updates

        bonus_by_user: dict[int, int] = {}
        if bet_seed_bonus > 0:
            allocated = 0
            users = list(effective_by_user.items())
            for index, (discord_id, effective_total) in enumerate(users):
                if index == len(users) - 1:
                    bonus = int(bet_seed_bonus) - allocated
                else:
                    bonus = int((effective_total / total_winning_effective) * int(bet_seed_bonus))
                    allocated += bonus
                bonus_by_user[discord_id] = bonus

        entries_by_user: dict[int, list[dict]] = {}
        for entry in winning_entries:
            entries_by_user.setdefault(entry["discord_id"], []).append(entry)

        for discord_id, entries in entries_by_user.items():
            user_bonus = bonus_by_user.get(discord_id, 0)
            user_effective = sum(e["effective_bet"] for e in entries)
            allocated_bonus = 0
            for index, entry in enumerate(entries):
                if user_bonus and index == len(entries) - 1:
                    entry_bonus = user_bonus - allocated_bonus
                elif user_bonus:
                    entry_bonus = int((entry["effective_bet"] / user_effective) * user_bonus)
                    allocated_bonus += entry_bonus
                else:
                    entry_bonus = 0
                entry["payout"] += entry_bonus
                balance_deltas[discord_id] = balance_deltas.get(discord_id, 0) + entry["payout"]
                payout_updates.append((entry["payout"], entry["bet_id"]))
                distributions["winners"].append(entry)

        return distributions, balance_deltas, payout_updates

    def _calculate_pool_payouts(
        self,
        rows: list,
        winning_team: str,
        *,
        bet_seed_radiant: int = 0,
        bet_seed_dire: int = 0,
    ) -> tuple:
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
        winning_seed = int(bet_seed_radiant) if winning_team == "radiant" else int(bet_seed_dire)
        losing_seed = int(bet_seed_dire) if winning_team == "radiant" else int(bet_seed_radiant)

        # Edge case: no real bets on winning side - losing bets burn and seed returns.
        if winner_pool == 0:
            for row in rows:
                bet = dict(row)
                leverage = bet.get("leverage", 1) or 1
                effective_bet = bet["amount"] * leverage
                distributions["losers"].append(
                    {
                        "bet_id": bet["bet_id"],
                        "discord_id": bet["discord_id"],
                        "amount": bet["amount"],
                        "leverage": leverage,
                        "effective_bet": effective_bet,
                        "team": bet["team_bet_on"],
                    }
                )
            seed_return = int(bet_seed_radiant) + int(bet_seed_dire)
            if seed_return > 0:
                distributions["seed_returned"] = [{"amount": seed_return}]
            return distributions, balance_deltas, payout_updates

        total_pool += losing_seed
        multiplier = total_pool / winner_pool
        if winning_seed > 0:
            distributions["seed_returned"] = [{"amount": winning_seed}]

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
                    bet_payout = int((entry["raw_payout"] / user_raw_total) * user_final_payout) if user_raw_total else 0
                    allocated += bet_payout

                entry["payout"] = bet_payout
                payout_updates.append((bet_payout, entry["bet_id"]))
                del entry["raw_payout"]  # Clean up internal field
                distributions["winners"].append(entry)

        return distributions, balance_deltas, payout_updates

    def refund_pending_bets_atomic(
        self,
        *,
        guild_id: int | None,
        since_ts: int,
        pending_match_id: int | None = None,
        bet_seed_reserved: int = 0,
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
            seed_fields = self._consume_pending_match_seed_in_txn(
                cursor,
                normalized_guild,
                pending_match_id,
                bet_seed_reserved=bet_seed_reserved,
            )
            bet_seed_reserved = seed_fields["bet_seed_reserved"]

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
                if bet_seed_reserved > 0:
                    self._credit_nonprofit_fund_in_txn(
                        cursor,
                        normalized_guild,
                        int(bet_seed_reserved),
                        source="dota_bet_seed_return",
                        related_id=pending_match_id if pending_match_id is not None else since_ts,
                        reason="aborted betting seed returned",
                    )
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

            self._set_economy_ledger_context(
                cursor,
                source="bet_refund",
                related_type="pending_match" if pending_match_id is not None else "bet_window",
                related_id=pending_match_id if pending_match_id is not None else since_ts,
                reason="cancelled bet stake refund",
                metadata={"refund_count": len(refund_deltas), "bet_count": len(bet_ids)},
            )
            try:
                cursor.executemany(
                    """
                    UPDATE players
                    SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?, updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    [
                        (delta, discord_id, normalized_guild)
                        for discord_id, delta in refund_deltas.items()
                    ],
                )
            finally:
                self._clear_economy_ledger_context(cursor)

            # Delete by bet_id for precise targeting
            if bet_ids:
                placeholders = ",".join("?" * len(bet_ids))
                cursor.execute(
                    f"DELETE FROM bets WHERE bet_id IN ({placeholders})",
                    bet_ids,
                )
            if bet_seed_reserved > 0:
                self._credit_nonprofit_fund_in_txn(
                    cursor,
                    normalized_guild,
                    int(bet_seed_reserved),
                    source="dota_bet_seed_return",
                    related_id=pending_match_id if pending_match_id is not None else since_ts,
                    reason="aborted betting seed returned",
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
                    # Use the stored payout when present. Only fall back to the
                    # 2x house-mode estimate when the column is genuinely NULL
                    # (unsettled/unknown) — a real stored 0 (a win that paid
                    # nothing) must not be treated as missing.
                    payout = bet["payout"] if bet["payout"] is not None else effective_bet * 2
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

    def get_bulk_unique_matches_bet_on(
        self, guild_id: int | None, discord_ids: list[int]
    ) -> dict[int, int]:
        """Per-player count of distinct matches the player has settled bets on.

        Used for the leaderboard's degen frequency calculation so it lines up
        with the per-player /gamble stats view, which keys off unique matches
        rather than raw bet count.
        """
        if not discord_ids:
            return {}
        normalized_guild = self.normalize_guild_id(guild_id)
        placeholders = ",".join("?" * len(discord_ids))
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"""
                SELECT discord_id, COUNT(DISTINCT match_id) as unique_matches
                FROM bets
                WHERE guild_id = ? AND match_id IS NOT NULL AND discord_id IN ({placeholders})
                GROUP BY discord_id
                """,
                (normalized_guild, *discord_ids),
            )
            return {row["discord_id"]: row["unique_matches"] for row in cursor.fetchall()}

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

        result = dict.fromkeys(discord_ids, 0)
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

    def _compute_new_bet_payouts(
        self,
        match_id: int,
        new_winners: list[dict],
        pool_mode: bool = True,
    ) -> tuple[dict[int, int], list[tuple[int, int]]]:
        """
        Pure computation of post-correction payouts (no DB writes).

        For the new winners: they get their stakes back + winnings.
        For pool mode: recalculate based on pool proportions.
        For house mode: double the effective bet.

        Args:
            match_id: The match being corrected
            new_winners: List of bet dicts for bets that now win
            pool_mode: True for parimutuel, False for house mode

        Returns:
            Tuple of (balance_deltas mapping discord_id -> amount to add,
            payout_updates list of (payout, bet_id) for the bets table).
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
                return balance_deltas, payout_updates

            # Group winners by user
            winners_by_user: dict[int, list[dict]] = {}
            for bet in new_winners:
                discord_id = bet["discord_id"]
                if discord_id not in winners_by_user:
                    winners_by_user[discord_id] = []
                winners_by_user[discord_id].append(bet)

            # Calculate payouts per user with single ceiling
            for discord_id, bets in winners_by_user.items():
                raw_total = sum((b["effective_bet"] / winner_pool) * total_pool for b in bets)
                user_payout = math.ceil(raw_total)
                balance_deltas[discord_id] = user_payout

                # Distribute across individual bets
                bet_sum = sum(b["effective_bet"] for b in bets)
                allocated = 0
                for i, bet in enumerate(bets):
                    if i == len(bets) - 1:
                        bet_payout = user_payout - allocated
                    else:
                        bet_payout = int((bet["effective_bet"] / bet_sum) * user_payout) if bet_sum else 0
                        allocated += bet_payout
                    payout_updates.append((bet_payout, bet["bet_id"]))
        else:
            # House mode: stake + (stake * multiplier), mirroring _calculate_house_payouts
            from config import HOUSE_PAYOUT_MULTIPLIER
            for bet in new_winners:
                payout = int(bet["effective_bet"] * (1 + HOUSE_PAYOUT_MULTIPLIER))
                discord_id = bet["discord_id"]
                balance_deltas[discord_id] = balance_deltas.get(discord_id, 0) + payout
                payout_updates.append((payout, bet["bet_id"]))

        return balance_deltas, payout_updates

    def settle_bet_correction_atomic(
        self,
        match_id: int,
        old_winners: list[dict],
        new_winners: list[dict],
        guild_id: int | None,
        pool_mode: bool = True,
    ) -> dict[int, int]:
        """
        Apply a full bet-payout correction in ONE atomic transaction.

        Folds three steps that must commit-or-rollback together:
        1. Reverse old winners (subtract their stale payouts from balances).
        2. Pay new winners (credit recalculated payouts to balances).
        3. Rewrite the bets.payout column (set new winners, NULL everyone else).

        Previously the bets-table rewrite committed in its own transaction
        while the balance deltas were applied separately afterward; if that
        second step failed, the bets table reflected new payouts while balances
        were never credited, permanently stranding the new winners' payout.

        Args:
            match_id: The match being corrected
            old_winners: Bet dicts that previously won (now losers)
            new_winners: Bet dicts that now win
            guild_id: Guild scope for the player balance updates
            pool_mode: True for parimutuel, False for house mode

        Returns:
            Dict mapping discord_id -> net balance delta applied (reversal + new).
        """
        gid = self.normalize_guild_id(guild_id)

        # Reversal is pure compute: subtract each old winner's stale payout.
        reversal_deltas = self.reverse_bet_payouts_for_correction(match_id, old_winners)

        # New payouts are pure compute too; bet-row payout updates come along.
        new_deltas, payout_updates = self._compute_new_bet_payouts(
            match_id, new_winners, pool_mode=pool_mode,
        )

        # Merge reversal + new into the net per-player balance delta.
        combined_deltas: dict[int, int] = {}
        for pid, delta in reversal_deltas.items():
            combined_deltas[pid] = combined_deltas.get(pid, 0) + delta
        for pid, delta in new_deltas.items():
            combined_deltas[pid] = combined_deltas.get(pid, 0) + delta

        new_winner_bet_ids = {bet["bet_id"] for bet in new_winners}
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            # 1+2. Apply net balance deltas to players in the SAME transaction
            # as the bets rewrite. Mirrors PlayerRepository.add_balance_many,
            # including lowest_balance_ever tracking for negative deltas.
            if combined_deltas:
                cursor.executemany(
                    """
                    UPDATE players
                    SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?,
                        updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    [(delta, pid, gid) for pid, delta in combined_deltas.items()],
                )
                negative_ids = [pid for pid, delta in combined_deltas.items() if delta < 0]
                if negative_ids:
                    placeholders = ",".join("?" * len(negative_ids))
                    cursor.execute(
                        f"""
                        UPDATE players
                        SET lowest_balance_ever = jopacoin_balance
                        WHERE discord_id IN ({placeholders}) AND guild_id = ?
                        AND (lowest_balance_ever IS NULL OR jopacoin_balance < lowest_balance_ever)
                        """,
                        negative_ids + [gid],
                    )

            # 3. Update payout column for new winners.
            if payout_updates:
                cursor.executemany(
                    "UPDATE bets SET payout = ? WHERE bet_id = ?",
                    payout_updates,
                )

            # Clear payout for every bet of this match that is NOT a new
            # winner. This explicitly nulls bets that won before the
            # correction but are now losers — they still hold a stale,
            # non-null payout at this point, so a "payout IS NOT NULL"
            # subquery would wrongly keep them.
            if new_winner_bet_ids:
                placeholders = ",".join("?" for _ in new_winner_bet_ids)
                cursor.execute(
                    f"""
                    UPDATE bets
                    SET payout = NULL
                    WHERE match_id = ?
                      AND bet_id NOT IN ({placeholders})
                    """,
                    (match_id, *new_winner_bet_ids),
                )
            else:
                cursor.execute(
                    "UPDATE bets SET payout = NULL WHERE match_id = ?",
                    (match_id,),
                )

        return combined_deltas

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

    def has_recent_bet(
        self, discord_id: int, guild_id: int | None, since_ts: int,
    ) -> bool:
        """Return True if the player placed any bet at or after ``since_ts``."""
        gid = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT 1 FROM bets
                 WHERE discord_id = ? AND guild_id = ? AND bet_time >= ?
                 LIMIT 1
                """,
                (discord_id, gid, int(since_ts)),
            )
            return cursor.fetchone() is not None
