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
        guild_id: int,
        dotabuff_url: str | None = None,
        steam_id: int | None = None,
        initial_mmr: int | None = None,
        preferred_roles: list[str] | None = None,
        main_role: str | None = None,
        glicko_rating: float | None = None,
        glicko_rd: float | None = None,
        glicko_volatility: float | None = None,
        os_mu: float | None = None,
        os_sigma: float | None = None,
    ) -> None:
        """
        Add a new player to the database.

        Args:
            discord_id: Discord user ID
            discord_username: Discord username
            guild_id: Guild ID for multi-server isolation
            dotabuff_url: Optional Dotabuff profile URL
            steam_id: Optional Steam32 account ID for match enrichment
            initial_mmr: Optional initial MMR from OpenDota
            preferred_roles: Optional list of preferred roles ["1", "2", etc.]
            main_role: Optional primary role
            glicko_rating: Optional initial Glicko rating
            glicko_rd: Optional initial rating deviation
            glicko_volatility: Optional initial volatility
            os_mu: Optional initial OpenSkill mu
            os_sigma: Optional initial OpenSkill sigma

        Raises:
            ValueError: If player with this discord_id already exists in this guild
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            # Check if player already exists in this guild
            cursor.execute(
                "SELECT discord_id FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, guild_id),
            )
            if cursor.fetchone():
                raise ValueError(f"Player with Discord ID {discord_id} already exists in this server.")

            roles_json = json.dumps(preferred_roles) if preferred_roles else None

            cursor.execute(
                """
                INSERT INTO players
                (discord_id, guild_id, discord_username, dotabuff_url, steam_id, initial_mmr, current_mmr,
                 preferred_roles, main_role, glicko_rating, glicko_rd, glicko_volatility,
                 os_mu, os_sigma, exclusion_count, jopacoin_balance, updated_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 3, CURRENT_TIMESTAMP)
            """,
                (
                    discord_id,
                    guild_id,
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
                    os_mu,
                    os_sigma,
                    NEW_PLAYER_EXCLUSION_BOOST,
                ),
            )

    def get_by_id(self, discord_id: int, guild_id: int) -> Player | None:
        """
        Get player by Discord ID and Guild ID.

        Returns:
            Player object or None if not found
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT * FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, guild_id),
            )
            row = cursor.fetchone()

            if not row:
                return None

            return self._row_to_player(row)

    def get_by_ids(self, discord_ids: list[int], guild_id: int) -> list[Player]:
        """
        Get multiple players by Discord IDs within a guild.

        IMPORTANT: Returns players in the SAME ORDER as the input discord_ids.
        """
        if not discord_ids:
            return []

        guild_id = self.normalize_guild_id(guild_id)

        with self.connection() as conn:
            cursor = conn.cursor()

            placeholders = ",".join("?" * len(discord_ids))
            cursor.execute(
                f"SELECT * FROM players WHERE discord_id IN ({placeholders}) AND guild_id = ?",
                discord_ids + [guild_id],
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

    def get_by_username(self, username: str, guild_id: int) -> list[dict]:
        """
        Find players whose Discord username matches the provided value (case-insensitive, partial match).

        Args:
            username: Full or partial Discord username (e.g., 'user#1234' or just 'user').
            guild_id: Guild ID to filter by.

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
                WHERE LOWER(discord_username) LIKE ? AND guild_id = ?
                """,
                (search, guild_id),
            )
            rows = cursor.fetchall()
            return [
                {"discord_id": row["discord_id"], "discord_username": row["discord_username"]}
                for row in rows
            ]

    def get_all(self, guild_id: int) -> list[Player]:
        """Get all players from database for a specific guild."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT * FROM players WHERE guild_id = ?", (guild_id,))
            rows = cursor.fetchall()
            return [self._row_to_player(row) for row in rows]

    def get_leaderboard(self, guild_id: int, limit: int = 20, offset: int = 0) -> list[Player]:
        """
        Get players for leaderboard, sorted by jopacoin balance descending.

        Uses SQL sorting to avoid loading all players into memory.

        Args:
            guild_id: Guild ID to filter by
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
                WHERE guild_id = ?
                ORDER BY
                    COALESCE(jopacoin_balance, 0) DESC,
                    COALESCE(wins, 0) DESC,
                    COALESCE(glicko_rating, 0) DESC
                LIMIT ? OFFSET ?
                """,
                (guild_id, limit, offset),
            )
            rows = cursor.fetchall()
            return [self._row_to_player(row) for row in rows]

    def get_leaderboard_by_glicko(
        self, guild_id: int, limit: int = 20, offset: int = 0, min_games: int = 0
    ) -> list[Player]:
        """
        Get players for leaderboard, sorted by Glicko-2 rating descending.

        Args:
            guild_id: Guild ID to filter by
            limit: Maximum number of players to return
            offset: Number of players to skip (for pagination)
            min_games: Minimum games played (wins + losses) to be included

        Returns:
            List of Player objects sorted by glicko_rating DESC (NULLs last)
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            if min_games > 0:
                cursor.execute(
                    """
                    SELECT * FROM players
                    WHERE guild_id = ? AND (COALESCE(wins, 0) + COALESCE(losses, 0)) >= ?
                    ORDER BY
                        CASE WHEN glicko_rating IS NULL THEN 1 ELSE 0 END,
                        glicko_rating DESC,
                        COALESCE(wins, 0) DESC
                    LIMIT ? OFFSET ?
                    """,
                    (guild_id, min_games, limit, offset),
                )
            else:
                cursor.execute(
                    """
                    SELECT * FROM players
                    WHERE guild_id = ?
                    ORDER BY
                        CASE WHEN glicko_rating IS NULL THEN 1 ELSE 0 END,
                        glicko_rating DESC,
                        COALESCE(wins, 0) DESC
                    LIMIT ? OFFSET ?
                    """,
                    (guild_id, limit, offset),
                )
            rows = cursor.fetchall()
            return [self._row_to_player(row) for row in rows]

    def get_leaderboard_by_openskill(
        self, guild_id: int, limit: int = 20, offset: int = 0, min_games: int = 0
    ) -> list[Player]:
        """
        Get players for leaderboard, sorted by OpenSkill mu descending.

        Args:
            guild_id: Guild ID to filter by
            limit: Maximum number of players to return
            offset: Number of players to skip (for pagination)
            min_games: Minimum games played (wins + losses) to be included

        Returns:
            List of Player objects sorted by os_mu DESC (NULLs last)
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            if min_games > 0:
                cursor.execute(
                    """
                    SELECT * FROM players
                    WHERE guild_id = ? AND (COALESCE(wins, 0) + COALESCE(losses, 0)) >= ?
                    ORDER BY
                        CASE WHEN os_mu IS NULL THEN 1 ELSE 0 END,
                        os_mu DESC,
                        COALESCE(wins, 0) DESC
                    LIMIT ? OFFSET ?
                    """,
                    (guild_id, min_games, limit, offset),
                )
            else:
                cursor.execute(
                    """
                    SELECT * FROM players
                    WHERE guild_id = ?
                    ORDER BY
                        CASE WHEN os_mu IS NULL THEN 1 ELSE 0 END,
                        os_mu DESC,
                        COALESCE(wins, 0) DESC
                    LIMIT ? OFFSET ?
                    """,
                    (guild_id, limit, offset),
                )
            rows = cursor.fetchall()
            return [self._row_to_player(row) for row in rows]

    def get_rated_player_count(self, guild_id: int, rating_type: str = "glicko") -> int:
        """
        Get total count of players with ratings.

        Args:
            guild_id: Guild ID to filter by
            rating_type: "glicko" for Glicko-2, "openskill" for OpenSkill

        Returns:
            Count of players with non-null ratings
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            if rating_type == "openskill":
                cursor.execute(
                    "SELECT COUNT(*) as count FROM players WHERE guild_id = ? AND os_mu IS NOT NULL",
                    (guild_id,),
                )
            else:
                cursor.execute(
                    "SELECT COUNT(*) as count FROM players WHERE guild_id = ? AND glicko_rating IS NOT NULL",
                    (guild_id,),
                )
            row = cursor.fetchone()
            return row["count"] if row else 0

    def get_player_count(self, guild_id: int) -> int:
        """Get total number of players in a guild."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT COUNT(*) as count FROM players WHERE guild_id = ?", (guild_id,))
            row = cursor.fetchone()
            return row["count"] if row else 0

    def exists(self, discord_id: int, guild_id: int) -> bool:
        """Check if a player exists in a guild."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT 1 FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, guild_id),
            )
            return cursor.fetchone() is not None

    def update_roles(self, discord_id: int, guild_id: int, roles: list[str]) -> None:
        """Update player's preferred roles."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET preferred_roles = ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
            """,
                (json.dumps(roles), discord_id, guild_id),
            )

    def update_glicko_rating(
        self, discord_id: int, guild_id: int, rating: float, rd: float, volatility: float
    ) -> None:
        """Update player's Glicko-2 rating."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET glicko_rating = ?, glicko_rd = ?, glicko_volatility = ?,
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
            """,
                (rating, rd, volatility, discord_id, guild_id),
            )

    def get_glicko_rating(self, discord_id: int, guild_id: int) -> tuple[float, float, float] | None:
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
                FROM players WHERE discord_id = ? AND guild_id = ?
            """,
                (discord_id, guild_id),
            )

            row = cursor.fetchone()
            if row and row[0] is not None:
                return (row[0], row[1], row[2])
            return None

    def get_last_match_date(self, discord_id: int, guild_id: int) -> tuple | None:
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
                WHERE discord_id = ? AND guild_id = ?
                """,
                (discord_id, guild_id),
            )
            row = cursor.fetchone()
            if not row:
                return None
            return (row["last_match_date"], row["created_at"])

    def get_last_match_dates(self, discord_ids: list[int], guild_id: int) -> dict[int, str | None]:
        """
        Get last_match_date for multiple players.

        Args:
            discord_ids: List of Discord user IDs
            guild_id: Guild ID to filter by

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
                WHERE discord_id IN ({placeholders}) AND guild_id = ?
                """,
                discord_ids + [guild_id],
            )
            rows = cursor.fetchall()
            return {row["discord_id"]: row["last_match_date"] for row in rows}

    def get_game_count(self, discord_id: int, guild_id: int) -> int:
        """Return total games played (wins + losses)."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT wins, losses
                FROM players
                WHERE discord_id = ? AND guild_id = ?
                """,
                (discord_id, guild_id),
            )
            row = cursor.fetchone()
            if not row:
                return 0
            wins = row["wins"] or 0
            losses = row["losses"] or 0
            return int(wins) + int(losses)

    def update_last_match_date(self, discord_id: int, guild_id: int, timestamp: str | None = None) -> None:
        """
        Update last_match_date for a player.

        Args:
            discord_id: Player ID
            guild_id: Guild ID
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
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    (discord_id, guild_id),
                )
            else:
                cursor.execute(
                    """
                    UPDATE players
                    SET last_match_date = ?, updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    (timestamp, discord_id, guild_id),
                )

    def update_mmr(self, discord_id: int, guild_id: int, new_mmr: float) -> None:
        """Update player's current MMR."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET current_mmr = ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
            """,
                (new_mmr, discord_id, guild_id),
            )

    def get_balance(self, discord_id: int, guild_id: int) -> int:
        """Get a player's jopacoin balance."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT COALESCE(jopacoin_balance, 0) as balance FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, guild_id),
            )
            row = cursor.fetchone()
            return int(row["balance"]) if row else 0

    def update_balance(self, discord_id: int, guild_id: int, amount: int) -> None:
        """Set a player's jopacoin balance to a specific amount."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET jopacoin_balance = ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
            """,
                (amount, discord_id, guild_id),
            )
            # Track lowest balance
            cursor.execute(
                """
                UPDATE players
                SET lowest_balance_ever = ?
                WHERE discord_id = ? AND guild_id = ?
                AND (lowest_balance_ever IS NULL OR ? < lowest_balance_ever)
                """,
                (amount, discord_id, guild_id, amount),
            )

    def add_balance(self, discord_id: int, guild_id: int, amount: int) -> None:
        """Add or subtract from a player's jopacoin balance."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
            """,
                (amount, discord_id, guild_id),
            )
            # Track lowest balance if this was a decrease
            if amount < 0:
                cursor.execute(
                    """
                    UPDATE players
                    SET lowest_balance_ever = jopacoin_balance
                    WHERE discord_id = ? AND guild_id = ?
                    AND (lowest_balance_ever IS NULL OR jopacoin_balance < lowest_balance_ever)
                    """,
                    (discord_id, guild_id),
                )

    def add_balance_many(self, deltas_by_discord_id: dict[int, int], guild_id: int) -> None:
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
                WHERE discord_id = ? AND guild_id = ?
                """,
                [(delta, discord_id, guild_id) for discord_id, delta in deltas_by_discord_id.items()],
            )
            # Track lowest balance for players who had negative deltas
            negative_ids = [did for did, delta in deltas_by_discord_id.items() if delta < 0]
            if negative_ids:
                placeholders = ",".join("?" * len(negative_ids))
                cursor.execute(
                    f"""
                    UPDATE players
                    SET lowest_balance_ever = jopacoin_balance
                    WHERE discord_id IN ({placeholders}) AND guild_id = ?
                    AND (lowest_balance_ever IS NULL OR jopacoin_balance < lowest_balance_ever)
                    """,
                    negative_ids + [guild_id],
                )

    def add_balance_with_garnishment(
        self, discord_id: int, guild_id: int, amount: int, garnishment_rate: float
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
                "SELECT COALESCE(jopacoin_balance, 0) as balance FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, guild_id),
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
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    (amount, discord_id, guild_id),
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
                WHERE discord_id = ? AND guild_id = ?
                """,
                (amount, discord_id, guild_id),
            )

            return {"gross": amount, "garnished": garnished, "net": net}

    def pay_debt_atomic(
        self, from_discord_id: int, to_discord_id: int, guild_id: int, amount: int
    ) -> dict[str, int]:
        """
        Atomically transfer jopacoin from one player to pay down another's debt.

        Args:
            from_discord_id: Player paying (must have positive balance)
            to_discord_id: Player receiving (can be same as from for self-payment)
            guild_id: Guild ID
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
                "SELECT COALESCE(jopacoin_balance, 0) as balance FROM players WHERE discord_id = ? AND guild_id = ?",
                (from_discord_id, guild_id),
            )
            from_row = cursor.fetchone()
            if not from_row:
                raise ValueError("Sender not found.")

            from_balance = int(from_row["balance"])
            if from_balance < amount:
                raise ValueError(f"Insufficient balance. You have {from_balance} jopacoin.")

            # Get recipient balance
            cursor.execute(
                "SELECT COALESCE(jopacoin_balance, 0) as balance FROM players WHERE discord_id = ? AND guild_id = ?",
                (to_discord_id, guild_id),
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
                WHERE discord_id = ? AND guild_id = ?
                """,
                (actual_amount, from_discord_id, guild_id),
            )

            # Add to recipient (reduces debt)
            cursor.execute(
                """
                UPDATE players
                SET jopacoin_balance = jopacoin_balance + ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (actual_amount, to_discord_id, guild_id),
            )

            return {
                "amount_paid": actual_amount,
                "from_new_balance": from_balance - actual_amount,
                "to_new_balance": to_balance + actual_amount,
            }

    def tip_atomic(
        self,
        from_discord_id: int,
        to_discord_id: int,
        guild_id: int,
        amount: int,
        fee: int,
    ) -> dict[str, int]:
        """
        Atomically transfer jopacoin from one player to another with a fee.

        The fee is sent to the nonprofit fund. Sender pays amount + fee,
        recipient receives only amount.

        Args:
            from_discord_id: Player sending the tip
            to_discord_id: Player receiving the tip
            guild_id: Guild ID
            amount: Amount to transfer to recipient
            fee: Fee for nonprofit fund (sender pays this on top of amount)

        Returns:
            Dict with 'amount', 'fee', 'from_new_balance', 'to_new_balance'

        Raises:
            ValueError if insufficient funds or player not found
        """
        total_cost = amount + fee

        if amount <= 0:
            raise ValueError("Amount must be positive.")
        if fee < 0:
            raise ValueError("Fee cannot be negative.")

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            # Get sender balance
            cursor.execute(
                "SELECT COALESCE(jopacoin_balance, 0) as balance FROM players WHERE discord_id = ? AND guild_id = ?",
                (from_discord_id, guild_id),
            )
            from_row = cursor.fetchone()
            if not from_row:
                raise ValueError("Sender not found.")

            from_balance = int(from_row["balance"])
            if from_balance < total_cost:
                raise ValueError(
                    f"Insufficient balance. You need {total_cost} (tip: {amount}, fee: {fee}). "
                    f"You have {from_balance}."
                )

            # Check recipient exists
            cursor.execute(
                "SELECT COALESCE(jopacoin_balance, 0) as balance FROM players WHERE discord_id = ? AND guild_id = ?",
                (to_discord_id, guild_id),
            )
            to_row = cursor.fetchone()
            if not to_row:
                raise ValueError("Recipient not found.")

            to_balance = int(to_row["balance"])

            # Deduct from sender (amount + fee)
            cursor.execute(
                """
                UPDATE players
                SET jopacoin_balance = jopacoin_balance - ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (total_cost, from_discord_id, guild_id),
            )

            # Add to recipient (only amount, fee is burned)
            cursor.execute(
                """
                UPDATE players
                SET jopacoin_balance = jopacoin_balance + ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (amount, to_discord_id, guild_id),
            )

            # Track lowest balance for sender
            new_from_balance = from_balance - total_cost
            cursor.execute(
                """
                UPDATE players
                SET lowest_balance_ever = ?
                WHERE discord_id = ? AND guild_id = ?
                AND (lowest_balance_ever IS NULL OR ? < lowest_balance_ever)
                """,
                (new_from_balance, from_discord_id, guild_id, new_from_balance),
            )

            return {
                "amount": amount,
                "fee": fee,
                "from_new_balance": new_from_balance,
                "to_new_balance": to_balance + amount,
            }

    def steal_atomic(
        self,
        thief_discord_id: int,
        victim_discord_id: int,
        guild_id: int,
        amount: int,
    ) -> dict[str, int]:
        """
        Atomically transfer jopacoin from victim to thief (shell mechanic).

        Unlike tips, this transfer:
        - Has no fee
        - Can push victim below MAX_DEBT (intentional - like BANKRUPT wedge)
        - Thief doesn't need sufficient balance

        Used for Red Shell and Blue Shell wheel outcomes.

        Args:
            thief_discord_id: Player receiving the stolen coins
            victim_discord_id: Player losing the coins
            guild_id: Guild ID
            amount: Amount to steal

        Returns:
            Dict with 'amount', 'thief_new_balance', 'victim_new_balance'

        Raises:
            ValueError if amount <= 0 or player not found
        """
        if amount <= 0:
            raise ValueError("Amount must be positive.")

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            # Get victim balance
            cursor.execute(
                "SELECT COALESCE(jopacoin_balance, 0) as balance FROM players WHERE discord_id = ? AND guild_id = ?",
                (victim_discord_id, guild_id),
            )
            victim_row = cursor.fetchone()
            if not victim_row:
                raise ValueError("Victim not found.")
            victim_balance = int(victim_row["balance"])

            # Get thief balance
            cursor.execute(
                "SELECT COALESCE(jopacoin_balance, 0) as balance FROM players WHERE discord_id = ? AND guild_id = ?",
                (thief_discord_id, guild_id),
            )
            thief_row = cursor.fetchone()
            if not thief_row:
                raise ValueError("Thief not found.")
            thief_balance = int(thief_row["balance"])

            # Deduct from victim (can go below MAX_DEBT - intentional)
            cursor.execute(
                """
                UPDATE players
                SET jopacoin_balance = jopacoin_balance - ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (amount, victim_discord_id, guild_id),
            )

            # Add to thief
            cursor.execute(
                """
                UPDATE players
                SET jopacoin_balance = jopacoin_balance + ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (amount, thief_discord_id, guild_id),
            )

            # Track lowest balance for victim
            new_victim_balance = victim_balance - amount
            cursor.execute(
                """
                UPDATE players
                SET lowest_balance_ever = ?
                WHERE discord_id = ? AND guild_id = ?
                AND (lowest_balance_ever IS NULL OR ? < lowest_balance_ever)
                """,
                (new_victim_balance, victim_discord_id, guild_id, new_victim_balance),
            )

            return {
                "amount": amount,
                "thief_new_balance": thief_balance + amount,
                "victim_new_balance": new_victim_balance,
            }

    def get_players_with_negative_balance(self, guild_id: int) -> list[dict]:
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
                WHERE guild_id = ? AND jopacoin_balance < 0
                ORDER BY jopacoin_balance ASC""",
                (guild_id,),
            )
            return [
                {
                    "discord_id": row["discord_id"],
                    "balance": row["jopacoin_balance"],
                    "username": row["discord_username"],
                }
                for row in cursor.fetchall()
            ]

    def get_stimulus_eligible_players(self, guild_id: int) -> list[dict]:
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
                WHERE guild_id = ? AND jopacoin_balance >= 0
                ORDER BY jopacoin_balance DESC
                LIMIT -1 OFFSET 3
                """,
                (guild_id,),
            )
            return [
                {"discord_id": row["discord_id"], "balance": row["jopacoin_balance"]}
                for row in cursor.fetchall()
            ]

    def get_all_registered_players_for_lottery(self, guild_id: int) -> list[dict]:
        """
        Get all registered players (discord_id only) for lottery selection.

        Returns:
            List of dicts with 'discord_id' for all registered players.
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT discord_id FROM players WHERE guild_id = ?", (guild_id,))
            return [{"discord_id": row["discord_id"]} for row in cursor.fetchall()]

    def get_players_by_games_played(self, guild_id: int) -> list[dict]:
        """
        Get players sorted by total games played (wins + losses).

        Only includes players with at least 1 game played.
        Used for social security distribution.

        Returns:
            List of dicts with 'discord_id' and 'games_played', sorted by games DESC.
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT discord_id, COALESCE(wins, 0) + COALESCE(losses, 0) as games_played
                FROM players
                WHERE guild_id = ? AND COALESCE(wins, 0) + COALESCE(losses, 0) > 0
                ORDER BY games_played DESC
                """,
                (guild_id,),
            )
            return [
                {"discord_id": row["discord_id"], "games_played": row["games_played"]}
                for row in cursor.fetchall()
            ]

    def apply_interest_bulk(self, updates: list[tuple[int, int]], guild_id: int) -> int:
        """
        Apply interest charges to multiple players in a single transaction.

        Interest is subtracted from balance (making debt larger/more negative).

        Args:
            updates: List of (discord_id, interest_amount) tuples
            guild_id: Guild ID

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
                WHERE discord_id = ? AND guild_id = ?
                """,
                [(interest, discord_id, guild_id) for discord_id, interest in updates],
            )
            return cursor.rowcount

    def increment_wins(self, discord_id: int, guild_id: int) -> None:
        """Increment player's win count."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players SET wins = wins + 1, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
            """,
                (discord_id, guild_id),
            )

    def increment_losses(self, discord_id: int, guild_id: int) -> None:
        """Increment player's loss count."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players SET losses = losses + 1, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
            """,
                (discord_id, guild_id),
            )

    def apply_match_outcome(self, winning_ids: list[int], losing_ids: list[int], guild_id: int) -> None:
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
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    [(pid, guild_id) for pid in winning_ids],
                )
            if losing_ids:
                cursor.executemany(
                    """
                    UPDATE players SET losses = losses + 1, updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    [(pid, guild_id) for pid in losing_ids],
                )

    def update_glicko_ratings_bulk(self, updates: list[tuple[int, float, float, float]], guild_id: int) -> int:
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
                WHERE discord_id = ? AND guild_id = ?
                """,
                [(rating, rd, vol, pid, guild_id) for pid, rating, rd, vol in updates],
            )
            return cursor.rowcount

    def get_exclusion_counts(self, discord_ids: list[int], guild_id: int) -> dict[int, int]:
        """Get exclusion counts for multiple players."""
        if not discord_ids:
            return {}

        with self.connection() as conn:
            cursor = conn.cursor()
            placeholders = ",".join("?" * len(discord_ids))
            cursor.execute(
                f"SELECT discord_id, COALESCE(exclusion_count, 0) as exclusion_count "
                f"FROM players WHERE discord_id IN ({placeholders}) AND guild_id = ?",
                discord_ids + [guild_id],
            )
            rows = cursor.fetchall()
            return {row["discord_id"]: row["exclusion_count"] for row in rows}

    def increment_exclusion_count(self, discord_id: int, guild_id: int) -> None:
        """Increment player's exclusion count by 4."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET exclusion_count = COALESCE(exclusion_count, 0) + 4,
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
            """,
                (discord_id, guild_id),
            )

    def increment_exclusion_count_half(self, discord_id: int, guild_id: int) -> None:
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
                WHERE discord_id = ? AND guild_id = ?
            """,
                (discord_id, guild_id),
            )

    def decay_exclusion_count(self, discord_id: int, guild_id: int) -> None:
        """Decay player's exclusion count by halving it."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET exclusion_count = COALESCE(exclusion_count, 0) / 2,
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
            """,
                (discord_id, guild_id),
            )

    def delete(self, discord_id: int, guild_id: int) -> bool:
        """
        Delete a player from the database.

        Returns:
            True if deleted, False if player didn't exist
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            cursor.execute(
                "SELECT discord_id FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, guild_id),
            )
            if not cursor.fetchone():
                return False

            cursor.execute(
                "DELETE FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, guild_id),
            )
            cursor.execute(
                "DELETE FROM match_participants WHERE discord_id = ? AND guild_id = ?",
                (discord_id, guild_id),
            )
            cursor.execute(
                "DELETE FROM rating_history WHERE discord_id = ? AND guild_id = ?",
                (discord_id, guild_id),
            )

            return True

    def delete_all(self, guild_id: int) -> int:
        """
        Delete all players in a guild (for testing).

        Returns:
            Number of players deleted
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            cursor.execute("SELECT COUNT(*) FROM players WHERE guild_id = ?", (guild_id,))
            count = cursor.fetchone()[0]

            cursor.execute("DELETE FROM players WHERE guild_id = ?", (guild_id,))
            cursor.execute("DELETE FROM match_participants WHERE guild_id = ?", (guild_id,))
            cursor.execute("DELETE FROM rating_history WHERE guild_id = ?", (guild_id,))

            return count

    def get_by_steam_id(self, steam_id: int, guild_id: int) -> Player | None:
        """
        Get player by Steam ID (32-bit account_id) within a guild.

        Checks the junction table first (for multi-steam-id support),
        then falls back to legacy players.steam_id column.

        Args:
            steam_id: The 32-bit Steam account ID
            guild_id: Guild ID to filter by

        Returns:
            Player object or None if not found
        """
        # First, try the junction table
        player = self.get_player_by_any_steam_id(steam_id, guild_id)
        if player:
            return player

        # Fallback to legacy column for backward compatibility
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT * FROM players WHERE steam_id = ? AND guild_id = ?",
                (steam_id, guild_id),
            )
            row = cursor.fetchone()

            if not row:
                return None

            return self._row_to_player(row)

    def get_steam_id(self, discord_id: int) -> int | None:
        """
        Get a player's primary Steam ID.

        Checks the junction table first, then falls back to legacy column.

        Returns:
            Steam ID (32-bit) or None if not set
        """
        # First, try the junction table
        primary = self.get_primary_steam_id(discord_id)
        if primary is not None:
            return primary

        # Fallback to legacy column for backward compatibility
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT steam_id FROM players WHERE discord_id = ?", (discord_id,))
            row = cursor.fetchone()
            return row["steam_id"] if row and row["steam_id"] else None

    def get_steam_ids_bulk(self, discord_ids: list[int]) -> dict[int, list[int]]:
        """
        Get all steam_ids for multiple players in one query.

        Args:
            discord_ids: List of Discord user IDs

        Returns:
            Dict mapping discord_id to list of steam_ids (primary first)
        """
        if not discord_ids:
            return {}

        result: dict[int, list[int]] = {did: [] for did in discord_ids}

        with self.connection() as conn:
            cursor = conn.cursor()
            placeholders = ",".join("?" * len(discord_ids))

            # First, check the junction table (primary first via ORDER BY)
            cursor.execute(
                f"""
                SELECT discord_id, steam_id
                FROM player_steam_ids
                WHERE discord_id IN ({placeholders})
                ORDER BY discord_id, is_primary DESC, added_at ASC
                """,
                discord_ids,
            )

            junction_found = set()
            for row in cursor.fetchall():
                did = row["discord_id"]
                sid = row["steam_id"]
                result[did].append(sid)
                junction_found.add(did)

            # Fallback to legacy column for players not in junction table
            missing = [did for did in discord_ids if did not in junction_found]
            if missing:
                placeholders = ",".join("?" * len(missing))
                cursor.execute(
                    f"SELECT discord_id, steam_id FROM players WHERE discord_id IN ({placeholders})",
                    missing,
                )
                for row in cursor.fetchall():
                    did = row["discord_id"]
                    sid = row["steam_id"]
                    if sid and not result[did]:
                        result[did].append(sid)

        return result

    def set_steam_id(self, discord_id: int, steam_id: int) -> None:
        """
        Set a player's primary Steam ID.

        Also updates the legacy players.steam_id column and adds to junction table.

        Args:
            discord_id: The player's Discord ID
            steam_id: The 32-bit Steam account ID
        """
        import time

        with self.connection() as conn:
            cursor = conn.cursor()

            # Update legacy column for backward compatibility
            cursor.execute(
                """
                UPDATE players
                SET steam_id = ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
                """,
                (steam_id, discord_id),
            )

            # Clear existing primary flag for this player
            cursor.execute(
                "UPDATE player_steam_ids SET is_primary = 0 WHERE discord_id = ?",
                (discord_id,),
            )

            # Add to junction table or update if already exists
            cursor.execute(
                """
                INSERT INTO player_steam_ids (discord_id, steam_id, is_primary, added_at)
                VALUES (?, ?, 1, ?)
                ON CONFLICT(discord_id, steam_id) DO UPDATE SET is_primary = 1
                """,
                (discord_id, steam_id, int(time.time())),
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

    def delete_fake_users(self, guild_id: int) -> int:
        """
        Delete all fake users (discord_id < 0) and their related data in a guild.

        Returns:
            Number of fake users deleted.
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            cursor.execute(
                "SELECT COUNT(*) FROM players WHERE discord_id < 0 AND guild_id = ?",
                (guild_id,),
            )
            count = cursor.fetchone()[0]
            if count == 0:
                return 0

            # Remove related records first to avoid orphan rows if FK cascades aren't enforced
            cursor.execute(
                "DELETE FROM match_participants WHERE discord_id < 0 AND guild_id = ?",
                (guild_id,),
            )
            cursor.execute(
                "DELETE FROM rating_history WHERE discord_id < 0 AND guild_id = ?",
                (guild_id,),
            )
            cursor.execute(
                "DELETE FROM bets WHERE discord_id < 0 AND guild_id = ?",
                (guild_id,),
            )
            cursor.execute(
                "DELETE FROM players WHERE discord_id < 0 AND guild_id = ?",
                (guild_id,),
            )

            return count

    def get_lowest_balance(self, discord_id: int, guild_id: int) -> int | None:
        """Get a player's lowest balance ever recorded."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT lowest_balance_ever FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, guild_id),
            )
            row = cursor.fetchone()
            return row["lowest_balance_ever"] if row and row["lowest_balance_ever"] is not None else None

    def update_lowest_balance_if_lower(self, discord_id: int, guild_id: int, new_balance: int) -> bool:
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
                WHERE discord_id = ? AND guild_id = ?
                AND (lowest_balance_ever IS NULL OR lowest_balance_ever > ?)
                """,
                (new_balance, discord_id, guild_id, new_balance),
            )
            return cursor.rowcount > 0

    def get_first_calibrated_at(self, discord_id: int, guild_id: int) -> int | None:
        """
        Get the Unix timestamp when the player first became calibrated.

        Returns:
            Unix timestamp or None if never calibrated
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT first_calibrated_at FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, guild_id),
            )
            row = cursor.fetchone()
            return row["first_calibrated_at"] if row and row["first_calibrated_at"] else None

    def set_first_calibrated_at(self, discord_id: int, guild_id: int, timestamp: int) -> None:
        """
        Set the first calibration timestamp for a player.

        Only sets if not already set (first calibration is permanent).

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID
            timestamp: Unix timestamp when player first became calibrated
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET first_calibrated_at = ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ? AND first_calibrated_at IS NULL
                """,
                (timestamp, discord_id, guild_id),
            )

    def get_registered_player_count(self, guild_id: int) -> int:
        """
        Get total count of registered players in a guild.

        Used for quorum calculation in disbursement voting.
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT COUNT(*) as count FROM players WHERE guild_id = ?", (guild_id,))
            row = cursor.fetchone()
            return row["count"] if row else 0

    # --- Captain eligibility (Immortal Draft) ---

    def set_captain_eligible(self, discord_id: int, guild_id: int, eligible: bool) -> bool:
        """
        Set captain eligibility for a player.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID
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
                WHERE discord_id = ? AND guild_id = ?
                """,
                (1 if eligible else 0, discord_id, guild_id),
            )
            return cursor.rowcount > 0

    def get_captain_eligible(self, discord_id: int, guild_id: int) -> bool:
        """
        Check if a player is captain-eligible.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID

        Returns:
            True if player is captain-eligible, False otherwise (including if not found)
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT is_captain_eligible FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, guild_id),
            )
            row = cursor.fetchone()
            if not row:
                return False
            return bool(row["is_captain_eligible"])

    def get_captain_eligible_players(self, discord_ids: list[int], guild_id: int) -> list[int]:
        """
        Get list of captain-eligible player IDs from a given set of IDs.

        Args:
            discord_ids: List of Discord IDs to filter
            guild_id: Guild ID

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
                AND guild_id = ?
                AND is_captain_eligible = 1
                """,
                discord_ids + [guild_id],
            )
            return [row["discord_id"] for row in cursor.fetchall()]

    # --- OpenSkill Plackett-Luce rating methods ---

    def get_openskill_rating(self, discord_id: int, guild_id: int) -> tuple[float, float] | None:
        """
        Get player's OpenSkill rating (mu, sigma).

        Returns:
            Tuple of (mu, sigma) or None if not found or not set
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT os_mu, os_sigma FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, guild_id),
            )
            row = cursor.fetchone()
            if row and row["os_mu"] is not None:
                return (row["os_mu"], row["os_sigma"])
            return None

    def update_openskill_rating(self, discord_id: int, guild_id: int, mu: float, sigma: float) -> None:
        """Update player's OpenSkill rating."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET os_mu = ?, os_sigma = ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (mu, sigma, discord_id, guild_id),
            )

    def update_openskill_ratings_bulk(
        self, updates: list[tuple[int, float, float]], guild_id: int
    ) -> int:
        """
        Bulk update OpenSkill ratings in a single transaction.

        Args:
            updates: List of (discord_id, mu, sigma) tuples
            guild_id: Guild ID

        Returns:
            Number of rows updated
        """
        if not updates:
            return 0
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.executemany(
                """
                UPDATE players
                SET os_mu = ?, os_sigma = ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                [(mu, sigma, pid, guild_id) for pid, mu, sigma in updates],
            )
            return cursor.rowcount

    def get_openskill_ratings_bulk(
        self, discord_ids: list[int], guild_id: int
    ) -> dict[int, tuple[float | None, float | None]]:
        """
        Get OpenSkill ratings for multiple players.

        Args:
            discord_ids: List of Discord user IDs
            guild_id: Guild ID

        Returns:
            Dict mapping discord_id to (mu, sigma) tuple (values may be None)
        """
        if not discord_ids:
            return {}
        with self.connection() as conn:
            cursor = conn.cursor()
            placeholders = ",".join("?" * len(discord_ids))
            cursor.execute(
                f"""
                SELECT discord_id, os_mu, os_sigma
                FROM players
                WHERE discord_id IN ({placeholders}) AND guild_id = ?
                """,
                discord_ids + [guild_id],
            )
            return {
                row["discord_id"]: (row["os_mu"], row["os_sigma"])
                for row in cursor.fetchall()
            }

    def get_last_wheel_spin(self, discord_id: int, guild_id: int) -> int | None:
        """
        Get the timestamp of a player's last wheel spin.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID

        Returns:
            Unix timestamp of last spin, or None if never spun
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT last_wheel_spin FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, guild_id),
            )
            row = cursor.fetchone()
            if not row or row["last_wheel_spin"] is None:
                return None
            return int(row["last_wheel_spin"])

    def set_last_wheel_spin(self, discord_id: int, guild_id: int, timestamp: int) -> None:
        """
        Set the timestamp of a player's last wheel spin.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID
            timestamp: Unix timestamp of the spin
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET last_wheel_spin = ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (timestamp, discord_id, guild_id),
            )

    def try_claim_wheel_spin(self, discord_id: int, guild_id: int, now: int, cooldown_seconds: int) -> bool:
        """
        Atomically check cooldown and claim a wheel spin.

        This prevents race conditions where concurrent requests could both pass
        the cooldown check before either sets the new timestamp.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID
            now: Current Unix timestamp
            cooldown_seconds: Required cooldown between spins

        Returns:
            True if spin was claimed (cooldown passed), False if still on cooldown
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            # Atomic check-and-set: only update if cooldown has passed
            cursor.execute(
                """
                UPDATE players
                SET last_wheel_spin = ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                  AND (last_wheel_spin IS NULL OR last_wheel_spin < ?)
                """,
                (now, discord_id, guild_id, now - cooldown_seconds),
            )
            # If rowcount > 0, the update happened (cooldown passed)
            return cursor.rowcount > 0

    def log_wheel_spin(
        self, discord_id: int, guild_id: int | None, result: int, spin_time: int
    ) -> int:
        """
        Log a wheel spin result for gambling history tracking.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID (None for DMs)
            result: Spin result (positive for win, negative for bankrupt, 0 for lose turn)
            spin_time: Unix timestamp of the spin

        Returns:
            The spin_id of the created record
        """
        normalized_guild_id = guild_id if guild_id is not None else 0
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO wheel_spins (guild_id, discord_id, result, spin_time)
                VALUES (?, ?, ?, ?)
                """,
                (normalized_guild_id, discord_id, result, spin_time),
            )
            return cursor.lastrowid

    def get_wheel_spin_history(self, discord_id: int, guild_id: int | None = None) -> list[dict]:
        """
        Get wheel spin history for a player.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID (optional, filters to specific guild if provided)

        Returns:
            List of dicts with 'result' and 'spin_time' keys, sorted by spin_time
        """
        normalized_guild_id = guild_id if guild_id is not None else 0
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT result, spin_time
                FROM wheel_spins
                WHERE discord_id = ? AND guild_id = ?
                ORDER BY spin_time ASC
                """,
                (discord_id, normalized_guild_id),
            )
            return [
                {"result": row["result"], "spin_time": row["spin_time"]}
                for row in cursor.fetchall()
            ]

    # --- Multi-Steam ID methods ---

    def get_steam_ids(self, discord_id: int) -> list[int]:
        """
        Get all Steam IDs for a player (primary first).

        Args:
            discord_id: Player's Discord ID

        Returns:
            List of steam_ids with primary first, or empty list if none
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            # Check junction table first
            cursor.execute(
                """
                SELECT steam_id FROM player_steam_ids
                WHERE discord_id = ?
                ORDER BY is_primary DESC, added_at ASC
                """,
                (discord_id,),
            )
            rows = cursor.fetchall()

            if rows:
                return [row["steam_id"] for row in rows]

            # Fallback to legacy column
            cursor.execute(
                "SELECT steam_id FROM players WHERE discord_id = ?",
                (discord_id,),
            )
            row = cursor.fetchone()
            if row and row["steam_id"]:
                return [row["steam_id"]]

            return []

    def add_steam_id(self, discord_id: int, steam_id: int, is_primary: bool = False) -> None:
        """
        Add a Steam ID to a player.

        Args:
            discord_id: Player's Discord ID
            steam_id: The 32-bit Steam account ID to add
            is_primary: Whether to set as primary (default False)

        Raises:
            ValueError: If steam_id is already linked to another player
        """
        import time

        with self.connection() as conn:
            cursor = conn.cursor()

            # Check if steam_id is already linked to another player
            cursor.execute(
                "SELECT discord_id FROM player_steam_ids WHERE steam_id = ?",
                (steam_id,),
            )
            existing = cursor.fetchone()
            if existing and existing["discord_id"] != discord_id:
                raise ValueError(f"Steam ID {steam_id} is already linked to another player")

            # Also check legacy column
            cursor.execute(
                "SELECT discord_id FROM players WHERE steam_id = ? AND discord_id != ?",
                (steam_id, discord_id),
            )
            if cursor.fetchone():
                raise ValueError(f"Steam ID {steam_id} is already linked to another player")

            # If setting as primary, clear existing primary flag
            if is_primary:
                cursor.execute(
                    "UPDATE player_steam_ids SET is_primary = 0 WHERE discord_id = ?",
                    (discord_id,),
                )

            # Add or update the steam_id
            cursor.execute(
                """
                INSERT INTO player_steam_ids (discord_id, steam_id, is_primary, added_at)
                VALUES (?, ?, ?, ?)
                ON CONFLICT(discord_id, steam_id) DO UPDATE SET is_primary = excluded.is_primary
                """,
                (discord_id, steam_id, 1 if is_primary else 0, int(time.time())),
            )

            # If primary, also update legacy column
            if is_primary:
                cursor.execute(
                    "UPDATE players SET steam_id = ?, updated_at = CURRENT_TIMESTAMP WHERE discord_id = ?",
                    (steam_id, discord_id),
                )

    def remove_steam_id(self, discord_id: int, steam_id: int) -> bool:
        """
        Remove a Steam ID from a player.

        Args:
            discord_id: Player's Discord ID
            steam_id: The steam_id to remove

        Returns:
            True if removed, False if not found
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            # Check if this was the primary
            cursor.execute(
                "SELECT is_primary FROM player_steam_ids WHERE discord_id = ? AND steam_id = ?",
                (discord_id, steam_id),
            )
            row = cursor.fetchone()
            was_primary = row and row["is_primary"]

            # Remove from junction table
            cursor.execute(
                "DELETE FROM player_steam_ids WHERE discord_id = ? AND steam_id = ?",
                (discord_id, steam_id),
            )

            if cursor.rowcount == 0:
                return False

            # If it was primary, promote another steam_id or clear legacy column
            if was_primary:
                cursor.execute(
                    """
                    SELECT steam_id FROM player_steam_ids
                    WHERE discord_id = ?
                    ORDER BY added_at ASC
                    LIMIT 1
                    """,
                    (discord_id,),
                )
                next_row = cursor.fetchone()
                if next_row:
                    # Promote the oldest remaining steam_id to primary
                    new_primary = next_row["steam_id"]
                    cursor.execute(
                        "UPDATE player_steam_ids SET is_primary = 1 WHERE discord_id = ? AND steam_id = ?",
                        (discord_id, new_primary),
                    )
                    cursor.execute(
                        "UPDATE players SET steam_id = ?, updated_at = CURRENT_TIMESTAMP WHERE discord_id = ?",
                        (new_primary, discord_id),
                    )
                else:
                    # No more steam_ids, clear legacy column
                    cursor.execute(
                        "UPDATE players SET steam_id = NULL, updated_at = CURRENT_TIMESTAMP WHERE discord_id = ?",
                        (discord_id,),
                    )

            return True

    def set_primary_steam_id(self, discord_id: int, steam_id: int) -> bool:
        """
        Set a Steam ID as the primary for a player.

        Args:
            discord_id: Player's Discord ID
            steam_id: The steam_id to set as primary (must already be linked)

        Returns:
            True if successful, False if steam_id not linked to this player
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            # Check if steam_id belongs to this player
            cursor.execute(
                "SELECT 1 FROM player_steam_ids WHERE discord_id = ? AND steam_id = ?",
                (discord_id, steam_id),
            )
            if not cursor.fetchone():
                return False

            # Clear existing primary
            cursor.execute(
                "UPDATE player_steam_ids SET is_primary = 0 WHERE discord_id = ?",
                (discord_id,),
            )

            # Set new primary
            cursor.execute(
                "UPDATE player_steam_ids SET is_primary = 1 WHERE discord_id = ? AND steam_id = ?",
                (discord_id, steam_id),
            )

            # Update legacy column
            cursor.execute(
                "UPDATE players SET steam_id = ?, updated_at = CURRENT_TIMESTAMP WHERE discord_id = ?",
                (steam_id, discord_id),
            )

            return True

    def get_primary_steam_id(self, discord_id: int) -> int | None:
        """
        Get the primary Steam ID for a player from the junction table.

        Args:
            discord_id: Player's Discord ID

        Returns:
            Primary steam_id or None if not set
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT steam_id FROM player_steam_ids
                WHERE discord_id = ? AND is_primary = 1
                """,
                (discord_id,),
            )
            row = cursor.fetchone()
            return row["steam_id"] if row else None

    def get_player_by_any_steam_id(self, steam_id: int, guild_id: int) -> Player | None:
        """
        Get player by any of their Steam IDs (from junction table) within a guild.

        Args:
            steam_id: The 32-bit Steam account ID
            guild_id: Guild ID to filter by

        Returns:
            Player object or None if not found
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT p.* FROM players p
                JOIN player_steam_ids psi ON p.discord_id = psi.discord_id
                WHERE psi.steam_id = ? AND p.guild_id = ?
                """,
                (steam_id, guild_id),
            )
            row = cursor.fetchone()

            if not row:
                return None

            return self._row_to_player(row)

    # --- Double or Nothing methods ---

    def get_last_double_or_nothing(self, discord_id: int, guild_id: int) -> int | None:
        """
        Get the timestamp of a player's last Double or Nothing spin.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID

        Returns:
            Unix timestamp of last spin, or None if never played
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT last_double_or_nothing FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, guild_id),
            )
            row = cursor.fetchone()
            if not row or row["last_double_or_nothing"] is None:
                return None
            return int(row["last_double_or_nothing"])

    def log_double_or_nothing(
        self,
        discord_id: int,
        guild_id: int,
        cost: int,
        balance_before: int,
        balance_after: int,
        won: bool,
        spin_time: int,
    ) -> None:
        """
        Log a Double or Nothing spin and update cooldown.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID
            cost: Cost paid to play
            balance_before: Balance before the gamble (after cost deducted)
            balance_after: Balance after the gamble
            won: Whether the player won
            spin_time: Unix timestamp of the spin
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            # Log the spin
            cursor.execute(
                """
                INSERT INTO double_or_nothing_spins
                (guild_id, discord_id, cost, balance_before, balance_after, won, spin_time)
                VALUES (?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    guild_id,
                    discord_id,
                    cost,
                    balance_before,
                    balance_after,
                    1 if won else 0,
                    spin_time,
                ),
            )
            # Update cooldown
            cursor.execute(
                """
                UPDATE players SET last_double_or_nothing = ? WHERE discord_id = ? AND guild_id = ?
                """,
                (spin_time, discord_id, guild_id),
            )

    def get_double_or_nothing_history(self, discord_id: int, guild_id: int) -> list[dict]:
        """
        Get Double or Nothing history for a player.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID

        Returns:
            List of dicts with spin details, sorted by spin_time
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT cost, balance_before, balance_after, won, spin_time
                FROM double_or_nothing_spins
                WHERE discord_id = ? AND guild_id = ?
                ORDER BY spin_time ASC
                """,
                (discord_id, guild_id),
            )
            return [
                {
                    "cost": row["cost"],
                    "balance_before": row["balance_before"],
                    "balance_after": row["balance_after"],
                    "won": bool(row["won"]),
                    "spin_time": row["spin_time"],
                }
                for row in cursor.fetchall()
            ]

    def get_player_above(self, discord_id: int, guild_id: int) -> Player | None:
        """
        Get the player ranked one position higher on the balance leaderboard.

        Used for Red Shell wheel mechanic - steals from the player ahead of you.

        Args:
            discord_id: The player's Discord ID
            guild_id: Guild ID

        Returns:
            Player object of the player ranked above, or None if user is #1 or not found
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            # First, get the user's balance
            cursor.execute(
                "SELECT COALESCE(jopacoin_balance, 0) as balance FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, guild_id),
            )
            user_row = cursor.fetchone()
            if not user_row:
                return None

            user_balance = int(user_row["balance"])

            # Find player with the smallest balance that is greater than user's balance
            # If there's a tie at user's balance, get the one with lower discord_id (tiebreaker)
            cursor.execute(
                """
                SELECT * FROM players
                WHERE guild_id = ? AND (
                    COALESCE(jopacoin_balance, 0) > ?
                    OR (COALESCE(jopacoin_balance, 0) = ? AND discord_id < ?)
                )
                ORDER BY COALESCE(jopacoin_balance, 0) ASC,
                         discord_id DESC
                LIMIT 1
                """,
                (guild_id, user_balance, user_balance, discord_id),
            )
            row = cursor.fetchone()

            if not row:
                return None

            return self._row_to_player(row)

    def _row_to_player(self, row) -> Player:
        """Convert database row to Player object."""
        preferred_roles = json.loads(row["preferred_roles"]) if row["preferred_roles"] else None

        # Handle os_mu and os_sigma which may not exist in older schemas
        os_mu = row["os_mu"] if "os_mu" in row.keys() else None
        os_sigma = row["os_sigma"] if "os_sigma" in row.keys() else None
        steam_id = row["steam_id"] if "steam_id" in row.keys() else None
        guild_id = row["guild_id"] if "guild_id" in row.keys() else None

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
            os_mu=os_mu,
            os_sigma=os_sigma,
            discord_id=row["discord_id"],
            guild_id=guild_id,
            jopacoin_balance=row["jopacoin_balance"] if row["jopacoin_balance"] else 0,
            steam_id=steam_id,
        )
