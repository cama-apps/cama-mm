"""
Service for handling player loans.

Loans allow players to borrow jopacoins with a flat fee.
Fees are collected into a nonprofit fund for gambling addiction.
"""

import time
from dataclasses import dataclass

from config import LOAN_COOLDOWN_SECONDS, LOAN_FEE_RATE, LOAN_MAX_AMOUNT, MAX_DEBT
from repositories.loan_repository import LoanRepository
from repositories.player_repository import PlayerRepository
from services.result import Result
from services import error_codes
from services.interfaces import ILoanService


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


@dataclass
class LoanApproval:
    """Details of an approved loan request."""

    amount: int
    fee: int
    total_owed: int
    new_balance: int


@dataclass
class LoanResult:
    """Result of taking a loan."""

    amount: int
    fee: int
    total_owed: int
    new_balance: int
    total_loans_taken: int
    was_negative_loan: bool  # True if taken while already in debt


@dataclass
class RepaymentResult:
    """Result of repaying a loan."""

    principal: int
    fee: int
    total_repaid: int
    balance_before: int
    new_balance: int
    nonprofit_total: int


class LoanService(ILoanService):
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

    def get_state(self, discord_id: int, guild_id: int | None = None) -> LoanState:
        """Get the current loan state for a player."""
        state = self.loan_repo.get_state(discord_id, guild_id)
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

    def get_nonprofit_fund(self, guild_id: int | None) -> int:
        """Get the total collected in the nonprofit fund."""
        return self.loan_repo.get_nonprofit_fund(guild_id)

    def add_to_nonprofit_fund(self, guild_id: int | None, amount: int) -> int:
        """Add amount to the nonprofit fund. Returns the new total."""
        return self.loan_repo.add_to_nonprofit_fund(guild_id, amount)

    def subtract_from_nonprofit_fund(self, guild_id: int | None, amount: int) -> int:
        """
        Subtract amount from the nonprofit fund (admin operation).

        Args:
            guild_id: Guild ID
            amount: Positive amount to subtract

        Returns:
            New fund balance

        Raises:
            ValueError: If insufficient funds
        """
        if amount <= 0:
            raise ValueError("Amount must be positive")
        current = self.get_nonprofit_fund(guild_id)
        if current < amount:
            raise ValueError(f"Insufficient funds. Available: {current}, requested: {amount}")
        # Use negative amount to subtract
        return self.loan_repo.add_to_nonprofit_fund(guild_id, -amount)

    def reset_loan_cooldown(self, discord_id: int, guild_id: int | None) -> None:
        """
        Reset a player's loan cooldown (admin operation).

        Sets last_loan_at to 0 (epoch) so they can take a new loan immediately.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID
        """
        state = self.get_state(discord_id, guild_id)
        self.loan_repo.upsert_state(
            discord_id=discord_id,
            guild_id=guild_id,
            last_loan_at=0,
            total_loans_taken=state.total_loans_taken,
            total_fees_paid=state.total_fees_paid,
            negative_loans_taken=state.negative_loans_taken,
            outstanding_principal=state.outstanding_principal,
            outstanding_fee=state.outstanding_fee,
        )

    # =========================================================================
    # Result-returning methods (new API)
    # These methods return Result types for cleaner error handling.
    # The old dict-returning methods are kept for backward compatibility.
    # =========================================================================

    def validate_loan(self, discord_id: int, amount: int, guild_id: int | None = None) -> Result[LoanApproval]:
        """
        Check if a player can take a loan.

        Returns:
            Result.ok(LoanApproval) if allowed
            Result.fail(error_message, code) if not allowed

        Error codes:
            - LOAN_ALREADY_EXISTS: Player has an outstanding loan
            - COOLDOWN_ACTIVE: Loan cooldown hasn't expired
            - VALIDATION_ERROR: Invalid amount
            - LOAN_AMOUNT_EXCEEDED: Amount exceeds maximum
        """
        state = self.get_state(discord_id, guild_id)
        balance = self.player_repo.get_balance(discord_id, guild_id)

        # Check if player already has an outstanding loan
        if state.has_outstanding_loan:
            return Result.fail(
                f"You have an outstanding loan of {state.outstanding_total} "
                f"(principal: {state.outstanding_principal}, fee: {state.outstanding_fee}). "
                "Repay it by playing in a match first!",
                code=error_codes.LOAN_ALREADY_EXISTS,
            )

        if state.is_on_cooldown:
            remaining = state.cooldown_ends_at - int(time.time())
            hours = remaining // 3600
            minutes = (remaining % 3600) // 60
            return Result.fail(
                f"Loan cooldown active. Try again in {hours}h {minutes}m.",
                code=error_codes.COOLDOWN_ACTIVE,
            )

        if amount <= 0:
            return Result.fail(
                "Loan amount must be positive.",
                code=error_codes.VALIDATION_ERROR,
            )

        if amount > self.max_amount:
            return Result.fail(
                f"Maximum loan amount is {self.max_amount}.",
                code=error_codes.LOAN_AMOUNT_EXCEEDED,
            )

        # Calculate loan details
        fee = int(amount * self.fee_rate)
        total_owed = amount + fee
        new_balance = balance + amount

        return Result.ok(
            LoanApproval(
                amount=amount,
                fee=fee,
                total_owed=total_owed,
                new_balance=new_balance,
            )
        )

    def execute_loan(
        self, discord_id: int, amount: int, guild_id: int | None = None
    ) -> Result[LoanResult]:
        """
        Take out a loan with deferred repayment.

        The player receives the full loan amount immediately.
        Repayment (principal + fee) happens when they play in a recorded match.

        This method uses atomic validation + execution to prevent race conditions
        where concurrent requests could both pass validation before either records
        the loan.

        Returns:
            Result.ok(LoanResult) on success
            Result.fail(error_message, code) on failure
        """
        # Calculate fee for the atomic operation
        fee = int(amount * self.fee_rate)

        try:
            # Atomic validation + execution in single transaction
            result = self.loan_repo.execute_loan_atomic(
                discord_id=discord_id,
                guild_id=guild_id,
                amount=amount,
                fee=fee,
                cooldown_seconds=self.cooldown_seconds,
                max_amount=self.max_amount,
            )

            return Result.ok(
                LoanResult(
                    amount=result["amount"],
                    fee=result["fee"],
                    total_owed=result["total_owed"],
                    new_balance=result["new_balance"],
                    total_loans_taken=result["total_loans_taken"],
                    was_negative_loan=result["was_negative_loan"],
                )
            )
        except ValueError as e:
            error_msg = str(e)
            # Map error messages to error codes
            if "outstanding loan" in error_msg.lower():
                return Result.fail(error_msg, code=error_codes.LOAN_ALREADY_EXISTS)
            elif "cooldown" in error_msg.lower():
                return Result.fail(error_msg, code=error_codes.COOLDOWN_ACTIVE)
            elif "must be positive" in error_msg.lower():
                return Result.fail(error_msg, code=error_codes.VALIDATION_ERROR)
            elif "maximum loan amount" in error_msg.lower():
                return Result.fail(error_msg, code=error_codes.LOAN_AMOUNT_EXCEEDED)
            elif "not found" in error_msg.lower():
                return Result.fail(error_msg, code=error_codes.PLAYER_NOT_FOUND)
            else:
                return Result.fail(error_msg, code=error_codes.VALIDATION_ERROR)

    def execute_repayment(
        self, discord_id: int, guild_id: int | None = None
    ) -> Result[RepaymentResult]:
        """
        Repay an outstanding loan.

        Called when a player participates in a recorded match.
        Deducts principal + fee from balance, adds fee to nonprofit fund.

        Returns:
            Result.ok(RepaymentResult) on success
            Result.fail(error_message, code) if no outstanding loan
        """
        state = self.get_state(discord_id, guild_id)

        if not state.has_outstanding_loan:
            return Result.fail(
                "No outstanding loan to repay.",
                code=error_codes.NO_OUTSTANDING_LOAN,
            )

        principal = state.outstanding_principal
        fee = state.outstanding_fee
        total_owed = principal + fee

        balance_before = self.player_repo.get_balance(discord_id, guild_id)

        # Deduct the total owed from balance (can push into debt)
        self.player_repo.add_balance(discord_id, guild_id, -total_owed)

        # Add fee to nonprofit fund
        nonprofit_total = self.loan_repo.add_to_nonprofit_fund(guild_id, fee)

        # Update loan state: clear outstanding, add fee to total_fees_paid
        self.loan_repo.upsert_state(
            discord_id=discord_id,
            guild_id=guild_id,
            total_fees_paid=state.total_fees_paid + fee,
            outstanding_principal=0,
            outstanding_fee=0,
        )

        new_balance = self.player_repo.get_balance(discord_id, guild_id)

        return Result.ok(
            RepaymentResult(
                principal=principal,
                fee=fee,
                total_repaid=total_owed,
                balance_before=balance_before,
                new_balance=new_balance,
                nonprofit_total=nonprofit_total,
            )
        )
