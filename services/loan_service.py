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
    negative_loans_taken: int  # Loans taken while already in debt (peak degen)
    is_on_cooldown: bool
    cooldown_ends_at: int | None  # Unix timestamp
    outstanding_principal: int = 0  # Amount borrowed, not yet repaid
    outstanding_fee: int = 0  # Fee owed, not yet paid

    @property
    def has_outstanding_loan(self) -> bool:
        """Check if player has an outstanding loan to repay."""
        return self.outstanding_principal > 0

    @property
    def outstanding_total(self) -> int:
        """Total amount owed (principal + fee)."""
        return self.outstanding_principal + self.outstanding_fee


class LoanRepository(BaseRepository):
    """Data access for loan state and nonprofit fund."""

    def get_state(self, discord_id: int) -> dict | None:
        """Get loan state for a player."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT discord_id, last_loan_at, total_loans_taken, total_fees_paid,
                       COALESCE(negative_loans_taken, 0) as negative_loans_taken,
                       COALESCE(outstanding_principal, 0) as outstanding_principal,
                       COALESCE(outstanding_fee, 0) as outstanding_fee
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
                "negative_loans_taken": row["negative_loans_taken"],
                "outstanding_principal": row["outstanding_principal"],
                "outstanding_fee": row["outstanding_fee"],
            }

    def upsert_state(
        self,
        discord_id: int,
        last_loan_at: int | None = None,
        total_loans_taken: int | None = None,
        total_fees_paid: int | None = None,
        negative_loans_taken: int | None = None,
        outstanding_principal: int | None = None,
        outstanding_fee: int | None = None,
    ) -> None:
        """Create or update loan state."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO loan_state (discord_id, last_loan_at, total_loans_taken, total_fees_paid,
                                        negative_loans_taken, outstanding_principal, outstanding_fee,
                                        updated_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
                ON CONFLICT(discord_id) DO UPDATE SET
                    last_loan_at = COALESCE(excluded.last_loan_at, loan_state.last_loan_at),
                    total_loans_taken = COALESCE(excluded.total_loans_taken, loan_state.total_loans_taken),
                    total_fees_paid = COALESCE(excluded.total_fees_paid, loan_state.total_fees_paid),
                    negative_loans_taken = COALESCE(excluded.negative_loans_taken, loan_state.negative_loans_taken),
                    outstanding_principal = COALESCE(excluded.outstanding_principal, loan_state.outstanding_principal),
                    outstanding_fee = COALESCE(excluded.outstanding_fee, loan_state.outstanding_fee),
                    updated_at = CURRENT_TIMESTAMP
                """,
                (discord_id, last_loan_at, total_loans_taken, total_fees_paid,
                 negative_loans_taken, outstanding_principal, outstanding_fee),
            )

    def clear_outstanding_loan(self, discord_id: int) -> None:
        """Clear the outstanding loan (set principal and fee to 0)."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE loan_state
                SET outstanding_principal = 0, outstanding_fee = 0, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
                """,
                (discord_id,),
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

    def disburse_fund_atomic(
        self,
        guild_id: int | None,
        distributions: list[tuple[int, int]],
    ) -> int:
        """
        Atomically deduct total from nonprofit fund and credit players.

        Args:
            guild_id: Guild ID
            distributions: List of (discord_id, amount) tuples

        Returns:
            Total amount distributed

        Raises:
            ValueError if insufficient funds in nonprofit
        """
        if not distributions:
            return 0

        normalized_guild_id = guild_id if guild_id is not None else 0
        total = sum(amount for _, amount in distributions)

        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("BEGIN IMMEDIATE")

            # Verify sufficient funds
            cursor.execute(
                "SELECT total_collected FROM nonprofit_fund WHERE guild_id = ?",
                (normalized_guild_id,),
            )
            row = cursor.fetchone()
            if not row or row["total_collected"] < total:
                available = row["total_collected"] if row else 0
                raise ValueError(
                    f"Insufficient funds in nonprofit. Available: {available}, needed: {total}"
                )

            # Deduct from nonprofit
            cursor.execute(
                """
                UPDATE nonprofit_fund
                SET total_collected = total_collected - ?, updated_at = CURRENT_TIMESTAMP
                WHERE guild_id = ?
                """,
                (total, normalized_guild_id),
            )

            # Credit players
            cursor.executemany(
                """
                UPDATE players
                SET jopacoin_balance = jopacoin_balance + ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
                """,
                [(amount, discord_id) for discord_id, amount in distributions],
            )

            return total


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
                negative_loans_taken=0,
                is_on_cooldown=False,
                cooldown_ends_at=None,
                outstanding_principal=0,
                outstanding_fee=0,
            )

        last_loan = state["last_loan_at"]
        cooldown_ends = last_loan + self.cooldown_seconds if last_loan else None
        is_on_cooldown = cooldown_ends is not None and now < cooldown_ends

        return LoanState(
            discord_id=discord_id,
            last_loan_at=last_loan,
            total_loans_taken=state["total_loans_taken"],
            total_fees_paid=state["total_fees_paid"],
            negative_loans_taken=state["negative_loans_taken"],
            is_on_cooldown=is_on_cooldown,
            cooldown_ends_at=cooldown_ends if is_on_cooldown else None,
            outstanding_principal=state["outstanding_principal"],
            outstanding_fee=state["outstanding_fee"],
        )

    def can_take_loan(self, discord_id: int, amount: int) -> dict:
        """
        Check if a player can take a loan.

        Returns:
            Dict with 'allowed' (bool) and 'reason' (str if not allowed)
        """
        state = self.get_state(discord_id)
        balance = self.player_repo.get_balance(discord_id)

        # Check if player already has an outstanding loan
        if state.has_outstanding_loan:
            return {
                "allowed": False,
                "reason": "has_outstanding_loan",
                "outstanding_principal": state.outstanding_principal,
                "outstanding_fee": state.outstanding_fee,
                "outstanding_total": state.outstanding_total,
            }

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

        # Calculate fee - with deferred repayment, we only add the amount now
        fee = int(amount * self.fee_rate)
        total_owed = amount + fee
        new_balance = balance + amount  # They receive the full loan amount

        return {
            "allowed": True,
            "amount": amount,
            "fee": fee,
            "total_owed": total_owed,
            "new_balance": new_balance,
        }

    def take_loan(self, discord_id: int, amount: int, guild_id: int | None = None) -> dict:
        """
        Take out a loan with deferred repayment.

        The player receives the full loan amount immediately.
        Repayment (principal + fee) happens when they play in a recorded match.

        Returns:
            Dict with 'success', 'amount', 'fee', 'total_owed', 'new_balance'
            Also 'was_negative_loan' if they took a loan while already in debt (peak degen)
        """
        check = self.can_take_loan(discord_id, amount)
        if not check["allowed"]:
            return {"success": False, **check}

        fee = check["fee"]
        total_owed = check["total_owed"]
        now = int(time.time())

        # Check if taking loan while already in debt (peak degen behavior)
        balance_before = self.player_repo.get_balance(discord_id)
        was_negative_loan = balance_before < 0

        # Get current state for updating totals
        state = self.get_state(discord_id)

        # Credit the full loan amount - NO immediate debit
        # Repayment happens when they play in a recorded match
        self.player_repo.add_balance(discord_id, amount)

        # Update loan state with outstanding amounts (fee goes to nonprofit on repayment)
        new_negative_loans = state.negative_loans_taken + (1 if was_negative_loan else 0)
        self.loan_repo.upsert_state(
            discord_id=discord_id,
            last_loan_at=now,
            total_loans_taken=state.total_loans_taken + 1,
            total_fees_paid=state.total_fees_paid,  # Don't add fee yet - deferred
            negative_loans_taken=new_negative_loans,
            outstanding_principal=amount,
            outstanding_fee=fee,
        )

        new_balance = self.player_repo.get_balance(discord_id)

        return {
            "success": True,
            "amount": amount,
            "fee": fee,
            "total_owed": total_owed,
            "new_balance": new_balance,
            "total_loans_taken": state.total_loans_taken + 1,
            "was_negative_loan": was_negative_loan,
        }

    def repay_loan(self, discord_id: int, guild_id: int | None = None) -> dict:
        """
        Repay an outstanding loan.

        Called when a player participates in a recorded match.
        Deducts principal + fee from balance, adds fee to nonprofit fund.

        Returns:
            Dict with repayment details or None if no outstanding loan.
        """
        state = self.get_state(discord_id)

        if not state.has_outstanding_loan:
            return {"success": False, "reason": "no_outstanding_loan"}

        principal = state.outstanding_principal
        fee = state.outstanding_fee
        total_owed = principal + fee

        balance_before = self.player_repo.get_balance(discord_id)

        # Deduct the total owed from balance (can push into debt)
        self.player_repo.add_balance(discord_id, -total_owed)

        # Add fee to nonprofit fund
        nonprofit_total = self.loan_repo.add_to_nonprofit_fund(guild_id, fee)

        # Update loan state: clear outstanding, add fee to total_fees_paid
        self.loan_repo.upsert_state(
            discord_id=discord_id,
            total_fees_paid=state.total_fees_paid + fee,
            outstanding_principal=0,
            outstanding_fee=0,
        )

        new_balance = self.player_repo.get_balance(discord_id)

        return {
            "success": True,
            "principal": principal,
            "fee": fee,
            "total_repaid": total_owed,
            "balance_before": balance_before,
            "new_balance": new_balance,
            "nonprofit_total": nonprofit_total,
        }

    def get_nonprofit_fund(self, guild_id: int | None) -> int:
        """Get the total collected in the nonprofit fund."""
        return self.loan_repo.get_nonprofit_fund(guild_id)
