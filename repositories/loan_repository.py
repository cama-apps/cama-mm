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

    def get_outstanding_borrower_ids(
        self, discord_ids: list[int], guild_id: int | None = None
    ) -> set[int]:
        """Return requested players with outstanding principal in one query."""
        unique_ids = list(dict.fromkeys(discord_ids))
        if not unique_ids:
            return set()

        normalized_id = self.normalize_guild_id(guild_id)
        placeholders = ",".join("?" for _ in unique_ids)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"""
                SELECT discord_id
                FROM loan_state
                WHERE guild_id = ?
                  AND discord_id IN ({placeholders})
                  AND COALESCE(outstanding_principal, 0) > 0
                """,
                [normalized_id, *unique_ids],
            )
            return {int(row["discord_id"]) for row in cursor.fetchall()}

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

    def reset_cooldown(self, discord_id: int, guild_id: int | None = None) -> None:
        """Clear a player's loan cooldown without touching any other state.

        Single conditional UPDATE so a loan landing concurrently cannot be
        overwritten with a stale snapshot (a read-modify-write here could
        clobber outstanding_principal back to 0, forgiving the loan). A player
        with no loan_state row has no cooldown, so the missing-row case is a
        no-op.
        """
        normalized_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE loan_state
                SET last_loan_at = 0, updated_at = CURRENT_TIMESTAMP
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

    def distribute_nonprofit_stipends_atomic(
        self,
        discord_ids: list[int],
        guild_id: int | None,
        max_stipend: int,
    ) -> dict[int, int]:
        """Pay eligible bankrupt players from the nonprofit fund in one txn."""
        unique_ids = list(dict.fromkeys(discord_ids))
        paid = dict.fromkeys(unique_ids, 0)
        if not unique_ids or max_stipend <= 0:
            return paid

        normalized_id = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT total_collected FROM nonprofit_fund WHERE guild_id = ?",
                (normalized_id,),
            )
            row = cursor.fetchone()
            remaining = max(0, int(row["total_collected"])) if row else 0
            if remaining <= 0:
                return paid

            eligible_ids: set[int] = set()
            for offset in range(0, len(unique_ids), 900):
                chunk = unique_ids[offset : offset + 900]
                placeholders = ",".join("?" for _ in chunk)
                rows = cursor.execute(
                    f"""
                    SELECT discord_id
                    FROM players
                    WHERE guild_id = ?
                      AND discord_id IN ({placeholders})
                      AND COALESCE(jopacoin_balance, 0) <= 0
                    """,
                    (normalized_id, *chunk),
                ).fetchall()
                eligible_ids.update(int(player["discord_id"]) for player in rows)

            for discord_id in unique_ids:
                if discord_id not in eligible_ids or remaining <= 0:
                    continue
                amount = min(int(max_stipend), remaining)
                self._set_economy_ledger_context(
                    cursor,
                    source="mana",
                    related_type="bankruptcy_stipend",
                    related_id=discord_id,
                    reason="white mana bankruptcy stipend reserve debit",
                    metadata={"amount": amount, "land": "Plains"},
                )
                try:
                    cursor.execute(
                        """
                        UPDATE nonprofit_fund
                        SET total_collected = total_collected - ?,
                            updated_at = CURRENT_TIMESTAMP
                        WHERE guild_id = ? AND total_collected >= ?
                        """,
                        (amount, normalized_id, amount),
                    )
                    if cursor.rowcount != 1:
                        raise RuntimeError("nonprofit stipend reserve changed unexpectedly")
                finally:
                    self._clear_economy_ledger_context(cursor)

                self._set_economy_ledger_context(
                    cursor,
                    source="mana",
                    related_type="bankruptcy_stipend",
                    reason="white mana bankruptcy stipend",
                    metadata={"amount": amount, "land": "Plains"},
                )
                try:
                    cursor.execute(
                        """
                        UPDATE players
                        SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?,
                            updated_at = CURRENT_TIMESTAMP
                        WHERE discord_id = ? AND guild_id = ?
                          AND COALESCE(jopacoin_balance, 0) <= 0
                        """,
                        (amount, discord_id, normalized_id),
                    )
                    if cursor.rowcount != 1:
                        raise RuntimeError("stipend recipient changed unexpectedly")
                finally:
                    self._clear_economy_ledger_context(cursor)
                paid[discord_id] = amount
                remaining -= amount

        return paid

    def consume_next_match_pot(self, guild_id: int | None) -> int:
        """Atomically claim and clear the reserve allocation queued for a match."""
        normalized_id = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT COALESCE(next_match_pot, 0) AS amount FROM nonprofit_fund WHERE guild_id = ?",
                (normalized_id,),
            )
            row = cursor.fetchone()
            amount = int(row["amount"]) if row else 0
            if amount:
                cursor.execute(
                    "UPDATE nonprofit_fund SET next_match_pot = 0, updated_at = CURRENT_TIMESTAMP WHERE guild_id = ?",
                    (normalized_id,),
                )
            return amount

    def add_to_nonprofit_fund(
        self,
        guild_id: int | None,
        amount: int,
        *,
        source: str | None = None,
        actor_id: int | None = None,
        related_type: str | None = None,
        related_id: str | int | None = None,
        reason: str | None = None,
        metadata: dict | str | None = None,
    ) -> int:
        """
        Add amount to the nonprofit fund.

        Returns the new total. Runs under BEGIN IMMEDIATE so the SELECT after
        the UPSERT reflects exactly this call's credit (no interleaved writes).
        """
        normalized_id = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            has_context = any(
                value is not None
                for value in (
                    source,
                    actor_id,
                    related_type,
                    related_id,
                    reason,
                    metadata,
                )
            )
            if has_context:
                self._set_economy_ledger_context(
                    cursor,
                    source=source,
                    actor_id=actor_id,
                    related_type=related_type,
                    related_id=related_id,
                    reason=reason,
                    metadata=metadata,
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
                    (normalized_id, amount),
                )
            finally:
                if has_context:
                    self._clear_economy_ledger_context(cursor)
            cursor.execute(
                "SELECT total_collected FROM nonprofit_fund WHERE guild_id = ?",
                (normalized_id,),
            )
            row = cursor.fetchone()
            return row["total_collected"] if row else amount

    def transfer_balance_to_nonprofit_atomic(
        self,
        discord_id: int,
        guild_id: int | None,
        amount: int,
        *,
        source: str | None = None,
        related_type: str | None = None,
        related_id: str | int | None = None,
        reason: str | None = None,
        metadata: dict | str | None = None,
    ) -> int:
        """Atomically move ``amount`` JC from a player's balance to the nonprofit
        fund. Debit and credit share one BEGIN IMMEDIATE, so a crash between them
        cannot destroy the coins (debited but never banked) or mint them (banked
        but never debited). Returns the new nonprofit total.
        """
        normalized_id = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            self._set_economy_ledger_context(
                cursor,
                source=source,
                related_type=related_type,
                related_id=related_id,
                reason=reason,
                metadata=metadata,
            )
            try:
                cursor.execute(
                    """
                    UPDATE players
                    SET jopacoin_balance = COALESCE(jopacoin_balance, 0) - ?,
                        updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    (amount, discord_id, normalized_id),
                )
                if cursor.rowcount == 0:
                    # No player row was debited; crediting the fund anyway
                    # would mint coins. Raising rolls the whole txn back.
                    raise ValueError("Player not found.")
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
            finally:
                self._clear_economy_ledger_context(cursor)
            cursor.execute(
                "SELECT total_collected FROM nonprofit_fund WHERE guild_id = ?",
                (normalized_id,),
            )
            row = cursor.fetchone()
            return row["total_collected"] if row else amount

    def deduct_from_nonprofit_fund(
        self,
        guild_id: int | None,
        amount: int,
        *,
        source: str | None = None,
        actor_id: int | None = None,
        related_type: str | None = None,
        related_id: str | int | None = None,
        reason: str | None = None,
        metadata: dict | str | None = None,
    ) -> int:
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

            has_context = any(
                value is not None
                for value in (
                    source,
                    actor_id,
                    related_type,
                    related_id,
                    reason,
                    metadata,
                )
            )
            if has_context:
                self._set_economy_ledger_context(
                    cursor,
                    source=source,
                    actor_id=actor_id,
                    related_type=related_type,
                    related_id=related_id,
                    reason=reason,
                    metadata=metadata,
                )
            try:
                cursor.execute(
                    """
                    UPDATE nonprofit_fund
                    SET total_collected = total_collected - ?, updated_at = CURRENT_TIMESTAMP
                    WHERE guild_id = ?
                    """,
                    (amount, normalized_id),
                )
            finally:
                if has_context:
                    self._clear_economy_ledger_context(cursor)

            cursor.execute(
                "SELECT total_collected FROM nonprofit_fund WHERE guild_id = ?",
                (normalized_id,),
            )
            row = cursor.fetchone()
            return row["total_collected"]

    def deduct_up_to_nonprofit_fund(
        self,
        guild_id: int | None,
        amount: int,
        *,
        source: str | None = None,
        actor_id: int | None = None,
        related_type: str | None = None,
        related_id: str | int | None = None,
        reason: str | None = None,
        metadata: dict | str | None = None,
    ) -> int:
        """Atomically deduct up to amount from the nonprofit fund.

        Returns the amount actually deducted. If the reserve is empty, returns 0.
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
            deduction = min(int(amount), int(current))
            if deduction <= 0:
                return 0

            has_context = any(
                value is not None
                for value in (
                    source,
                    actor_id,
                    related_type,
                    related_id,
                    reason,
                    metadata,
                )
            )
            if has_context:
                self._set_economy_ledger_context(
                    cursor,
                    source=source,
                    actor_id=actor_id,
                    related_type=related_type,
                    related_id=related_id,
                    reason=reason,
                    metadata=metadata,
                )
            try:
                cursor.execute(
                    """
                    UPDATE nonprofit_fund
                    SET total_collected = total_collected - ?, updated_at = CURRENT_TIMESTAMP
                    WHERE guild_id = ?
                    """,
                    (deduction, normalized_id),
                )
            finally:
                if has_context:
                    self._clear_economy_ledger_context(cursor)

            return deduction

    def get_and_deduct_nonprofit_fund_atomic(
        self,
        guild_id: int | None,
        min_amount: int = 0,
        *,
        source: str | None = None,
        actor_id: int | None = None,
        related_type: str | None = None,
        related_id: str | int | None = None,
        reason: str | None = None,
        metadata: dict | str | None = None,
    ) -> int:
        """
        Atomically read the entire nonprofit fund balance and deduct it.

        Prevents race conditions where fund_amount is read, then another
        operation modifies the fund before the deduction completes.

        Args:
            guild_id: Guild ID
            min_amount: Minimum required fund balance (raises ValueError if below)

        Returns:
            The fund balance that was deducted (the entire fund)

        Raises:
            ValueError: If the fund balance is below min_amount
        """
        normalized_id = self.normalize_guild_id(guild_id)

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT total_collected FROM nonprofit_fund WHERE guild_id = ?",
                (normalized_id,),
            )
            row = cursor.fetchone()
            current = row["total_collected"] if row else 0

            if current < min_amount:
                raise ValueError(
                    f"Insufficient nonprofit funds. Available: {current}, required: {min_amount}"
                )

            if current <= 0:
                return 0

            has_context = any(
                value is not None
                for value in (
                    source,
                    actor_id,
                    related_type,
                    related_id,
                    reason,
                    metadata,
                )
            )
            if has_context:
                self._set_economy_ledger_context(
                    cursor,
                    source=source,
                    actor_id=actor_id,
                    related_type=related_type,
                    related_id=related_id,
                    reason=reason,
                    metadata=metadata,
                )
            try:
                cursor.execute(
                    """
                    UPDATE nonprofit_fund
                    SET total_collected = 0, updated_at = CURRENT_TIMESTAMP
                    WHERE guild_id = ?
                    """,
                    (normalized_id,),
                )
            finally:
                if has_context:
                    self._clear_economy_ledger_context(cursor)

            return current

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

            self._set_economy_ledger_context(
                cursor,
                source="loan",
                related_type="loan",
                related_id=discord_id,
                reason="loan principal credit",
                metadata={"amount": amount, "fee": fee},
            )
            try:
                # Credit the loan amount to player
                cursor.execute(
                    """
                    UPDATE players
                    SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?, updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    (amount, discord_id, normalized_guild_id),
                )
            finally:
                self._clear_economy_ledger_context(cursor)

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

    def execute_repayment_atomic(
        self,
        discord_id: int,
        guild_id: int | None,
    ) -> dict:
        """
        Atomically settle an outstanding loan: debit balance, credit nonprofit fund,
        clear the loan state. All four steps share one BEGIN IMMEDIATE so a crash
        mid-sequence cannot leave the player charged but still indebted, or vice versa.

        Returns a dict with principal, fee, balance_before, new_balance, nonprofit_total.
        Raises ValueError("No outstanding loan to repay.") if there is no loan.
        Raises ValueError("Player not found.") if the player row is missing.
        """
        normalized_guild_id = self.normalize_guild_id(guild_id)

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            cursor.execute(
                """
                SELECT outstanding_principal, outstanding_fee,
                       COALESCE(total_fees_paid, 0) as total_fees_paid
                FROM loan_state
                WHERE discord_id = ? AND guild_id = ?
                """,
                (discord_id, normalized_guild_id),
            )
            state_row = cursor.fetchone()
            if not state_row:
                raise ValueError("No outstanding loan to repay.")

            principal = state_row["outstanding_principal"] or 0
            fee = state_row["outstanding_fee"] or 0
            total_owed = principal + fee
            if total_owed <= 0:
                raise ValueError("No outstanding loan to repay.")

            cursor.execute(
                "SELECT COALESCE(jopacoin_balance, 0) as balance FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, normalized_guild_id),
            )
            balance_row = cursor.fetchone()
            if not balance_row:
                raise ValueError("Player not found.")
            balance_before = balance_row["balance"]

            self._set_economy_ledger_context(
                cursor,
                source="loan_repayment",
                related_type="loan",
                related_id=discord_id,
                reason="loan principal and fee repayment",
                metadata={"principal": principal, "fee": fee, "total_owed": total_owed},
            )
            try:
                cursor.execute(
                    """
                    UPDATE players
                    SET jopacoin_balance = COALESCE(jopacoin_balance, 0) - ?, updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    (total_owed, discord_id, normalized_guild_id),
                )
            finally:
                self._clear_economy_ledger_context(cursor)

            self._set_economy_ledger_context(
                cursor,
                source="loan_repayment",
                related_type="loan",
                related_id=discord_id,
                reason="loan repayment fee reserve credit",
                metadata={"principal": principal, "fee": fee, "total_owed": total_owed},
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
                    (normalized_guild_id, fee),
                )
            finally:
                self._clear_economy_ledger_context(cursor)
            cursor.execute(
                "SELECT total_collected FROM nonprofit_fund WHERE guild_id = ?",
                (normalized_guild_id,),
            )
            nonprofit_row = cursor.fetchone()
            nonprofit_total = nonprofit_row["total_collected"] if nonprofit_row else fee

            cursor.execute(
                """
                UPDATE loan_state
                SET outstanding_principal = 0,
                    outstanding_fee = 0,
                    total_fees_paid = COALESCE(total_fees_paid, 0) + ?,
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (fee, discord_id, normalized_guild_id),
            )

            return {
                "principal": principal,
                "fee": fee,
                "total_owed": total_owed,
                "balance_before": balance_before,
                "new_balance": balance_before - total_owed,
                "nonprofit_total": nonprofit_total,
            }

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
