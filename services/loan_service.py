"""
Service for handling player loans.

Loans allow players to borrow jopacoins with a flat fee.
Fees are collected into a nonprofit fund for gambling addiction.
"""

import time
from dataclasses import dataclass

from config import LOAN_COOLDOWN_SECONDS, LOAN_FEE_RATE, LOAN_MAX_AMOUNT, MAX_DEBT
from repositories.base_repository import BaseRepository
from repositories.player_repository import PlayerRepository


@dataclass
class LoanState:
    """Current loan state for a player."""

    discord_id: int
    last_loan_at: int | None  # Unix timestamp
    total_loans_taken: int
    total_fees_paid: int
    is_on_cooldown: bool
    cooldown_ends_at: int | None  # Unix timestamp


class LoanRepository(BaseRepository):
    """Data access for loan state and nonprofit fund."""

    def get_state(self, discord_id: int) -> dict | None:
        """Get loan state for a player."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT discord_id, last_loan_at, total_loans_taken, total_fees_paid
                FROM loan_state
                WHERE discord_id = ?
                """,
                (discord_id,),
            )
            row = cursor.fetchone()
            if not row:
                return None
            return {
                "discord_id": row["discord_id"],
                "last_loan_at": row["last_loan_at"],
                "total_loans_taken": row["total_loans_taken"],
                "total_fees_paid": row["total_fees_paid"],
            }

    def upsert_state(
        self,
        discord_id: int,
        last_loan_at: int,
        total_loans_taken: int,
        total_fees_paid: int,
    ) -> None:
        """Create or update loan state."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO loan_state (discord_id, last_loan_at, total_loans_taken, total_fees_paid, updated_at)
                VALUES (?, ?, ?, ?, CURRENT_TIMESTAMP)
                ON CONFLICT(discord_id) DO UPDATE SET
                    last_loan_at = excluded.last_loan_at,
                    total_loans_taken = excluded.total_loans_taken,
                    total_fees_paid = excluded.total_fees_paid,
                    updated_at = CURRENT_TIMESTAMP
                """,
                (discord_id, last_loan_at, total_loans_taken, total_fees_paid),
            )

    def get_nonprofit_fund(self, guild_id: int | None) -> int:
        """Get the total collected in the nonprofit fund for a guild."""
        normalized_guild_id = guild_id if guild_id is not None else 0
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT total_collected FROM nonprofit_fund WHERE guild_id = ?",
                (normalized_guild_id,),
            )
            row = cursor.fetchone()
            return row["total_collected"] if row else 0

    def add_to_nonprofit_fund(self, guild_id: int | None, amount: int) -> int:
        """
        Add amount to the nonprofit fund.

        Returns the new total.
        """
        normalized_guild_id = guild_id if guild_id is not None else 0
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO nonprofit_fund (guild_id, total_collected, updated_at)
                VALUES (?, ?, CURRENT_TIMESTAMP)
                ON CONFLICT(guild_id) DO UPDATE SET
                    total_collected = total_collected + excluded.total_collected,
                    updated_at = CURRENT_TIMESTAMP
                """,
                (normalized_guild_id, amount),
            )
            cursor.execute(
                "SELECT total_collected FROM nonprofit_fund WHERE guild_id = ?",
                (normalized_guild_id,),
            )
            row = cursor.fetchone()
            return row["total_collected"] if row else amount


class LoanService:
    """
    Handles loan applications and fee collection.

    When a player takes a loan:
    1. They receive the full loan amount
    2. They owe the loan amount plus a flat fee (e.g., 20%)
    3. The fee goes to the nonprofit fund for gambling addiction
    """

    def __init__(
        self,
        loan_repo: LoanRepository,
        player_repo: PlayerRepository,
        cooldown_seconds: int | None = None,
        max_amount: int | None = None,
        fee_rate: float | None = None,
        max_debt: int | None = None,
    ):
        self.loan_repo = loan_repo
        self.player_repo = player_repo
        self.cooldown_seconds = (
            cooldown_seconds if cooldown_seconds is not None else LOAN_COOLDOWN_SECONDS
        )
        self.max_amount = max_amount if max_amount is not None else LOAN_MAX_AMOUNT
        self.fee_rate = fee_rate if fee_rate is not None else LOAN_FEE_RATE
        self.max_debt = max_debt if max_debt is not None else MAX_DEBT

    def get_state(self, discord_id: int) -> LoanState:
        """Get the current loan state for a player."""
        state = self.loan_repo.get_state(discord_id)
        now = int(time.time())

        if not state:
            return LoanState(
                discord_id=discord_id,
                last_loan_at=None,
                total_loans_taken=0,
                total_fees_paid=0,
                is_on_cooldown=False,
                cooldown_ends_at=None,
            )

        last_loan = state["last_loan_at"]
        cooldown_ends = last_loan + self.cooldown_seconds if last_loan else None
        is_on_cooldown = cooldown_ends is not None and now < cooldown_ends

        return LoanState(
            discord_id=discord_id,
            last_loan_at=last_loan,
            total_loans_taken=state["total_loans_taken"],
            total_fees_paid=state["total_fees_paid"],
            is_on_cooldown=is_on_cooldown,
            cooldown_ends_at=cooldown_ends if is_on_cooldown else None,
        )

    def can_take_loan(self, discord_id: int, amount: int) -> dict:
        """
        Check if a player can take a loan.

        Returns:
            Dict with 'allowed' (bool) and 'reason' (str if not allowed)
        """
        state = self.get_state(discord_id)
        balance = self.player_repo.get_balance(discord_id)

        if state.is_on_cooldown:
            return {
                "allowed": False,
                "reason": "on_cooldown",
                "cooldown_ends_at": state.cooldown_ends_at,
            }

        if amount <= 0:
            return {
                "allowed": False,
                "reason": "invalid_amount",
            }

        if amount > self.max_amount:
            return {
                "allowed": False,
                "reason": "exceeds_max",
                "max_amount": self.max_amount,
            }

        # Calculate fee and check if total debt would exceed limit
        fee = int(amount * self.fee_rate)
        total_owed = amount + fee
        new_balance = balance + amount - total_owed  # net change is -fee

        if new_balance < -self.max_debt:
            return {
                "allowed": False,
                "reason": "exceeds_debt_limit",
                "current_balance": balance,
                "max_debt": self.max_debt,
            }

        return {
            "allowed": True,
            "amount": amount,
            "fee": fee,
            "total_owed": total_owed,
            "new_balance": new_balance,
        }

    def take_loan(self, discord_id: int, amount: int, guild_id: int | None = None) -> dict:
        """
        Take out a loan.

        The player receives the full amount, then owes amount + fee.
        Fee goes to the nonprofit fund.

        Returns:
            Dict with 'success', 'amount', 'fee', 'new_balance', 'nonprofit_total'
        """
        check = self.can_take_loan(discord_id, amount)
        if not check["allowed"]:
            return {"success": False, **check}

        fee = check["fee"]
        total_owed = check["total_owed"]
        now = int(time.time())

        # Get current state for updating totals
        state = self.get_state(discord_id)

        # Credit the loan amount, then debit the total owed
        # Net effect: balance decreases by fee amount
        self.player_repo.add_balance(discord_id, amount)  # Receive loan
        self.player_repo.add_balance(discord_id, -total_owed)  # Owe amount + fee

        # Add fee to nonprofit fund
        nonprofit_total = self.loan_repo.add_to_nonprofit_fund(guild_id, fee)

        # Update loan state
        self.loan_repo.upsert_state(
            discord_id=discord_id,
            last_loan_at=now,
            total_loans_taken=state.total_loans_taken + 1,
            total_fees_paid=state.total_fees_paid + fee,
        )

        new_balance = self.player_repo.get_balance(discord_id)

        return {
            "success": True,
            "amount": amount,
            "fee": fee,
            "total_owed": total_owed,
            "new_balance": new_balance,
            "nonprofit_total": nonprofit_total,
            "total_loans_taken": state.total_loans_taken + 1,
        }

    def get_nonprofit_fund(self, guild_id: int | None) -> int:
        """Get the total collected in the nonprofit fund."""
        return self.loan_repo.get_nonprofit_fund(guild_id)
