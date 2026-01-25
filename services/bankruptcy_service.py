"""
Service for handling player bankruptcy declarations.

Bankruptcy allows players with negative balances to reset their debt,
but at the cost of reduced winnings for the next several games.
"""

import time
from dataclasses import dataclass

from config import (
    BANKRUPTCY_COOLDOWN_SECONDS,
    BANKRUPTCY_FRESH_START_BALANCE,
    BANKRUPTCY_PENALTY_GAMES,
    BANKRUPTCY_PENALTY_RATE,
)
from repositories.base_repository import BaseRepository
from repositories.player_repository import PlayerRepository
from services.result import Result
from services import error_codes
from services.interfaces import IBankruptcyService


@dataclass
class BankruptcyState:
    """Current bankruptcy state for a player."""

    discord_id: int
    last_bankruptcy_at: int | None  # Unix timestamp
    penalty_games_remaining: int
    is_on_cooldown: bool
    cooldown_ends_at: int | None  # Unix timestamp


@dataclass
class BankruptcyDeclaration:
    """Result of declaring bankruptcy."""

    debt_cleared: int
    penalty_games: int
    penalty_rate: float
    new_balance: int


@dataclass
class PenaltyApplication:
    """Result of applying penalty to winnings."""

    original: int
    penalized: int
    penalty_applied: int


class BankruptcyRepository(BaseRepository):
    """Data access for bankruptcy state."""

    def get_state(self, discord_id: int) -> dict | None:
        """Get bankruptcy state for a player."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT discord_id, last_bankruptcy_at, penalty_games_remaining,
                       COALESCE(bankruptcy_count, 0) as bankruptcy_count
                FROM bankruptcy_state
                WHERE discord_id = ?
                """,
                (discord_id,),
            )
            row = cursor.fetchone()
            if not row:
                return None
            return {
                "discord_id": row["discord_id"],
                "last_bankruptcy_at": row["last_bankruptcy_at"],
                "penalty_games_remaining": row["penalty_games_remaining"],
                "bankruptcy_count": row["bankruptcy_count"],
            }

    def get_bulk_states(self, discord_ids: list[int]) -> dict[int, dict]:
        """Get bankruptcy states for multiple players in one query.

        Args:
            discord_ids: List of Discord IDs to fetch states for.

        Returns:
            Dict mapping discord_id to state dict. Only includes users
            who have bankruptcy records in the database (excludes users
            who have never declared bankruptcy). Empty input returns empty dict.
        """
        if not discord_ids:
            return {}
        with self.connection() as conn:
            cursor = conn.cursor()
            placeholders = ",".join("?" * len(discord_ids))
            cursor.execute(
                f"""
                SELECT discord_id, last_bankruptcy_at, penalty_games_remaining,
                       COALESCE(bankruptcy_count, 0) as bankruptcy_count
                FROM bankruptcy_state
                WHERE discord_id IN ({placeholders})
                """,
                discord_ids,
            )
            return {row["discord_id"]: dict(row) for row in cursor.fetchall()}

    def upsert_state(
        self, discord_id: int, last_bankruptcy_at: int, penalty_games_remaining: int
    ) -> None:
        """Create or update bankruptcy state, incrementing bankruptcy_count."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO bankruptcy_state (discord_id, last_bankruptcy_at, penalty_games_remaining, bankruptcy_count, updated_at)
                VALUES (?, ?, ?, 1, CURRENT_TIMESTAMP)
                ON CONFLICT(discord_id) DO UPDATE SET
                    last_bankruptcy_at = excluded.last_bankruptcy_at,
                    penalty_games_remaining = excluded.penalty_games_remaining,
                    bankruptcy_count = COALESCE(bankruptcy_state.bankruptcy_count, 0) + 1,
                    updated_at = CURRENT_TIMESTAMP
                """,
                (discord_id, last_bankruptcy_at, penalty_games_remaining),
            )

    def reset_cooldown_only(
        self, discord_id: int, last_bankruptcy_at: int, penalty_games_remaining: int
    ) -> None:
        """Reset cooldown and penalty without incrementing bankruptcy_count."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE bankruptcy_state
                SET last_bankruptcy_at = ?,
                    penalty_games_remaining = ?,
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
                """,
                (last_bankruptcy_at, penalty_games_remaining, discord_id),
            )

    def decrement_penalty_games(self, discord_id: int) -> int:
        """
        Decrement penalty games remaining by 1 if > 0.

        Returns the new count.
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE bankruptcy_state
                SET penalty_games_remaining = MAX(0, penalty_games_remaining - 1),
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
                """,
                (discord_id,),
            )
            cursor.execute(
                "SELECT penalty_games_remaining FROM bankruptcy_state WHERE discord_id = ?",
                (discord_id,),
            )
            row = cursor.fetchone()
            return row["penalty_games_remaining"] if row else 0

    def get_penalty_games(self, discord_id: int) -> int:
        """Get the number of penalty games remaining for a player."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT penalty_games_remaining FROM bankruptcy_state WHERE discord_id = ?",
                (discord_id,),
            )
            row = cursor.fetchone()
            return row["penalty_games_remaining"] if row else 0


class BankruptcyService(IBankruptcyService):
    """
    Handles bankruptcy declarations and penalties.

    When a player declares bankruptcy:
    1. Their debt is cleared (balance set to 0)
    2. They receive reduced winnings for the next N games
    3. They cannot declare bankruptcy again for a cooldown period
    """

    def __init__(
        self,
        bankruptcy_repo: BankruptcyRepository,
        player_repo: PlayerRepository,
        cooldown_seconds: int | None = None,
        penalty_games: int | None = None,
        penalty_rate: float | None = None,
    ):
        self.bankruptcy_repo = bankruptcy_repo
        self.player_repo = player_repo
        self.cooldown_seconds = (
            cooldown_seconds if cooldown_seconds is not None else BANKRUPTCY_COOLDOWN_SECONDS
        )
        self.penalty_games = (
            penalty_games if penalty_games is not None else BANKRUPTCY_PENALTY_GAMES
        )
        self.penalty_rate = penalty_rate if penalty_rate is not None else BANKRUPTCY_PENALTY_RATE

    def get_state(self, discord_id: int) -> BankruptcyState:
        """Get the current bankruptcy state for a player."""
        state = self.bankruptcy_repo.get_state(discord_id)
        now = int(time.time())

        if not state:
            return BankruptcyState(
                discord_id=discord_id,
                last_bankruptcy_at=None,
                penalty_games_remaining=0,
                is_on_cooldown=False,
                cooldown_ends_at=None,
            )

        last_bankruptcy = state["last_bankruptcy_at"]
        cooldown_ends = last_bankruptcy + self.cooldown_seconds if last_bankruptcy else None
        is_on_cooldown = cooldown_ends is not None and now < cooldown_ends

        return BankruptcyState(
            discord_id=discord_id,
            last_bankruptcy_at=last_bankruptcy,
            penalty_games_remaining=state["penalty_games_remaining"],
            is_on_cooldown=is_on_cooldown,
            cooldown_ends_at=cooldown_ends if is_on_cooldown else None,
        )

    def get_bulk_states(self, discord_ids: list[int]) -> dict[int, BankruptcyState]:
        """Get bankruptcy states for multiple players efficiently.

        Args:
            discord_ids: List of Discord IDs to fetch states for.
                Duplicates are automatically de-duped before processing.

        Returns:
            Dict mapping discord_id to BankruptcyState. Players without
            bankruptcy history return a default state (no penalty, no cooldown).
            Empty input returns empty dict.
        """
        if not discord_ids:
            return {}
        unique_ids = list(set(discord_ids))  # De-dupe for efficiency
        raw_states = self.bankruptcy_repo.get_bulk_states(unique_ids)
        now = int(time.time())
        result = {}
        for discord_id in unique_ids:
            state = raw_states.get(discord_id)
            if not state:
                result[discord_id] = BankruptcyState(
                    discord_id=discord_id,
                    last_bankruptcy_at=None,
                    penalty_games_remaining=0,
                    is_on_cooldown=False,
                    cooldown_ends_at=None,
                )
            else:
                last_bankruptcy = state["last_bankruptcy_at"]
                cooldown_ends = last_bankruptcy + self.cooldown_seconds if last_bankruptcy else None
                is_on_cooldown = cooldown_ends is not None and now < cooldown_ends
                result[discord_id] = BankruptcyState(
                    discord_id=discord_id,
                    last_bankruptcy_at=last_bankruptcy,
                    penalty_games_remaining=state["penalty_games_remaining"],
                    is_on_cooldown=is_on_cooldown,
                    cooldown_ends_at=cooldown_ends if is_on_cooldown else None,
                )
        return result

    def can_declare_bankruptcy(self, discord_id: int) -> dict:
        """
        Check if a player can declare bankruptcy.

        Returns:
            Dict with 'allowed' (bool) and 'reason' (str if not allowed)
        """
        balance = self.player_repo.get_balance(discord_id)
        state = self.get_state(discord_id)

        if balance >= 0:
            return {
                "allowed": False,
                "reason": "not_in_debt",
                "balance": balance,
            }

        if state.is_on_cooldown:
            return {
                "allowed": False,
                "reason": "on_cooldown",
                "cooldown_ends_at": state.cooldown_ends_at,
            }

        return {"allowed": True, "debt": abs(balance)}

    def declare_bankruptcy(self, discord_id: int) -> dict:
        """
        Declare bankruptcy for a player.

        Clears their debt and applies the penalty.

        Returns:
            Dict with 'success', 'debt_cleared', 'penalty_games'
        """
        check = self.can_declare_bankruptcy(discord_id)
        if not check["allowed"]:
            return {"success": False, **check}

        debt_cleared = check["debt"]
        now = int(time.time())

        # Clear debt and give fresh start balance
        self.player_repo.update_balance(discord_id, BANKRUPTCY_FRESH_START_BALANCE)

        # Record bankruptcy and set penalty
        self.bankruptcy_repo.upsert_state(
            discord_id=discord_id,
            last_bankruptcy_at=now,
            penalty_games_remaining=self.penalty_games,
        )

        return {
            "success": True,
            "debt_cleared": debt_cleared,
            "penalty_games": self.penalty_games,
            "penalty_rate": self.penalty_rate,
        }

    def apply_penalty_to_winnings(self, discord_id: int, amount: int) -> dict[str, int]:
        """
        Apply bankruptcy penalty to winnings if applicable.

        Args:
            discord_id: The player's Discord ID
            amount: The original winnings amount

        Returns:
            Dict with 'original', 'penalized', 'penalty_applied'
        """
        penalty_games = self.bankruptcy_repo.get_penalty_games(discord_id)

        if penalty_games <= 0:
            return {"original": amount, "penalized": amount, "penalty_applied": 0}

        # Apply penalty rate (e.g., 0.5 means they get half)
        penalized = int(amount * self.penalty_rate)
        penalty_applied = amount - penalized

        return {
            "original": amount,
            "penalized": penalized,
            "penalty_applied": penalty_applied,
        }

    def on_game_played(self, discord_id: int) -> int:
        """
        Called when a player plays a game. Decrements their penalty counter.

        Returns the remaining penalty games.
        """
        return self.bankruptcy_repo.decrement_penalty_games(discord_id)

    # =========================================================================
    # Result-returning methods (new API)
    # These methods return Result types for cleaner error handling.
    # The old dict-returning methods are kept for backward compatibility.
    # =========================================================================

    def validate_bankruptcy(self, discord_id: int) -> Result[int]:
        """
        Check if a player can declare bankruptcy.

        Returns:
            Result.ok(debt_amount) if allowed
            Result.fail(error_message, code) if not allowed

        Error codes:
            - NOT_IN_DEBT: Player has non-negative balance
            - BANKRUPTCY_COOLDOWN: Cooldown hasn't expired
        """
        balance = self.player_repo.get_balance(discord_id)
        state = self.get_state(discord_id)

        if balance >= 0:
            return Result.fail(
                f"You're not in debt (balance: {balance}). Only players in debt can declare bankruptcy.",
                code=error_codes.NOT_IN_DEBT,
            )

        if state.is_on_cooldown:
            remaining = state.cooldown_ends_at - int(time.time())
            days = remaining // 86400
            hours = (remaining % 86400) // 3600
            return Result.fail(
                f"Bankruptcy cooldown active. Try again in {days}d {hours}h.",
                code=error_codes.BANKRUPTCY_COOLDOWN,
            )

        return Result.ok(abs(balance))

    def execute_bankruptcy(self, discord_id: int) -> Result[BankruptcyDeclaration]:
        """
        Declare bankruptcy for a player.

        Clears their debt and applies the penalty.

        Returns:
            Result.ok(BankruptcyDeclaration) on success
            Result.fail(error_message, code) on failure
        """
        validation = self.validate_bankruptcy(discord_id)
        if not validation.success:
            return Result.fail(validation.error, code=validation.error_code)

        debt_cleared = validation.value
        now = int(time.time())

        # Clear debt and give fresh start balance
        self.player_repo.update_balance(discord_id, BANKRUPTCY_FRESH_START_BALANCE)

        # Record bankruptcy and set penalty
        self.bankruptcy_repo.upsert_state(
            discord_id=discord_id,
            last_bankruptcy_at=now,
            penalty_games_remaining=self.penalty_games,
        )

        return Result.ok(
            BankruptcyDeclaration(
                debt_cleared=debt_cleared,
                penalty_games=self.penalty_games,
                penalty_rate=self.penalty_rate,
                new_balance=BANKRUPTCY_FRESH_START_BALANCE,
            )
        )

    def calculate_penalized_winnings(
        self, discord_id: int, amount: int
    ) -> Result[PenaltyApplication]:
        """
        Calculate penalized winnings for a player under bankruptcy penalty.

        This does NOT apply the penalty - just calculates what it would be.
        Use this for display purposes or before actually applying.

        Returns:
            Result.ok(PenaltyApplication) - always succeeds
        """
        penalty_games = self.bankruptcy_repo.get_penalty_games(discord_id)

        if penalty_games <= 0:
            return Result.ok(
                PenaltyApplication(
                    original=amount,
                    penalized=amount,
                    penalty_applied=0,
                )
            )

        # Apply penalty rate (e.g., 0.5 means they get half)
        penalized = int(amount * self.penalty_rate)
        penalty_applied = amount - penalized

        return Result.ok(
            PenaltyApplication(
                original=amount,
                penalized=penalized,
                penalty_applied=penalty_applied,
            )
        )
