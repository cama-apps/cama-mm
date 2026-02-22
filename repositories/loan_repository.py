"""
Repository for loan state and nonprofit fund data access.
"""

import time

from repositories.base_repository import BaseRepository
from repositories.interfaces import ILoanRepository


class LoanRepository(BaseRepository, ILoanRepository):
    """Data access for loan state and nonprofit fund."""

    def get_state(self, discord_id: int, guild_id: int | None = None) -> dict | None:
        """Get loan state for a player."""
        normalized_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT discord_id, guild_id, last_loan_at, total_loans_taken, total_fees_paid,
                       COALESCE(negative_loans_taken, 0) as negative_loans_taken,
                       COALESCE(outstanding_principal, 0) as outstanding_principal,
                       COALESCE(outstanding_fee, 0) as outstanding_fee
                FROM loan_state
                WHERE discord_id = ? AND guild_id = ?
                """,
                (discord_id, normalized_id),
            )
            row = cursor.fetchone()
            if not row:
                return None
            return {
                "discord_id": row["discord_id"],
                "guild_id": row["guild_id"],
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
        guild_id: int | None = None,
        last_loan_at: int | None = None,
        total_loans_taken: int | None = None,
        total_fees_paid: int | None = None,
        negative_loans_taken: int | None = None,
        outstanding_principal: int | None = None,
        outstanding_fee: int | None = None,
    ) -> None:
        """Create or update loan state."""
        normalized_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO loan_state (discord_id, guild_id, last_loan_at, total_loans_taken, total_fees_paid,
                                        negative_loans_taken, outstanding_principal, outstanding_fee,
                                        updated_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
                ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                    last_loan_at = COALESCE(excluded.last_loan_at, loan_state.last_loan_at),
                    total_loans_taken = COALESCE(excluded.total_loans_taken, loan_state.total_loans_taken),
                    total_fees_paid = COALESCE(excluded.total_fees_paid, loan_state.total_fees_paid),
                    negative_loans_taken = COALESCE(excluded.negative_loans_taken, loan_state.negative_loans_taken),
                    outstanding_principal = COALESCE(excluded.outstanding_principal, loan_state.outstanding_principal),
                    outstanding_fee = COALESCE(excluded.outstanding_fee, loan_state.outstanding_fee),
                    updated_at = CURRENT_TIMESTAMP
                """,
                (discord_id, normalized_id, last_loan_at, total_loans_taken, total_fees_paid,
                 negative_loans_taken, outstanding_principal, outstanding_fee),
            )

    def clear_outstanding_loan(self, discord_id: int, guild_id: int | None = None) -> None:
        """Clear the outstanding loan (set principal and fee to 0)."""
        normalized_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE loan_state
                SET outstanding_principal = 0, outstanding_fee = 0, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (discord_id, normalized_id),
            )

    def get_nonprofit_fund(self, guild_id: int | None) -> int:
        """Get the total collected in the nonprofit fund for a guild."""
        normalized_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT total_collected FROM nonprofit_fund WHERE guild_id = ?",
                (normalized_id,),
            )
            row = cursor.fetchone()
            return row["total_collected"] if row else 0

    def add_to_nonprofit_fund(self, guild_id: int | None, amount: int) -> int:
        """
        Add amount to the nonprofit fund.

        Returns the new total.
        """
        normalized_id = self.normalize_guild_id(guild_id)
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
                (normalized_id, amount),
            )
            cursor.execute(
                "SELECT total_collected FROM nonprofit_fund WHERE guild_id = ?",
                (normalized_id,),
            )
            row = cursor.fetchone()
            return row["total_collected"] if row else amount

    def deduct_from_nonprofit_fund(self, guild_id: int | None, amount: int) -> int:
        """
        Atomically deduct amount from the nonprofit fund.

        Validates sufficient funds inside a BEGIN IMMEDIATE transaction.

        Args:
            guild_id: Guild ID
            amount: Positive amount to deduct

        Returns:
            New fund balance after deduction

        Raises:
            ValueError: If amount <= 0 or insufficient funds
        """
        if amount <= 0:
            raise ValueError("Amount must be positive")

        normalized_id = self.normalize_guild_id(guild_id)

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT total_collected FROM nonprofit_fund WHERE guild_id = ?",
                (normalized_id,),
            )
            row = cursor.fetchone()
            current = row["total_collected"] if row else 0

            if current < amount:
                raise ValueError(
                    f"Insufficient nonprofit funds. Available: {current}, requested: {amount}"
                )

            cursor.execute(
                """
                UPDATE nonprofit_fund
                SET total_collected = total_collected - ?, updated_at = CURRENT_TIMESTAMP
                WHERE guild_id = ?
                """,
                (amount, normalized_id),
            )

            cursor.execute(
                "SELECT total_collected FROM nonprofit_fund WHERE guild_id = ?",
                (normalized_id,),
            )
            row = cursor.fetchone()
            return row["total_collected"]

    def execute_loan_atomic(
        self,
        discord_id: int,
        guild_id: int | None,
        amount: int,
        fee: int,
        cooldown_seconds: int,
        max_amount: int,
    ) -> dict:
        """
        Atomically validate and execute a loan.

        Prevents race condition where concurrent requests could both pass
        validation before either records the loan.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID for multi-guild support
            amount: Loan amount requested
            fee: Calculated fee for the loan
            cooldown_seconds: Required cooldown between loans
            max_amount: Maximum allowed loan amount

        Returns:
            Dict with loan details on success

        Raises:
            ValueError with specific message on failure
        """
        now = int(time.time())
        normalized_guild_id = self.normalize_guild_id(guild_id)

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            # Check for existing outstanding loan
            cursor.execute(
                """
                SELECT outstanding_principal, outstanding_fee, last_loan_at,
                       COALESCE(total_loans_taken, 0) as total_loans_taken,
                       COALESCE(total_fees_paid, 0) as total_fees_paid,
                       COALESCE(negative_loans_taken, 0) as negative_loans_taken
                FROM loan_state
                WHERE discord_id = ? AND guild_id = ?
                """,
                (discord_id, normalized_guild_id),
            )
            state_row = cursor.fetchone()

            outstanding_principal = 0
            outstanding_fee = 0
            last_loan_at = None
            total_loans_taken = 0
            total_fees_paid = 0
            negative_loans_taken = 0

            if state_row:
                outstanding_principal = state_row["outstanding_principal"] or 0
                outstanding_fee = state_row["outstanding_fee"] or 0
                last_loan_at = state_row["last_loan_at"]
                total_loans_taken = state_row["total_loans_taken"]
                total_fees_paid = state_row["total_fees_paid"]
                negative_loans_taken = state_row["negative_loans_taken"]

            # Validate: no outstanding loan
            if outstanding_principal > 0:
                total_owed = outstanding_principal + outstanding_fee
                raise ValueError(
                    f"You have an outstanding loan of {total_owed} "
                    f"(principal: {outstanding_principal}, fee: {outstanding_fee}). "
                    "Repay it by playing in a match first!"
                )

            # Validate: cooldown check
            if last_loan_at and (now - last_loan_at) < cooldown_seconds:
                remaining = cooldown_seconds - (now - last_loan_at)
                hours = remaining // 3600
                minutes = (remaining % 3600) // 60
                raise ValueError(f"Loan cooldown active. Try again in {hours}h {minutes}m.")

            # Validate: amount bounds
            if amount <= 0:
                raise ValueError("Loan amount must be positive.")
            if amount > max_amount:
                raise ValueError(f"Maximum loan amount is {max_amount}.")

            # Get current balance to check if this is a "negative loan" (degen behavior)
            cursor.execute(
                "SELECT COALESCE(jopacoin_balance, 0) as balance FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, normalized_guild_id),
            )
            balance_row = cursor.fetchone()
            if not balance_row:
                raise ValueError("Player not found.")

            balance_before = balance_row["balance"]
            was_negative_loan = balance_before < 0

            # Credit the loan amount to player
            cursor.execute(
                """
                UPDATE players
                SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (amount, discord_id, normalized_guild_id),
            )

            # Update/insert loan state
            new_negative_loans = negative_loans_taken + (1 if was_negative_loan else 0)
            cursor.execute(
                """
                INSERT INTO loan_state (discord_id, guild_id, last_loan_at, total_loans_taken, total_fees_paid,
                                        negative_loans_taken, outstanding_principal, outstanding_fee,
                                        updated_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
                ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                    last_loan_at = excluded.last_loan_at,
                    total_loans_taken = excluded.total_loans_taken,
                    negative_loans_taken = excluded.negative_loans_taken,
                    outstanding_principal = excluded.outstanding_principal,
                    outstanding_fee = excluded.outstanding_fee,
                    updated_at = CURRENT_TIMESTAMP
                """,
                (discord_id, normalized_guild_id, now, total_loans_taken + 1, total_fees_paid,
                 new_negative_loans, amount, fee),
            )

            new_balance = balance_before + amount

            return {
                "amount": amount,
                "fee": fee,
                "total_owed": amount + fee,
                "new_balance": new_balance,
                "total_loans_taken": total_loans_taken + 1,
                "was_negative_loan": was_negative_loan,
            }

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

        normalized_id = self.normalize_guild_id(guild_id)
        total = sum(amount for _, amount in distributions)

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            # Verify sufficient funds
            cursor.execute(
                "SELECT total_collected FROM nonprofit_fund WHERE guild_id = ?",
                (normalized_id,),
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
                (total, normalized_id),
            )

            # Credit players
            cursor.executemany(
                """
                UPDATE players
                SET jopacoin_balance = jopacoin_balance + ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                [(amount, discord_id, normalized_id) for discord_id, amount in distributions],
            )

            return total

    def get_negative_loans_bulk(self, discord_ids: list[int], guild_id: int) -> dict[int, int]:
        """Get negative_loans_taken for multiple players in a single query.

        Returns dict of {discord_id: negative_loans_taken}.
        """
        if not discord_ids:
            return {}
        normalized_guild = self.normalize_guild_id(guild_id)
        placeholders = ",".join("?" * len(discord_ids))
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"""
                SELECT discord_id, COALESCE(negative_loans_taken, 0) as negative_loans
                FROM loan_state
                WHERE guild_id = ? AND discord_id IN ({placeholders})
                """,
                [normalized_guild] + list(discord_ids),
            )
            return {row["discord_id"]: row["negative_loans"] for row in cursor.fetchall()}

    def get_total_loans_taken(self, guild_id: int) -> int:
        """Get total number of loans taken server-wide.

        Returns the sum of total_loans_taken across all players in the guild.
        """
        normalized_guild = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT COALESCE(SUM(total_loans_taken), 0) as total
                FROM loan_state
                WHERE guild_id = ?
                """,
                (normalized_guild,),
            )
            row = cursor.fetchone()
            return row["total"] if row else 0
