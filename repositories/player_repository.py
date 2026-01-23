"""
Repository for player data access.
"""

import json
import logging

from config import NEW_PLAYER_EXCLUSION_BOOST
from domain.models.player import Player
from repositories.base_repository import BaseRepository
from repositories.interfaces import IPlayerRepository

logger = logging.getLogger("cama_bot.repositories.player")


class PlayerRepository(BaseRepository, IPlayerRepository):
    """
    Handles all player-related database operations.

    Responsibilities:
    - CRUD operations for players
    - Glicko rating persistence
    - Role preferences storage
    - Exclusion count tracking
    """

    def add(
        self,
        discord_id: int,
        discord_username: str,
        dotabuff_url: str | None = None,
        steam_id: int | None = None,
        initial_mmr: int | None = None,
        preferred_roles: list[str] | None = None,
        main_role: str | None = None,
        glicko_rating: float | None = None,
        glicko_rd: float | None = None,
        glicko_volatility: float | None = None,
    ) -> None:
        """
        Add a new player to the database.

        Args:
            discord_id: Discord user ID
            discord_username: Discord username
            dotabuff_url: Optional Dotabuff profile URL
            steam_id: Optional Steam32 account ID for match enrichment
            initial_mmr: Optional initial MMR from OpenDota
            preferred_roles: Optional list of preferred roles ["1", "2", etc.]
            main_role: Optional primary role
            glicko_rating: Optional initial Glicko rating
            glicko_rd: Optional initial rating deviation
            glicko_volatility: Optional initial volatility

        Raises:
            ValueError: If player with this discord_id already exists
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            # Check if player already exists
            cursor.execute("SELECT discord_id FROM players WHERE discord_id = ?", (discord_id,))
            if cursor.fetchone():
                raise ValueError(f"Player with Discord ID {discord_id} already exists.")

            roles_json = json.dumps(preferred_roles) if preferred_roles else None

            cursor.execute(
                """
                INSERT INTO players
                (discord_id, discord_username, dotabuff_url, steam_id, initial_mmr, current_mmr,
                 preferred_roles, main_role, glicko_rating, glicko_rd, glicko_volatility,
                 exclusion_count, jopacoin_balance, updated_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 3, CURRENT_TIMESTAMP)
            """,
                (
                    discord_id,
                    discord_username,
                    dotabuff_url,
                    steam_id,
                    initial_mmr,
                    initial_mmr,
                    roles_json,
                    main_role,
                    glicko_rating,
                    glicko_rd,
                    glicko_volatility,
                    NEW_PLAYER_EXCLUSION_BOOST,
                ),
            )

    def get_by_id(self, discord_id: int) -> Player | None:
        """
        Get player by Discord ID.

        Returns:
            Player object or None if not found
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT * FROM players WHERE discord_id = ?", (discord_id,))
            row = cursor.fetchone()

            if not row:
                return None

            return self._row_to_player(row)

    def get_by_ids(self, discord_ids: list[int]) -> list[Player]:
        """
        Get multiple players by Discord IDs.

        IMPORTANT: Returns players in the SAME ORDER as the input discord_ids.
        """
        if not discord_ids:
            return []

        with self.connection() as conn:
            cursor = conn.cursor()

            placeholders = ",".join("?" * len(discord_ids))
            cursor.execute(
                f"SELECT * FROM players WHERE discord_id IN ({placeholders})", discord_ids
            )
            rows = cursor.fetchall()

            # Create mapping for order preservation
            id_to_row = {}
            for row in rows:
                discord_id = row["discord_id"]
                if discord_id in id_to_row:
                    logger.warning(f"Duplicate player entry: discord_id={discord_id}")
                    continue
                id_to_row[discord_id] = row

            # Return in same order as input
            players = []
            for discord_id in discord_ids:
                if discord_id not in id_to_row:
                    logger.warning(f"Player not found: discord_id={discord_id}")
                    continue
                players.append(self._row_to_player(id_to_row[discord_id]))

            return players

    def get_by_username(self, username: str) -> list[dict]:
        """
        Find players whose Discord username matches the provided value (case-insensitive, partial match).

        Args:
            username: Full or partial Discord username (e.g., 'user#1234' or just 'user').

        Returns:
            List of dicts containing discord_id and discord_username for each match.
        """
        if not username:
            return []

        search = f"%{username.lower()}%"
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT discord_id, discord_username
                FROM players
                WHERE LOWER(discord_username) LIKE ?
                """,
                (search,),
            )
            rows = cursor.fetchall()
            return [
                {"discord_id": row["discord_id"], "discord_username": row["discord_username"]}
                for row in rows
            ]

    def get_all(self) -> list[Player]:
        """Get all players from database."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT * FROM players")
            rows = cursor.fetchall()
            return [self._row_to_player(row) for row in rows]

    def get_leaderboard(self, limit: int = 20, offset: int = 0) -> list[Player]:
        """
        Get players for leaderboard, sorted by jopacoin balance descending.

        Uses SQL sorting to avoid loading all players into memory.

        Args:
            limit: Maximum number of players to return
            offset: Number of players to skip (for pagination)

        Returns:
            List of Player objects sorted by jopacoin_balance DESC, wins DESC, glicko_rating DESC
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT * FROM players
                ORDER BY
                    COALESCE(jopacoin_balance, 0) DESC,
                    COALESCE(wins, 0) DESC,
                    COALESCE(glicko_rating, 0) DESC
                LIMIT ? OFFSET ?
                """,
                (limit, offset),
            )
            rows = cursor.fetchall()
            return [self._row_to_player(row) for row in rows]

    def get_player_count(self) -> int:
        """Get total number of players."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT COUNT(*) as count FROM players")
            row = cursor.fetchone()
            return row["count"] if row else 0

    def exists(self, discord_id: int) -> bool:
        """Check if a player exists."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT 1 FROM players WHERE discord_id = ?", (discord_id,))
            return cursor.fetchone() is not None

    def update_roles(self, discord_id: int, roles: list[str]) -> None:
        """Update player's preferred roles."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET preferred_roles = ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
            """,
                (json.dumps(roles), discord_id),
            )

    def update_glicko_rating(
        self, discord_id: int, rating: float, rd: float, volatility: float
    ) -> None:
        """Update player's Glicko-2 rating."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET glicko_rating = ?, glicko_rd = ?, glicko_volatility = ?,
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
            """,
                (rating, rd, volatility, discord_id),
            )

    def get_glicko_rating(self, discord_id: int) -> tuple[float, float, float] | None:
        """
        Get player's Glicko-2 rating data.

        Returns:
            Tuple of (rating, rd, volatility) or None if not found
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT glicko_rating, glicko_rd, glicko_volatility
                FROM players WHERE discord_id = ?
            """,
                (discord_id,),
            )

            row = cursor.fetchone()
            if row and row[0] is not None:
                return (row[0], row[1], row[2])
            return None

    def get_last_match_date(self, discord_id: int) -> tuple | None:
        """
        Get the last_match_date and created_at for a player.

        Returns:
            Tuple (last_match_date, created_at) or None if player not found.
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT last_match_date, created_at
                FROM players
                WHERE discord_id = ?
                """,
                (discord_id,),
            )
            row = cursor.fetchone()
            if not row:
                return None
            return (row["last_match_date"], row["created_at"])

    def get_last_match_dates(self, discord_ids: list[int]) -> dict[int, str | None]:
        """
        Get last_match_date for multiple players.

        Args:
            discord_ids: List of Discord user IDs

        Returns:
            Dict mapping discord_id to last_match_date (ISO string or None)
        """
        if not discord_ids:
            return {}

        with self.connection() as conn:
            cursor = conn.cursor()
            placeholders = ",".join("?" * len(discord_ids))
            cursor.execute(
                f"""
                SELECT discord_id, last_match_date
                FROM players
                WHERE discord_id IN ({placeholders})
                """,
                discord_ids,
            )
            rows = cursor.fetchall()
            return {row["discord_id"]: row["last_match_date"] for row in rows}

    def get_game_count(self, discord_id: int) -> int:
        """Return total games played (wins + losses)."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT wins, losses
                FROM players
                WHERE discord_id = ?
                """,
                (discord_id,),
            )
            row = cursor.fetchone()
            if not row:
                return 0
            wins = row["wins"] or 0
            losses = row["losses"] or 0
            return int(wins) + int(losses)

    def update_last_match_date(self, discord_id: int, timestamp: str | None = None) -> None:
        """
        Update last_match_date for a player.

        Args:
            discord_id: Player ID
            timestamp: ISO timestamp string; if None, uses CURRENT_TIMESTAMP.
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            if timestamp is None:
                cursor.execute(
                    """
                    UPDATE players
                    SET last_match_date = CURRENT_TIMESTAMP,
                        updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ?
                    """,
                    (discord_id,),
                )
            else:
                cursor.execute(
                    """
                    UPDATE players
                    SET last_match_date = ?, updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ?
                    """,
                    (timestamp, discord_id),
                )

    def update_mmr(self, discord_id: int, new_mmr: float) -> None:
        """Update player's current MMR."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET current_mmr = ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
            """,
                (new_mmr, discord_id),
            )

    def get_balance(self, discord_id: int) -> int:
        """Get a player's jopacoin balance."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT COALESCE(jopacoin_balance, 0) as balance FROM players WHERE discord_id = ?",
                (discord_id,),
            )
            row = cursor.fetchone()
            return int(row["balance"]) if row else 0

    def update_balance(self, discord_id: int, amount: int) -> None:
        """Set a player's jopacoin balance to a specific amount."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET jopacoin_balance = ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
            """,
                (amount, discord_id),
            )
            # Track lowest balance
            cursor.execute(
                """
                UPDATE players
                SET lowest_balance_ever = ?
                WHERE discord_id = ?
                AND (lowest_balance_ever IS NULL OR ? < lowest_balance_ever)
                """,
                (amount, discord_id, amount),
            )

    def add_balance(self, discord_id: int, amount: int) -> None:
        """Add or subtract from a player's jopacoin balance."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
            """,
                (amount, discord_id),
            )
            # Track lowest balance if this was a decrease
            if amount < 0:
                cursor.execute(
                    """
                    UPDATE players
                    SET lowest_balance_ever = jopacoin_balance
                    WHERE discord_id = ?
                    AND (lowest_balance_ever IS NULL OR jopacoin_balance < lowest_balance_ever)
                    """,
                    (discord_id,),
                )

    def add_balance_many(self, deltas_by_discord_id: dict[int, int]) -> None:
        """
        Apply multiple balance deltas in a single transaction.
        """
        if not deltas_by_discord_id:
            return
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.executemany(
                """
                UPDATE players
                SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
                """,
                [(delta, discord_id) for discord_id, delta in deltas_by_discord_id.items()],
            )
            # Track lowest balance for players who had negative deltas
            negative_ids = [did for did, delta in deltas_by_discord_id.items() if delta < 0]
            if negative_ids:
                placeholders = ",".join("?" * len(negative_ids))
                cursor.execute(
                    f"""
                    UPDATE players
                    SET lowest_balance_ever = jopacoin_balance
                    WHERE discord_id IN ({placeholders})
                    AND (lowest_balance_ever IS NULL OR jopacoin_balance < lowest_balance_ever)
                    """,
                    negative_ids,
                )

    def add_balance_with_garnishment(
        self, discord_id: int, amount: int, garnishment_rate: float
    ) -> dict[str, int]:
        """
        Add income with garnishment applied if player has debt.

        When a player has a negative balance (debt), a portion of their income
        is garnished to pay down the debt. The full amount is credited to the
        balance, but the return value indicates how much was "garnished" vs "net".

        Returns:
            Dict with 'gross', 'garnished', 'net' amounts.
            - gross: The original income amount
            - garnished: Amount that went toward debt repayment
            - net: Amount the player "feels" they received (gross - garnished)
        """
        if amount <= 0:
            return {"gross": amount, "garnished": 0, "net": amount}

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            cursor.execute(
                "SELECT COALESCE(jopacoin_balance, 0) as balance FROM players WHERE discord_id = ?",
                (discord_id,),
            )
            row = cursor.fetchone()
            if not row:
                raise ValueError("Player not found.")

            current_balance = int(row["balance"])

            if current_balance >= 0:
                # No debt, full amount credited without garnishment
                cursor.execute(
                    """
                    UPDATE players
                    SET jopacoin_balance = jopacoin_balance + ?, updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ?
                    """,
                    (amount, discord_id),
                )
                return {"gross": amount, "garnished": 0, "net": amount}

            # Player has debt - apply garnishment
            garnished = int(amount * garnishment_rate)
            net = amount - garnished

            # Full amount goes to balance (paying down debt + net income)
            cursor.execute(
                """
                UPDATE players
                SET jopacoin_balance = jopacoin_balance + ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
                """,
                (amount, discord_id),
            )

            return {"gross": amount, "garnished": garnished, "net": net}

    def pay_debt_atomic(
        self, from_discord_id: int, to_discord_id: int, amount: int
    ) -> dict[str, int]:
        """
        Atomically transfer jopacoin from one player to pay down another's debt.

        Args:
            from_discord_id: Player paying (must have positive balance)
            to_discord_id: Player receiving (can be same as from for self-payment)
            amount: Amount to transfer

        Returns:
            Dict with 'amount_paid', 'from_new_balance', 'to_new_balance'

        Raises:
            ValueError if insufficient funds, player not found, or recipient has no debt
        """
        if amount <= 0:
            raise ValueError("Amount must be positive.")

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            # Get sender balance
            cursor.execute(
                "SELECT COALESCE(jopacoin_balance, 0) as balance FROM players WHERE discord_id = ?",
                (from_discord_id,),
            )
            from_row = cursor.fetchone()
            if not from_row:
                raise ValueError("Sender not found.")

            from_balance = int(from_row["balance"])
            if from_balance < amount:
                raise ValueError(f"Insufficient balance. You have {from_balance} jopacoin.")

            # Get recipient balance
            cursor.execute(
                "SELECT COALESCE(jopacoin_balance, 0) as balance FROM players WHERE discord_id = ?",
                (to_discord_id,),
            )
            to_row = cursor.fetchone()
            if not to_row:
                raise ValueError("Recipient not found.")

            to_balance = int(to_row["balance"])
            if to_balance >= 0:
                raise ValueError("Recipient has no debt to pay off.")

            # Cap amount at the debt (don't overpay)
            debt = abs(to_balance)
            actual_amount = min(amount, debt)

            # Deduct from sender
            cursor.execute(
                """
                UPDATE players
                SET jopacoin_balance = jopacoin_balance - ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
                """,
                (actual_amount, from_discord_id),
            )

            # Add to recipient (reduces debt)
            cursor.execute(
                """
                UPDATE players
                SET jopacoin_balance = jopacoin_balance + ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
                """,
                (actual_amount, to_discord_id),
            )

            return {
                "amount_paid": actual_amount,
                "from_new_balance": from_balance - actual_amount,
                "to_new_balance": to_balance + actual_amount,
            }

    def get_players_with_negative_balance(self) -> list[dict]:
        """
        Get all players with negative balance for interest application.

        Returns:
            List of dicts with 'discord_id', 'balance', and 'username' for each debtor,
            sorted by balance ascending (most debt first).
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """SELECT discord_id, jopacoin_balance, discord_username
                FROM players
                WHERE jopacoin_balance < 0
                ORDER BY jopacoin_balance ASC"""
            )
            return [
                {
                    "discord_id": row["discord_id"],
                    "balance": row["jopacoin_balance"],
                    "username": row["discord_username"],
                }
                for row in cursor.fetchall()
            ]

    def get_stimulus_eligible_players(self) -> list[dict]:
        """
        Get players eligible for stimulus: non-negative balance, excluding top 3 by balance.

        Returns:
            List of dicts with 'discord_id' and 'balance' for eligible players.
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            # Get all players with non-negative balance, ordered by balance DESC
            # Skip the top 3 (richest players)
            cursor.execute(
                """
                SELECT discord_id, jopacoin_balance
                FROM players
                WHERE jopacoin_balance >= 0
                ORDER BY jopacoin_balance DESC
                LIMIT -1 OFFSET 3
                """
            )
            return [
                {"discord_id": row["discord_id"], "balance": row["jopacoin_balance"]}
                for row in cursor.fetchall()
            ]

    def apply_interest_bulk(self, updates: list[tuple[int, int]]) -> int:
        """
        Apply interest charges to multiple players in a single transaction.

        Interest is subtracted from balance (making debt larger/more negative).

        Args:
            updates: List of (discord_id, interest_amount) tuples

        Returns:
            Number of rows updated.
        """
        if not updates:
            return 0
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.executemany(
                """
                UPDATE players
                SET jopacoin_balance = jopacoin_balance - ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
                """,
                [(interest, discord_id) for discord_id, interest in updates],
            )
            return cursor.rowcount

    def increment_wins(self, discord_id: int) -> None:
        """Increment player's win count."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players SET wins = wins + 1, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
            """,
                (discord_id,),
            )

    def increment_losses(self, discord_id: int) -> None:
        """Increment player's loss count."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players SET losses = losses + 1, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
            """,
                (discord_id,),
            )

    def apply_match_outcome(self, winning_ids: list[int], losing_ids: list[int]) -> None:
        """
        Apply win/loss increments for a match in a single transaction.
        """
        if not winning_ids and not losing_ids:
            return
        with self.connection() as conn:
            cursor = conn.cursor()
            if winning_ids:
                cursor.executemany(
                    """
                    UPDATE players SET wins = wins + 1, updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ?
                    """,
                    [(pid,) for pid in winning_ids],
                )
            if losing_ids:
                cursor.executemany(
                    """
                    UPDATE players SET losses = losses + 1, updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ?
                    """,
                    [(pid,) for pid in losing_ids],
                )

    def update_glicko_ratings_bulk(self, updates: list[tuple[int, float, float, float]]) -> int:
        """
        Bulk update Glicko ratings in a single transaction.

        updates: List of (discord_id, rating, rd, volatility)
        Returns number of rows updated.
        """
        if not updates:
            return 0
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.executemany(
                """
                UPDATE players
                SET glicko_rating = ?, glicko_rd = ?, glicko_volatility = ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
                """,
                [(rating, rd, vol, pid) for pid, rating, rd, vol in updates],
            )
            return cursor.rowcount

    def get_exclusion_counts(self, discord_ids: list[int]) -> dict[int, int]:
        """Get exclusion counts for multiple players."""
        if not discord_ids:
            return {}

        with self.connection() as conn:
            cursor = conn.cursor()
            placeholders = ",".join("?" * len(discord_ids))
            cursor.execute(
                f"SELECT discord_id, COALESCE(exclusion_count, 0) as exclusion_count "
                f"FROM players WHERE discord_id IN ({placeholders})",
                discord_ids,
            )
            rows = cursor.fetchall()
            return {row["discord_id"]: row["exclusion_count"] for row in rows}

    def increment_exclusion_count(self, discord_id: int) -> None:
        """Increment player's exclusion count by 4."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET exclusion_count = COALESCE(exclusion_count, 0) + 4,
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
            """,
                (discord_id,),
            )

    def increment_exclusion_count_half(self, discord_id: int) -> None:
        """Increment player's exclusion count by 2 (half the normal bonus).

        Used for conditional players who weren't picked.
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET exclusion_count = COALESCE(exclusion_count, 0) + 2,
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
            """,
                (discord_id,),
            )

    def decay_exclusion_count(self, discord_id: int) -> None:
        """Decay player's exclusion count by halving it."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET exclusion_count = COALESCE(exclusion_count, 0) / 2,
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
            """,
                (discord_id,),
            )

    def delete(self, discord_id: int) -> bool:
        """
        Delete a player from the database.

        Returns:
            True if deleted, False if player didn't exist
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            cursor.execute("SELECT discord_id FROM players WHERE discord_id = ?", (discord_id,))
            if not cursor.fetchone():
                return False

            cursor.execute("DELETE FROM players WHERE discord_id = ?", (discord_id,))
            cursor.execute("DELETE FROM match_participants WHERE discord_id = ?", (discord_id,))
            cursor.execute("DELETE FROM rating_history WHERE discord_id = ?", (discord_id,))

            return True

    def delete_all(self) -> int:
        """
        Delete all players (for testing).

        Returns:
            Number of players deleted
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            cursor.execute("SELECT COUNT(*) FROM players")
            count = cursor.fetchone()[0]

            cursor.execute("DELETE FROM players")
            cursor.execute("DELETE FROM match_participants")
            cursor.execute("DELETE FROM rating_history")

            return count

    def get_by_steam_id(self, steam_id: int) -> Player | None:
        """
        Get player by Steam ID (32-bit account_id).

        Args:
            steam_id: The 32-bit Steam account ID

        Returns:
            Player object or None if not found
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT * FROM players WHERE steam_id = ?", (steam_id,))
            row = cursor.fetchone()

            if not row:
                return None

            return self._row_to_player(row)

    def get_steam_id(self, discord_id: int) -> int | None:
        """
        Get a player's Steam ID.

        Returns:
            Steam ID (32-bit) or None if not set
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT steam_id FROM players WHERE discord_id = ?", (discord_id,))
            row = cursor.fetchone()
            return row["steam_id"] if row and row["steam_id"] else None

    def set_steam_id(self, discord_id: int, steam_id: int) -> None:
        """
        Set a player's Steam ID.

        Args:
            discord_id: The player's Discord ID
            steam_id: The 32-bit Steam account ID
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET steam_id = ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
                """,
                (steam_id, discord_id),
            )

    def get_all_with_dotabuff_no_steam_id(self) -> list[dict]:
        """
        Get all players who have a dotabuff_url but no steam_id set.
        Used for backfilling steam_id from dotabuff URLs.

        Returns:
            List of dicts with discord_id and dotabuff_url
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT discord_id, dotabuff_url
                FROM players
                WHERE dotabuff_url IS NOT NULL
                  AND dotabuff_url != ''
                  AND steam_id IS NULL
                """
            )
            return [
                {"discord_id": row["discord_id"], "dotabuff_url": row["dotabuff_url"]}
                for row in cursor.fetchall()
            ]

    def delete_fake_users(self) -> int:
        """
        Delete all fake users (discord_id < 0) and their related data.

        Returns:
            Number of fake users deleted.
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            cursor.execute("SELECT COUNT(*) FROM players WHERE discord_id < 0")
            count = cursor.fetchone()[0]
            if count == 0:
                return 0

            # Remove related records first to avoid orphan rows if FK cascades aren't enforced
            cursor.execute("DELETE FROM match_participants WHERE discord_id < 0")
            cursor.execute("DELETE FROM rating_history WHERE discord_id < 0")
            cursor.execute("DELETE FROM bets WHERE discord_id < 0")
            cursor.execute("DELETE FROM players WHERE discord_id < 0")

            return count

    def get_lowest_balance(self, discord_id: int) -> int | None:
        """Get a player's lowest balance ever recorded."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT lowest_balance_ever FROM players WHERE discord_id = ?",
                (discord_id,),
            )
            row = cursor.fetchone()
            return row["lowest_balance_ever"] if row and row["lowest_balance_ever"] is not None else None

    def update_lowest_balance_if_lower(self, discord_id: int, new_balance: int) -> bool:
        """
        Update lowest_balance_ever if new_balance is lower than current record.

        Returns True if the record was updated, False otherwise.
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET lowest_balance_ever = ?,
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
                AND (lowest_balance_ever IS NULL OR lowest_balance_ever > ?)
                """,
                (new_balance, discord_id, new_balance),
            )
            return cursor.rowcount > 0

    def get_first_calibrated_at(self, discord_id: int) -> int | None:
        """
        Get the Unix timestamp when the player first became calibrated.

        Returns:
            Unix timestamp or None if never calibrated
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT first_calibrated_at FROM players WHERE discord_id = ?",
                (discord_id,),
            )
            row = cursor.fetchone()
            return row["first_calibrated_at"] if row and row["first_calibrated_at"] else None

    def set_first_calibrated_at(self, discord_id: int, timestamp: int) -> None:
        """
        Set the first calibration timestamp for a player.

        Only sets if not already set (first calibration is permanent).

        Args:
            discord_id: Player's Discord ID
            timestamp: Unix timestamp when player first became calibrated
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET first_calibrated_at = ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND first_calibrated_at IS NULL
                """,
                (timestamp, discord_id),
            )

    def get_registered_player_count(self) -> int:
        """
        Get total count of registered players.

        Used for quorum calculation in disbursement voting.
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT COUNT(*) as count FROM players")
            row = cursor.fetchone()
            return row["count"] if row else 0

    # --- Captain eligibility (Immortal Draft) ---

    def set_captain_eligible(self, discord_id: int, eligible: bool) -> bool:
        """
        Set captain eligibility for a player.

        Args:
            discord_id: Player's Discord ID
            eligible: True to mark as captain-eligible, False to remove eligibility

        Returns:
            True if player was found and updated, False if player not found
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET is_captain_eligible = ?,
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
                """,
                (1 if eligible else 0, discord_id),
            )
            return cursor.rowcount > 0

    def get_captain_eligible(self, discord_id: int) -> bool:
        """
        Check if a player is captain-eligible.

        Args:
            discord_id: Player's Discord ID

        Returns:
            True if player is captain-eligible, False otherwise (including if not found)
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT is_captain_eligible FROM players WHERE discord_id = ?",
                (discord_id,),
            )
            row = cursor.fetchone()
            if not row:
                return False
            return bool(row["is_captain_eligible"])

    def get_captain_eligible_players(self, discord_ids: list[int]) -> list[int]:
        """
        Get list of captain-eligible player IDs from a given set of IDs.

        Args:
            discord_ids: List of Discord IDs to filter

        Returns:
            List of Discord IDs that are captain-eligible
        """
        if not discord_ids:
            return []

        with self.connection() as conn:
            cursor = conn.cursor()
            placeholders = ",".join("?" * len(discord_ids))
            cursor.execute(
                f"""
                SELECT discord_id FROM players
                WHERE discord_id IN ({placeholders})
                AND is_captain_eligible = 1
                """,
                discord_ids,
            )
            return [row["discord_id"] for row in cursor.fetchall()]

    def _row_to_player(self, row) -> Player:
        """Convert database row to Player object."""
        preferred_roles = json.loads(row["preferred_roles"]) if row["preferred_roles"] else None

        return Player(
            name=row["discord_username"],
            mmr=int(row["current_mmr"]) if row["current_mmr"] else None,
            initial_mmr=int(row["initial_mmr"]) if row["initial_mmr"] else None,
            wins=row["wins"],
            losses=row["losses"],
            preferred_roles=preferred_roles,
            main_role=row["main_role"],
            glicko_rating=row["glicko_rating"],
            glicko_rd=row["glicko_rd"],
            glicko_volatility=row["glicko_volatility"],
            discord_id=row["discord_id"],
            jopacoin_balance=row["jopacoin_balance"] if row["jopacoin_balance"] else 0,
        )
